//! A pure Rust MPEG audio (MP3, MP2, MP1) decoder: a safe, bit-exact port of
//! [minimp3](https://github.com/lieff/minimp3) and its `minimp3_ex` helpers.
//!
//! - [`Reader`] (over any `Read + Seek`) and [`SliceReader`] (over bytes in
//!   memory) are the easy way in: they skip tags, trim the encoder delay and
//!   padding for gapless playback, know the stream length, and seek to exact
//!   samples.
//! - [`decode_all`] decodes a whole buffer into a `Vec`.
//! - [`frames`], [`VbrTag`], [`detect`] and the tag functions work without
//!   allocating.
//! - [`Decoder`] (from `nanomp3-core`) decodes one frame at a time with no
//!   allocation or buffering at all.
//!
//! ```no_run
//! # fn main() -> Result<(), nanomp3::Error> {
//! let file = std::fs::File::open("song.mp3")?;
//! let mut reader = nanomp3::Reader::<_, f32>::new(std::io::BufReader::new(file))?;
//! println!("{:?} samples per channel at {} Hz", reader.total_samples(), reader.sample_rate());
//! reader.seek(44_100)?; // one second in, at 44.1 kHz
//! let mut pcm = vec![0f32; 4096];
//! while let n @ 1.. = reader.read(&mut pcm)? {
//!     let interleaved = &pcm[..n];
//!     // ...
//! }
//! # Ok(()) }
//! ```
//!
//! # Features
//!
//! - `std` (default): [`Reader`] and the other `*_reader` functions. Implies `alloc`.
//! - `alloc`: [`SliceReader`] and [`decode_all`].
//! - `wide` and `layer12` (default): see `nanomp3-core`.
//!
//! # minimp3 compatibility
//!
//! Output matches minimp3 bit for bit, except where minimp3 has an obvious
//! bug: nanomp3 fixes those by default, and [`Options::minimp3_compat`] (or
//! [`Decoder::new_minimp3_compat`]) restores minimp3's behavior exactly. The
//! fixed bugs are:
//!
//! - `i16` output rounds samples in (−1.5, −0.5] to 0 instead of −1.
//! - APEv2 tags without a header are over-trimmed by 32 bytes of audio.
//! - With I/O readers, tags at the end of the stream are stripped again every
//!   time the end is reached, which can cut audio that happens to end like a
//!   tag.
//! - With I/O readers, junk longer than the read buffer (128 KiB) ends the
//!   scan early, so the length and seek index stop short.
//! - With I/O readers, the decoder is told there are more bytes available
//!   than there are, and can read stale data (in C, past the buffer). This
//!   can make the reported length come out short.
//! - With I/O readers, a seek fails with an I/O error if a frame it needs to
//!   re-read is cut short by the end of the file.
//!
//! By default, the I/O readers behave exactly like the in-memory ones, and
//! those behave exactly like minimp3 (except for the tag fix above).

#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub use nanomp3_core::{Channels, Decoder, FrameInfo, Sample, MAX_SAMPLES_PER_FRAME};

mod frames;
mod tags;
mod vbr;
pub use frames::{frames, Frame, Frames};
pub use tags::{id3v2_len, strip_tags, strip_trailing_tags};
pub use vbr::VbrTag;

#[cfg(feature = "alloc")]
mod load;
#[cfg(feature = "alloc")]
mod reader;
#[cfg(feature = "alloc")]
pub use load::{decode_all, decode_all_with, Decoded};
#[cfg(feature = "alloc")]
pub use reader::{Options, SliceReader};
#[cfg(feature = "alloc")]
#[doc(hidden)]
pub use reader::Raw as __Raw;

#[cfg(feature = "std")]
mod io;
#[cfg(feature = "std")]
pub use io::{decode_all_reader, decode_all_reader_with, detect_reader, for_each_frame_reader, Reader};

/// Bytes [`detect`] looks at (minimp3's `MINIMP3_BUF_SIZE`).
const DETECT_SIZE: usize = 16 * 1024;

/// Guesses whether `buf` (the start of a file) is MPEG audio: it starts with an
/// ID3v2 tag, or valid frames follow each other within the first 16 KiB
/// (`mp3dec_detect_buf`).
pub fn detect(buf: &[u8]) -> bool {
    detect_impl(buf, false)
}

fn detect_impl(buf: &[u8], minimp3_compat: bool) -> bool {
    if buf.len() < tags::ID3_DETECT_SIZE {
        return false; // too small to be MPEG audio
    }
    if id3v2_len(buf) != 0 {
        return true; // an ID3v2 tag is enough evidence
    }
    let buf = tags::strip_trailing_tags_impl(buf, minimp3_compat);
    let buf = &buf[..buf.len().min(DETECT_SIZE)];
    nanomp3_core::__private::find_frame(buf, &mut 0).1 != 0
}

/// Errors from the readers and `*_reader` functions.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The sample rate or MPEG layer changed mid-stream, or the number of
    /// channels did (unless [`Options::allow_mono_stereo_transition`]).
    FormatChanged,
    /// Reading the input failed.
    #[cfg(feature = "std")]
    Io(std::io::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::FormatChanged => f.write_str("the stream's format changed mid-stream"),
            #[cfg(feature = "std")]
            Error::Io(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            #[cfg(feature = "std")]
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// minimp3-compatible variants of functions without an options argument, for
/// the parity tests. Not public API.
#[doc(hidden)]
pub mod __compat {
    pub fn detect(buf: &[u8]) -> bool {
        super::detect_impl(buf, true)
    }

    pub fn strip_trailing_tags(buf: &[u8]) -> &[u8] {
        crate::tags::strip_trailing_tags_impl(buf, true)
    }

    /// (offset, frame_size, free_format_bytes, buf_size, hz, channels, layer, bitrate_kbps)
    pub type RawFrame = (u64, usize, usize, usize, u32, u8, u8, u32);

    #[cfg(feature = "alloc")]
    fn tuple(f: &crate::frames::RawFrame) -> RawFrame {
        let i = f.info;
        (f.offset, f.frame_size, f.free_format_bytes, f.buf_size, i.hz, i.channels, i.layer, i.bitrate_kbps)
    }

    #[cfg(feature = "alloc")]
    pub fn frames(buf: &[u8], compat: bool) -> alloc::vec::Vec<RawFrame> {
        crate::frames::RawFrames::new(buf, compat).map(|f| tuple(&f)).collect()
    }

    #[cfg(feature = "std")]
    pub fn frames_reader<R: std::io::Read + std::io::Seek>(
        mut io: R,
        compat: bool,
    ) -> std::io::Result<std::vec::Vec<RawFrame>> {
        let mut buf = std::vec![0; 2 * crate::io::IO_SIZE];
        let mut v = std::vec::Vec::new();
        let r = crate::io::iterate_cb(&mut io, &mut buf, compat, &mut |f| {
            v.push(tuple(f));
            core::ops::ControlFlow::Continue(())
        });
        match r {
            Err(crate::Error::Io(e)) => Err(e),
            _ => Ok(v),
        }
    }

    #[cfg(feature = "std")]
    pub fn detect_reader<R: std::io::Read + std::io::Seek>(mut io: R) -> std::io::Result<bool> {
        crate::io::detect_reader_impl(&mut io, true)
    }
}
