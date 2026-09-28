//! Readers over `std::io::Read + Seek` (the `*_cb` functions of minimp3_ex).

use std::io::{self, Read, Seek, SeekFrom};
use std::vec;
use std::vec::Vec;

use core::ops::ControlFlow;

use crate::frames::{raw_info, RawFrame};
use crate::load::{load_body, Decoded};
use crate::reader::{Input, Options, Raw, State};
use crate::tags::{id3v2_len, strip_trailing_tags_impl, ID3_DETECT_SIZE};
use crate::vbr::check_vbrtag;
use crate::{Channels, Error, Frame, Sample};
use nanomp3_core::__private::{self as core_private, find_frame, Header};

/// minimp3's `MINIMP3_IO_SIZE`: the read buffer for I/O readers.
pub(crate) const IO_SIZE: usize = 128 * 1024;
/// minimp3's `MINIMP3_BUF_SIZE`: enough for 10 consecutive frames in the worst
/// case; the buffer is refilled when less than this remains.
pub(crate) const BUF_SIZE: usize = 16 * 1024;

/// `fread` semantics: read until `buf` is full or the input ends.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn short_read() -> Error {
    Error::Io(io::ErrorKind::UnexpectedEof.into())
}

/// Trailing-tag stripping at the end of the input. minimp3 strips again every
/// time it reaches the end (and once more for inputs under `BUF_SIZE`), which
/// can cut audio that happens to end like a tag; by default it's done once.
struct Strip {
    compat: bool,
    done: bool,
}

impl Strip {
    fn new(compat: bool) -> Self {
        Self { compat, done: false }
    }

    fn apply(&mut self, buf: &[u8], filled: &mut usize) {
        if self.compat || !self.done {
            *filled = strip_trailing_tags_impl(&buf[..*filled], self.compat).len();
            self.done = true;
        }
    }
}

/// Moves the unconsumed bytes to the front of `buf` and reads more after them
/// (the refill step all of minimp3_ex's `*_cb` loops share). Returns whether
/// the input ended. On error the bytes are moved but nothing is read.
fn refill(
    io: &mut impl Read,
    buf: &mut [u8],
    consumed: &mut usize,
    filled: &mut usize,
    strip: &mut Strip,
) -> io::Result<bool> {
    buf.copy_within(*consumed..*filled, 0);
    *filled -= *consumed;
    *consumed = 0;
    let readed = read_full(io, &mut buf[*filled..])?;
    let eof = readed != buf.len() - *filled;
    *filled += readed;
    if eof {
        strip.apply(buf, filled);
    }
    Ok(eof)
}

/// `mp3dec_iterate_cb`, reading from the current position of `io`. `buf` is
/// `2 * IO_SIZE` bytes: `IO_SIZE` of buffer plus slack for compat-mode
/// over-reads.
pub(crate) fn iterate_cb<R: Read + Seek>(
    io: &mut R,
    padded: &mut [u8],
    compat: bool,
    f: &mut dyn FnMut(&RawFrame) -> ControlFlow<()>,
) -> Result<(), Error> {
    let mut filled = read_full(io, &mut padded[..ID3_DETECT_SIZE])?;
    let mut consumed = 0;
    let mut readed: u64 = 0;
    let mut eof = false;
    let mut strip = Strip::new(compat);
    if filled != ID3_DETECT_SIZE {
        return Ok(());
    }
    let id3v2 = id3v2_len(&padded[..ID3_DETECT_SIZE]);
    if id3v2 != 0 {
        io.seek(SeekFrom::Start(id3v2 as u64))?;
        filled = read_full(io, &mut padded[..IO_SIZE])?;
        readed += id3v2 as u64;
    } else {
        filled += read_full(io, &mut padded[ID3_DETECT_SIZE..IO_SIZE])?;
    }
    if filled < BUF_SIZE {
        strip.apply(padded, &mut filled);
    }
    loop {
        let mut free_format_bytes = 0;
        let (i, frame_size) = find_frame(&padded[consumed..filled], &mut free_format_bytes);
        if i != 0 && frame_size == 0 {
            if compat {
                // minimp3 stops here even if more input follows: a stretch of
                // junk longer than the buffer ends the stream early.
                consumed += i;
                continue;
            }
            // Keep the last few bytes, which could start a header, and read on.
            let n = if eof { i } else { i.saturating_sub(4) };
            consumed += n;
            readed += n as u64;
            if !eof {
                eof = refill(io, &mut padded[..IO_SIZE], &mut consumed, &mut filled, &mut strip)?;
            }
            continue;
        }
        if frame_size == 0 {
            break;
        }
        let hdr = consumed + i;
        readed += i as u64;
        // minimp3 reports the bytes available from `consumed` rather than from
        // the frame, overstating them by `i` (and letting the decoder read
        // stale or out-of-bounds bytes).
        let avail = if compat { filled - consumed } else { filled - hdr };
        let frame = RawFrame {
            data: &padded[hdr..hdr + avail],
            buf_size: avail,
            frame_size,
            free_format_bytes,
            offset: readed,
            info: raw_info(Header::read(&padded[hdr..]), frame_size),
        };
        if f(&frame).is_break() {
            return Ok(());
        }
        readed += frame_size as u64;
        consumed += i + frame_size;
        if !eof && filled - consumed < BUF_SIZE {
            eof = refill(io, &mut padded[..IO_SIZE], &mut consumed, &mut filled, &mut strip)?;
        }
    }
    Ok(())
}

