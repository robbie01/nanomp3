//! A sample-accurate, seekable reader (`mp3dec_ex_t` in minimp3_ex).
//!
//! The logic shared by the slice and I/O readers lives in [`State`]; the
//! parts that differ (how frames are iterated and bytes fetched) are behind
//! [`Input`]. Both mirror minimp3_ex closely; places where the default
//! behavior deliberately departs from it are marked `minimp3_compat`.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::ControlFlow;

use crate::frames::{RawFrame, RawFrames};
use crate::vbr::check_vbrtag;
use crate::{Channels, Error, Sample, MAX_SAMPLES_PER_FRAME};
use nanomp3_core::__private::{self as core_private, l3_side_info, FrameInfo as RawInfo, Header};
use nanomp3_core::Decoder;

/// Frames decoded and discarded after a seek to refill the decoder's state.
const PREDECODE_FRAMES: usize = 2;

/// Options for [`SliceReader`] and [`Reader`](crate::Reader).
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct Options {
    /// Don't scan the whole stream when opening it to find its length if it
    /// has no VBR tag (`MP3D_DO_NOT_SCAN`). The length is then unknown, and
    /// the first seek to a sample builds the frame index instead.
    pub skip_scan: bool,
    /// Keep decoding when the stream switches between mono and stereo
    /// (`MP3D_ALLOW_MONO_STEREO_TRANSITION`). The number of channels in the
    /// output then changes too. By default such a switch is an error.
    pub allow_mono_stereo_transition: bool,
    /// Reproduce minimp3 exactly, including behavior nanomp3 otherwise
    /// corrects. See the crate documentation.
    pub minimp3_compat: bool,
}

impl Options {
    pub fn skip_scan(mut self, yes: bool) -> Self {
        self.skip_scan = yes;
        self
    }
    pub fn allow_mono_stereo_transition(mut self, yes: bool) -> Self {
        self.allow_mono_stereo_transition = yes;
        self
    }
    pub fn minimp3_compat(mut self, yes: bool) -> Self {
        self.minimp3_compat = yes;
        self
    }
}

#[derive(Clone, Copy)]
struct IndexEntry {
    sample: u64,
    offset: u64,
}

/// The source-independent part of `mp3dec_ex_t`.
pub(crate) struct State<S> {
    dec: Decoder,
    index: Vec<IndexEntry>,
    /// C's `index.frames != NULL`.
    index_allocated: bool,
    pub(crate) offset: u64,
    pub(crate) samples: u64,
    pub(crate) detected_samples: u64,
    pub(crate) cur_sample: u64,
    pub(crate) start_offset: u64,
    pub(crate) end_offset: u64,
    pub(crate) info: RawInfo,
    buffer: Vec<S>,
    do_not_scan: bool,
    allow_transition: bool,
    pub(crate) compat: bool,
    pub(crate) vbr_tag_found: bool,
    indexes_built: bool,
    free_format_bytes: usize,
    buffer_samples: i32,
    buffer_consumed: i32,
    to_skip: i32,
    start_delay: i32,
    /// Sticky until the next seek, like C's `last_error`.
    pub(crate) failed: bool,
    /// The error to report from the next call, taken once.
    pending_error: Option<Error>,
}

/// Where frames come from.
pub(crate) trait Input {
    /// `mp3dec_iterate_*` starting at absolute byte `start`, with offsets
    /// relative to `start`.
    fn iterate(
        &mut self,
        start: u64,
        compat: bool,
        f: &mut dyn FnMut(&RawFrame) -> ControlFlow<()>,
    ) -> Result<(), Error>;
    /// The complete frame at `offset` and its size.
    fn frame_at(&mut self, offset: u64, free_format_bytes: usize) -> Result<(&[u8], usize), Error>;
    /// The header of the frame at `offset`.
    fn header_at(&mut self, offset: u64) -> Result<Header, Error>;
    /// Positions the input for reading at `offset` and drops buffered data.
    fn reposition(&mut self, offset: u64) -> Result<(), Error>;
    /// Where a byte seek to `position` lands.
    fn byte_seek_target(&self, position: u64) -> u64;
    /// Called at the start of each `read_frame`.
    fn begin_read(&mut self) {}
    /// The bytes to hand to the decoder next; empty at the end of the input.
    fn window(&mut self, st: &mut State<impl Sample>) -> &[u8];
    /// Marks `n` bytes of the window as consumed.
    fn consume(&mut self, n: usize);
}