/// Frames from a `Read + Seek` (`mp3dec_ex_open_cb`).
#[doc(hidden)]
pub struct IoInput<R> {
    io: R,
    buf: Vec<u8>,
    consumed: usize,
    filled: usize,
    /// End of input seen. Per read call in minimp3 (so it keeps re-reading
    /// and re-stripping tags at the end); sticky until the next seek otherwise.
    eof: bool,
    strip: Strip,
}

impl<R: Read + Seek> Input for IoInput<R> {
    fn iterate(
        &mut self,
        start: u64,
        compat: bool,
        f: &mut dyn FnMut(&RawFrame) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        self.io.seek(SeekFrom::Start(start))?;
        iterate_cb(&mut self.io, &mut self.buf, compat, f)
    }

    fn frame_at(&mut self, offset: u64, free_format_bytes: usize) -> Result<(&[u8], usize), Error> {
        self.io.seek(SeekFrom::Start(offset))?;
        if read_full(&mut self.io, &mut self.buf[..4])? != 4 {
            return Err(short_read());
        }
        let hdr = Header::read(&self.buf);
        let frame_size = hdr.frame_bytes(free_format_bytes) + hdr.padding();
        let want = frame_size.clamp(4, IO_SIZE) - 4;
        let got = read_full(&mut self.io, &mut self.buf[4..4 + want])?;
        if got != want || frame_size < 4 {
            // A frame cut short by the end of the file (or with a bogus
            // free-format size) fails the whole seek in minimp3's I/O mode,
            // while its memory mode just uses the bytes there are. In C, a
            // size under 4 even makes it read SIZE_MAX bytes into its buffer.
            if self.strip.compat {
                return Err(short_read());
            }
        }
        Ok((&self.buf[..4 + got], frame_size))
    }

    fn header_at(&mut self, offset: u64) -> Result<Header, Error> {
        self.io.seek(SeekFrom::Start(offset))?;
        if read_full(&mut self.io, &mut self.buf[..4])? != 4 {
            return Err(short_read());
        }
        Ok(Header::read(&self.buf))
    }

    fn reposition(&mut self, offset: u64) -> Result<(), Error> {
        self.io.seek(SeekFrom::Start(offset))?;
        self.consumed = 0;
        self.filled = 0;
        self.eof = false;
        self.strip.done = false;
        Ok(())
    }

    fn byte_seek_target(&self, position: u64) -> u64 {
        position
    }

    fn begin_read(&mut self) {
        if self.strip.compat {
            self.eof = false;
        }
    }

    fn window(&mut self, st: &mut State<impl Sample>) -> &[u8] {
        if !self.eof && self.filled - self.consumed < BUF_SIZE {
            let buf = &mut self.buf[..IO_SIZE];
            match refill(&mut self.io, buf, &mut self.consumed, &mut self.filled, &mut self.strip) {
                Ok(eof) => self.eof = eof,
                Err(e) => {
                    // C records the error and carries on with what it has.
                    st.fail(Error::Io(e));
                    self.eof = true;
                    self.strip.apply(buf, &mut self.filled);
                }
            }
        }
        &self.buf[self.consumed..self.filled]
    }

    fn consume(&mut self, n: usize) {
        self.consumed += n;
    }
}

/// A seekable reader over MP3 data from any `Read + Seek` source, such as a
/// [`File`](std::fs::File). See the crate documentation.
///
/// Positions and lengths count samples per channel (a stereo stream's sample 10
/// is interleaved samples 20 and 21), while [`read`](Self::read) fills a
/// buffer of interleaved samples.
pub struct Reader<R, S = f32> {
    st: State<S>,
    input: IoInput<R>,
}