impl<S: Sample> State<S> {
    pub(crate) fn new(options: Options) -> Self {
        let dec = if options.minimp3_compat { Decoder::new_minimp3_compat() } else { Decoder::new() };
        Self {
            dec,
            index: Vec::new(),
            index_allocated: false,
            offset: 0,
            samples: 0,
            detected_samples: 0,
            cur_sample: 0,
            start_offset: 0,
            end_offset: 0,
            info: RawInfo::default(),
            buffer: vec![S::from_synth(0.0, false); MAX_SAMPLES_PER_FRAME],
            do_not_scan: options.skip_scan,
            allow_transition: options.allow_mono_stereo_transition,
            compat: options.minimp3_compat,
            vbr_tag_found: false,
            indexes_built: false,
            free_format_bytes: 0,
            buffer_samples: 0,
            buffer_consumed: 0,
            to_skip: 0,
            start_delay: 0,
            failed: false,
            pending_error: None,
        }
    }

    /// `mp3dec_ex_open_*` after the input is set up.
    pub(crate) fn open(&mut self, input: &mut impl Input) -> Result<(), Error> {
        core_private::init(&mut self.dec);
        let compat = self.compat;
        input.iterate(0, compat, &mut |f| self.load_index(f))?;
        core_private::init(&mut self.dec);
        self.buffer_samples = 0;
        self.indexes_built = !(self.vbr_tag_found || self.do_not_scan);
        self.do_not_scan = false;
        Ok(())
    }

    /// `mp3dec_load_index`, the iterate callback that builds the seek index.
    fn load_index(&mut self, frame: &RawFrame) -> ControlFlow<()> {
        if !self.index_allocated && self.start_offset == 0 {
            // Detect a VBR tag and try to avoid a full scan.
            self.info = frame.info;
            self.start_offset = frame.offset;
            self.offset = frame.offset;
            self.end_offset = frame.offset + frame.buf_size as u64;
            self.free_format_bytes = frame.free_format_bytes; // should not change
            if self.info.layer == 3 {
                let tag = check_vbrtag(frame.data, frame.frame_size);
                if tag.is_some() {
                    self.start_offset = frame.offset + frame.frame_size as u64;
                    self.offset = self.start_offset;
                }
                match tag {
                    Some(tag @ crate::VbrTag { frames: Some(frames), .. }) => {
                        let channels = i32::from(frame.info.channels);
                        let (delay, padding) = tag.delay_padding();
                        let padding = padding * channels;
                        self.start_delay = delay * channels;
                        self.to_skip = self.start_delay;
                        self.samples = u64::from(Header::read(frame.data).frame_samples())
                            * u64::from(frame.info.channels)
                            * u64::from(frames);
                        if self.samples >= self.start_delay as u64 {
                            self.samples -= self.start_delay as u64;
                        }
                        if padding > 0 && self.samples >= padding as u64 {
                            self.samples -= padding as u64;
                        }
                        self.detected_samples = self.samples;
                        self.vbr_tag_found = true;
                        return ControlFlow::Break(());
                    }
                    Some(_) => return ControlFlow::Continue(()),
                    None => {}
                }
            }
        }
        if self.do_not_scan {
            return ControlFlow::Break(());
        }
        self.index_allocated = true;
        self.index.push(IndexEntry { offset: frame.offset, sample: self.samples });
        let mut info = frame.info;
        if self.buffer_samples == 0 && self.index.len() < 256 {
            // For some cut mp3s the bit reservoir isn't filled and decoding
            // can't start from the first frames: try up to 255 frames until
            // samples start coming out.
            let data = &frame.data[..frame.data.len().min(i32::MAX as usize)];
            self.buffer_samples =
                core_private::decode_frame(&mut self.dec, data, Some(&mut self.buffer[..]), &mut info) as i32;
            self.samples += self.buffer_samples as u64 * u64::from(info.channels);
        } else {
            self.samples += u64::from(Header::read(frame.data).frame_samples()) * u64::from(info.channels);
        }
        ControlFlow::Continue(())
    }

    /// `mp3dec_ex_seek`. `position` counts interleaved samples.
    pub(crate) fn seek(&mut self, input: &mut impl Input, position: u64, to_byte: bool) -> Result<(), Error> {
        let r = self.seek_impl(input, position, to_byte);
        if r.is_err() {
            // minimp3 returns from a failed seek with its buffer counters out
            // of step (a lazy index build overwrites one of them), and its
            // next read then returns memory past the end of its buffer.
            // Instead, stay at the end of the stream until a seek succeeds.
            self.failed = true;
            self.buffer_samples = 0;
            self.buffer_consumed = 0;
        }
        r
    }

    fn seek_impl(&mut self, input: &mut impl Input, position: u64, to_byte: bool) -> Result<(), Error> {
        if to_byte {
            self.offset = input.byte_seek_target(position);
            self.cur_sample = 0;
            return self.seek_exit(input);
        }
        let mut position = position;
        if self.samples != 0 && position > self.samples {
            position = self.samples;
        }
        self.cur_sample = position;
        position = position.saturating_add(self.start_delay as u64);
        if position == 0 {
            return self.seek_zero(input);
        }
        if !self.indexes_built {
            // No index yet (the VBR tag gave the length, or scanning was skipped).
            self.indexes_built = true;
            self.samples = 0;
            self.buffer_samples = 0;
            let (start, compat) = (self.start_offset, self.compat);
            input.iterate(start, compat, &mut |f| self.load_index(f))?;
            for e in &mut self.index {
                e.offset += self.start_offset;
            }
            self.samples = self.detected_samples;
        }
        if !self.index_allocated {
            return self.seek_zero(input); // no frames in file
        }
        let mut i = idx_binary_search(&self.index, position);
        if i != 0 {
            let mut to_fill_bytes: i32 = 511;
            i -= i.min(PREDECODE_FRAMES);
            if self.info.layer == 3 {
                // Make sure the bit reservoir is filled when we start decoding.
                while i != 0 && to_fill_bytes != 0 {
                    let (frame, frame_size) = input.frame_at(self.index[i - 1].offset, self.free_format_bytes)?;
                    i -= 1;
                    let Some((pos, limit)) = l3_side_info(frame, frame_size) else {
                        break; // frame not decodable, we can start from here
                    };
                    let frame_bytes = (limit - pos) / 8;
                    to_fill_bytes -= to_fill_bytes.min(frame_bytes);
                }
            }
        }
        self.offset = self.index[i].offset;
        self.to_skip = if position > self.index[i].sample {
            (position - self.index[i].sample).min(i32::MAX as u64) as i32
        } else {
            0
        };
        while i + 1 < self.index.len() && self.index[i].sample == 0 && self.index[i + 1].sample == 0 {
            // Skip frames that can't be decoded at the start.
            let hdr = input.header_at(self.index[i].offset)?;
            let frame_samples = hdr.frame_samples() as i32 * i32::from(self.info.channels);
            self.to_skip = self.to_skip.saturating_add(frame_samples);
            i += 1;
        }
        self.seek_exit(input)
    }

    fn seek_zero(&mut self, input: &mut impl Input) -> Result<(), Error> {
        self.offset = self.start_offset;
        self.to_skip = 0;
        self.seek_exit(input)
    }

    fn seek_exit(&mut self, input: &mut impl Input) -> Result<(), Error> {
        input.reposition(self.offset)?;
        self.buffer_samples = 0;
        self.buffer_consumed = 0;
        self.failed = false;
        self.pending_error = None;
        core_private::init(&mut self.dec);
        Ok(())
    }

    /// Records a sticky error, reported once.
    pub(crate) fn fail(&mut self, e: Error) {
        self.failed = true;
        if self.pending_error.is_none() {
            self.pending_error = Some(e);
        }
    }