impl<R: Read + Seek, S: Sample> Reader<R, S> {
    /// Opens `io`, scanning it for its length (see [`Options::skip_scan`]).
    pub fn new(io: R) -> Result<Self, Error> {
        Self::with_options(io, Options::default())
    }

    pub fn with_options(io: R, options: Options) -> Result<Self, Error> {
        let input = IoInput {
            io,
            buf: vec![0; 2 * IO_SIZE],
            consumed: 0,
            filled: 0,
            eof: false,
            strip: Strip::new(options.minimp3_compat),
        };
        let mut r = Self { st: State::new(options), input };
        r.st.open(&mut r.input)?;
        r.input.io.seek(SeekFrom::Start(r.st.start_offset))?;
        Ok(r)
    }

    /// Reads interleaved samples into `out`, returning how many were written.
    /// Returns `Ok(0)` at the end of the stream.
    ///
    /// After an error the reader stays at the end of the stream until the
    /// next seek.
    pub fn read(&mut self, out: &mut [S]) -> Result<usize, Error> {
        self.st.read(&mut self.input, out)
    }

    /// Decodes and returns the remaining interleaved samples of the current
    /// frame, or `None` at the end of the stream. This avoids a copy.
    pub fn read_frame(&mut self) -> Result<Option<&[S]>, Error> {
        self.st.read_frame_samples(&mut self.input)
    }

    /// Seeks to a sample (per channel), counted from the start of the audio
    /// after gapless trimming. Seeking past the end goes to the end.
    pub fn seek(&mut self, sample: u64) -> Result<(), Error> {
        let ch = u64::from(self.st.info.channels.max(1));
        self.st.seek(&mut self.input, sample.saturating_mul(ch), false)
    }

    /// Resumes decoding at a byte offset in the input; decoding resynchronizes
    /// at the next frame. The reader loses track of its sample position, so
    /// the end-of-stream padding cut (which counts samples from here) is only
    /// right after seeking to the very start.
    pub fn seek_to_byte(&mut self, offset: u64) -> Result<(), Error> {
        self.st.seek(&mut self.input, offset, true)
    }

    /// Length of the stream in samples per channel, after gapless trimming, if
    /// known.
    pub fn total_samples(&self) -> Option<u64> {
        let ch = u64::from(self.st.info.channels.max(1));
        self.st.total().map(|t| t / ch)
    }

    /// Channels of the first frame, or `None` if there is no audio.
    pub fn channels(&self) -> Option<Channels> {
        self.st.channels()
    }

    /// Sample rate of the stream in Hz (0 if there is no audio).
    pub fn sample_rate(&self) -> u32 {
        self.st.info.hz
    }

    /// Whether the stream had a VBR tag giving its length.
    pub fn has_vbr_tag(&self) -> bool {
        self.st.vbr_tag_found
    }

    /// Returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.input.io
    }

    #[doc(hidden)]
    pub fn __raw(&mut self) -> Raw<'_, S, IoInput<R>> {
        Raw::new(&mut self.st, &mut self.input)
    }
}

/// Like [`detect`](crate::detect), reading the start of `io`
/// (`mp3dec_detect_cb`).
pub fn detect_reader<R: Read + Seek>(mut io: R) -> io::Result<bool> {
    detect_reader_impl(&mut io, false)
}

pub(crate) fn detect_reader_impl<R: Read + Seek>(io: &mut R, compat: bool) -> io::Result<bool> {
    let mut buf = vec![0; BUF_SIZE];
    io.seek(SeekFrom::Start(0))?;
    let mut filled = read_full(io, &mut buf[..ID3_DETECT_SIZE])?;
    if filled < ID3_DETECT_SIZE {
        return Ok(false); // too small to be MPEG audio
    }
    if id3v2_len(&buf) != 0 {
        return Ok(true); // an ID3v2 tag is enough evidence
    }
    filled += read_full(io, &mut buf[ID3_DETECT_SIZE..])?;
    if filled < BUF_SIZE {
        filled = strip_trailing_tags_impl(&buf[..filled], compat).len();
    }
    Ok(find_frame(&buf[..filled], &mut 0).1 != 0)
}