    /// `mp3dec_ex_read_frame`: returns the range of `buffer` to output.
    fn read_frame(
        &mut self,
        input: &mut impl Input,
        frame_info: &mut RawInfo,
        max_samples: usize,
    ) -> Option<core::ops::Range<usize>> {
        if self.detected_samples != 0 && self.cur_sample >= self.detected_samples {
            return None; // at end of stream
        }
        if self.failed {
            return None; // error state; seek resets it
        }
        input.begin_read();
        while self.buffer_consumed == self.buffer_samples {
            let dec_buf = input.window(self);
            if dec_buf.is_empty() {
                return None;
            }
            let dec_buf = &dec_buf[..dec_buf.len().min(i32::MAX as usize)];
            self.buffer_samples =
                core_private::decode_frame(&mut self.dec, dec_buf, Some(&mut self.buffer[..]), frame_info) as i32;
            // Samples of an undecodable frame, needed below; C reads them from
            // whatever bytes are at the current position.
            let mut hdr = [0; 4];
            let n = dec_buf.len().min(4);
            hdr[..n].copy_from_slice(&dec_buf[..n]);
            input.consume(frame_info.frame_bytes);
            self.buffer_consumed = 0;
            if self.info.hz != frame_info.hz || self.info.layer != frame_info.layer {
                self.fail(Error::FormatChanged);
                return None;
            }
            if self.buffer_samples != 0 {
                self.buffer_samples *= i32::from(frame_info.channels);
                if self.to_skip > 0 {
                    let skip = self.buffer_samples.min(self.to_skip);
                    self.buffer_consumed += skip;
                    self.to_skip -= skip;
                }
                if !self.allow_transition
                    && self.buffer_consumed != self.buffer_samples
                    && self.info.channels != frame_info.channels
                {
                    self.fail(Error::FormatChanged);
                    return None;
                }
            } else if self.to_skip > 0 {
                // Decoding can't always start at any frame because of the bit
                // reservoir; count skipped samples for such frames.
                let frame_samples = Header(hdr).frame_samples() as i32 * i32::from(frame_info.channels);
                self.to_skip -= frame_samples.min(self.to_skip);
            }
            self.offset += frame_info.frame_bytes as u64;
        }
        let mut out_samples = ((self.buffer_samples - self.buffer_consumed).max(0) as usize).min(max_samples);
        if self.detected_samples != 0 && self.cur_sample + out_samples as u64 >= self.detected_samples {
            // Count decoded samples to cut the padding properly.
            out_samples = (self.detected_samples - self.cur_sample) as usize;
        }
        self.cur_sample += out_samples as u64;
        let start = self.buffer_consumed as usize;
        self.buffer_consumed += out_samples as i32;
        Some(start..start + out_samples)
    }

    /// `mp3dec_ex_read`.
    pub(crate) fn read(&mut self, input: &mut impl Input, out: &mut [S]) -> Result<usize, Error> {
        let mut frame_info = RawInfo::default();
        let mut n = 0;
        while n < out.len() {
            let Some(r) = self.read_frame(input, &mut frame_info, out.len() - n) else {
                break;
            };
            if r.is_empty() {
                break;
            }
            out[n..n + r.len()].copy_from_slice(&self.buffer[r.clone()]);
            n += r.len();
        }
        if n == 0 {
            if let Some(e) = self.pending_error.take() {
                return Err(e);
            }
        }
        Ok(n)
    }

    /// One frame's worth of samples (`mp3dec_ex_read_frame` with no limit).
    pub(crate) fn read_frame_samples(&mut self, input: &mut impl Input) -> Result<Option<&[S]>, Error> {
        let mut frame_info = RawInfo::default();
        match self.read_frame(input, &mut frame_info, usize::MAX) {
            Some(r) if !r.is_empty() => Ok(Some(&self.buffer[r])),
            _ => match self.pending_error.take() {
                Some(e) => Err(e),
                None => Ok(None),
            },
        }
    }

    pub(crate) fn channels(&self) -> Option<Channels> {
        match self.info.channels {
            1 => Some(Channels::Mono),
            2 => Some(Channels::Stereo),
            _ => None,
        }
    }

    /// Total length in interleaved samples, if known.
    pub(crate) fn total(&self) -> Option<u64> {
        (self.samples != 0 || self.indexes_built || self.vbr_tag_found).then_some(self.samples)
    }
}

/// `mp3dec_idx_binary_search`: the last index entry at or before `position`.
fn idx_binary_search(index: &[IndexEntry], position: u64) -> usize {
    let (mut start, mut end, mut found) = (0, index.len(), 0);
    while start <= end {
        let mid = (start + end) / 2;
        if index[mid].sample >= position {
            if index[mid].sample == position {
                return mid;
            }
            // C underflows here; unreachable because index[0].sample == 0 and
            // position > 0.
            let Some(e) = mid.checked_sub(1) else { break };
            end = e;
        } else {
            found = mid;
            start = mid + 1;
            if start == index.len() {
                break;
            }
        }
    }
    found
}

/// Frames from an in-memory buffer (`mp3dec_ex_open_buf`).
#[doc(hidden)]
pub struct SliceInput<'a> {
    pub(crate) file: &'a [u8],
}

impl Input for SliceInput<'_> {
    fn iterate(
        &mut self,
        start: u64,
        compat: bool,
        f: &mut dyn FnMut(&RawFrame) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let buf = &self.file[(start as usize).min(self.file.len())..];
        for frame in RawFrames::new(buf, compat) {
            if f(&frame).is_break() {
                break;
            }
        }
        Ok(())
    }

    fn frame_at(&mut self, offset: u64, free_format_bytes: usize) -> Result<(&[u8], usize), Error> {
        let frame = &self.file[offset as usize..];
        let hdr = Header::read(frame);
        Ok((frame, hdr.frame_bytes(free_format_bytes) + hdr.padding()))
    }

    fn header_at(&mut self, offset: u64) -> Result<Header, Error> {
        Ok(Header::read(&self.file[offset as usize..]))
    }

    fn reposition(&mut self, _offset: u64) -> Result<(), Error> {
        Ok(())
    }

    fn byte_seek_target(&self, position: u64) -> u64 {
        position.min(self.file.len() as u64)
    }

    fn window(&mut self, st: &mut State<impl Sample>) -> &[u8] {
        let end = if st.end_offset != 0 { st.end_offset } else { self.file.len() as u64 };
        // C computes `end - offset` unsigned, which wraps (and over-reads)
        // after a byte seek into trailing tags; there's nothing to decode there.
        let (start, end) = (st.offset as usize, end as usize);
        self.file.get(start..end).unwrap_or(&[])
    }

    fn consume(&mut self, _n: usize) {}
}

/// A seekable reader over MP3 data in memory. See the crate documentation.
///
/// Positions and lengths count samples per channel (a stereo stream's sample 10
/// is interleaved samples 20 and 21), while [`read`](Self::read) fills a
/// buffer of interleaved samples.
pub struct SliceReader<'a, S = f32> {
    pub(crate) st: State<S>,
    pub(crate) input: SliceInput<'a>,
}

impl<'a, S: Sample> SliceReader<'a, S> {
    /// Opens `data`, scanning it for its length (see [`Options::skip_scan`]).
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_options(data, Options::default())
    }

    pub fn with_options(data: &'a [u8], options: Options) -> Self {
        let mut r = Self { st: State::new(options), input: SliceInput { file: data } };
        r.st.open(&mut r.input).expect("slice input can't fail");
        r
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

    #[doc(hidden)]
    pub fn __raw(&mut self) -> Raw<'_, S, SliceInput<'a>> {
        Raw::new(&mut self.st, &mut self.input)
    }
}

/// Unstable access to minimp3_ex's raw state, for parity tests.
#[doc(hidden)]
pub struct Raw<'r, S, I> {
    st: &'r mut State<S>,
    input: &'r mut I,
}

impl<'r, S, I> Raw<'r, S, I> {
    pub(crate) fn new(st: &'r mut State<S>, input: &'r mut I) -> Self {
        Raw { st, input }
    }
}

#[allow(private_bounds)]
impl<S: Sample, I: Input> Raw<'_, S, I> {
    /// `mp3dec_ex_seek` with an interleaved position.
    pub fn seek(&mut self, position: u64, to_byte: bool) -> Result<(), Error> {
        self.st.seek(self.input, position, to_byte)
    }
    /// `(samples, detected_samples, cur_sample, vbr_tag_found, failed)`
    pub fn state(&self) -> (u64, u64, u64, bool, bool) {
        (self.st.samples, self.st.detected_samples, self.st.cur_sample, self.st.vbr_tag_found, self.st.failed)
    }
    /// `(channels, hz, layer)` of the first frame.
    pub fn info(&self) -> (u8, u32, u8) {
        (self.st.info.channels, self.st.info.hz, self.st.info.layer)
    }
    /// `mp3dec_ex_read_frame` with a sample limit.
    pub fn read_frame(&mut self, max_samples: usize) -> Option<&[S]> {
        let mut info = RawInfo::default();
        let r = self.st.read_frame(self.input, &mut info, max_samples)?;
        Some(&self.st.buffer[r])
    }
}