/// Calls `f` for each frame read from `io`, starting at its current position,
/// like [`frames`](crate::frames) (`mp3dec_iterate_cb`). Frame offsets are
/// relative to the starting position.
pub fn for_each_frame_reader<R: Read + Seek>(
    mut io: R,
    mut f: impl FnMut(Frame<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    let mut buf = vec![0; 2 * IO_SIZE];
    let r = iterate_cb(&mut io, &mut buf, false, &mut |raw| {
        let hdr = Header::read(raw.data);
        f(Frame {
            offset: raw.offset as usize,
            data: &raw.data[..raw.frame_size],
            channels: if raw.info.channels == 1 { Channels::Mono } else { Channels::Stereo },
            sample_rate: raw.info.hz,
            layer: raw.info.layer,
            bitrate_kbps: raw.info.bitrate_kbps,
            samples: hdr.frame_samples() as usize,
        })
    });
    match r {
        Ok(()) => Ok(()),
        Err(Error::Io(e)) => Err(e),
        Err(_) => unreachable!("iteration only fails on I/O"),
    }
}

/// Decodes all of `io` into memory, like [`decode_all`](crate::decode_all)
/// (`mp3dec_load_cb`).
pub fn decode_all_reader<S: Sample, R: Read + Seek>(io: R) -> io::Result<Decoded<S>> {
    decode_all_reader_with(io, Options::default())
}

/// [`decode_all_reader`] with options.
pub fn decode_all_reader_with<S: Sample, R: Read + Seek>(mut io: R, options: Options) -> io::Result<Decoded<S>> {
    let compat = options.minimp3_compat;
    let mut buf = vec![0u8; IO_SIZE];
    let mut out = Decoded::empty();
    io.seek(SeekFrom::Start(0))?;
    let mut filled = read_full(&mut io, &mut buf[..ID3_DETECT_SIZE])?;
    let mut consumed = 0;
    let mut eof = false;
    let mut strip = Strip::new(compat);
    if filled != ID3_DETECT_SIZE {
        return Ok(out);
    }
    let id3v2 = id3v2_len(&buf);
    if id3v2 != 0 {
        io.seek(SeekFrom::Start(id3v2 as u64))?;
        filled = read_full(&mut io, &mut buf)?;
    } else {
        filled += read_full(&mut io, &mut buf[ID3_DETECT_SIZE..])?;
    }
    if filled < BUF_SIZE {
        strip.apply(&buf, &mut filled);
    }

    // Find the first frame and check it for a VBR tag.
    let mut to_skip = 0;
    let mut detected = 0;
    let mut frame_info;
    loop {
        if !eof && filled - consumed < BUF_SIZE {
            eof = refill(&mut io, &mut buf, &mut consumed, &mut filled, &mut strip)?;
        }
        let (i, frame_size) = find_frame(&buf[consumed..filled], &mut 0);
        consumed += i;
        if i != 0 && frame_size == 0 {
            continue;
        }
        if frame_size == 0 {
            return Ok(out);
        }
        let hdr = &buf[consumed..];
        frame_info = raw_info(Header::read(hdr), frame_size);
        if frame_info.layer != 3 {
            break;
        }
        let tag = check_vbrtag(hdr, frame_size);
        if let Some(tag @ crate::VbrTag { frames: Some(frames), .. }) = tag {
            let samples = Header::read(hdr).frame_samples() as u64 * u64::from(frame_info.channels);
            let channels = i32::from(frame_info.channels);
            let (delay, padding) = tag.delay_padding();
            let padding = padding * channels;
            to_skip = delay * channels;
            detected = samples * u64::from(frames);
            if detected >= to_skip as u64 {
                detected -= to_skip as u64;
            }
            if padding > 0 && detected >= padding as u64 {
                detected -= padding as u64;
            }
            if detected == 0 {
                return Ok(out);
            }
        }
        if tag.is_some() {
            consumed += frame_size;
        }
        break;
    }

    let mut decoder = if compat { nanomp3_core::Decoder::new_minimp3_compat() } else { nanomp3_core::Decoder::new() };
    let mut io_err = None;
    load_body(&mut out, &mut decoder, &mut frame_info, to_skip, detected, options, |dec, pcm, info| {
        if !eof && filled - consumed < BUF_SIZE {
            match refill(&mut io, &mut buf, &mut consumed, &mut filled, &mut strip) {
                Ok(e) => eof = e,
                Err(e) => {
                    io_err = Some(e);
                    return None;
                }
            }
        }
        let samples = core_private::decode_frame(dec, &buf[consumed..filled], Some(pcm), info);
        consumed += info.frame_bytes;
        Some(samples)
    });
    match io_err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

