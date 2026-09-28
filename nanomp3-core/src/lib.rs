//! The MPEG audio frame decoder behind [nanomp3](https://docs.rs/nanomp3): a
//! safe Rust port of [minimp3](https://github.com/lieff/minimp3) whose output
//! is bit-identical to upstream's.
//!
//! It's `no_std`, allocates nothing and does no buffering: [`Decoder::decode`]
//! finds and decodes one frame at a time from the bytes you give it. Because a
//! frame only counts once the frames after it are seen to be consistent, keep
//! several frames of input available (16 KiB is plenty) and refill as it
//! drains.
//!
//! ```
//! # let data: &[u8] = &[];
//! use nanomp3_core::{DecodeError, Decoder, MAX_SAMPLES_PER_FRAME};
//!
//! let mut decoder = Decoder::new();
//! let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME]; // or i16, u8, i32, ...
//! let mut mp3 = data;
//! while !mp3.is_empty() {
//!     let (consumed, result) = decoder.decode(mp3, &mut pcm);
//!     mp3 = &mp3[consumed..];
//!     match result {
//!         Ok(info) => {
//!             let samples = &pcm[..info.samples_produced * info.channels.num() as usize];
//!             // ... interleaved samples at info.sample_rate
//!         }
//!         // Normal while starting up or after a seek; later frames decode.
//!         Err(DecodeError::ReservoirUnavailable(_)) => {}
//!         Err(e) => eprintln!("skipped: {e}"),
//!     }
//! }
//! ```
//!
//! Most users want the `nanomp3` crate, which re-exports this one and adds a
//! seekable reader with its own buffering, gapless playback, and tag handling.
//!
//! # Features
//!
//! - `wide` (default): explicit SIMD via the `wide` crate. Without it, the
//!   same kernels run on plain arrays and rely on LLVM's auto-vectorizer.
//! - `layer12` (default): decode MPEG Layer I and II frames too. Without it
//!   they are reported as [`DecodeError::UnsupportedLayer`], like minimp3
//!   built with `MINIMP3_ONLY_MP3`.

#![no_std]
#![forbid(unsafe_code)]

mod minimp3;

pub use minimp3::Sample;

#[doc(hidden)]
pub mod __private {
    //! Internals shared with the `nanomp3` crate, which pins an exact version
    //! of this one. Not public API: no semver guarantees.
    pub use crate::minimp3::{l3_side_info, mp3d_find_frame as find_frame, FrameInfo, Header};
    use crate::{minimp3, Decoder, Sample};

    /// `mp3dec_decode_frame` with C's `info` semantics: when no frame is
    /// found, only `frame_bytes` is written.
    pub fn decode_frame<S: Sample>(
        dec: &mut Decoder,
        mp3: &[u8],
        pcm: Option<&mut [S]>,
        info: &mut FrameInfo,
    ) -> usize {
        minimp3::mp3dec_decode_frame(&mut dec.dec, mp3, pcm, info, dec.minimp3_compat)
    }

    /// `mp3dec_init`.
    pub fn init(dec: &mut Decoder) {
        minimp3::mp3dec_init(&mut dec.dec);
    }
}

#[cfg(test)]
mod tests;

/// The minimum length of the PCM output buffer, in samples.
pub const MAX_SAMPLES_PER_FRAME: usize = 1152*2;

/// The core MP3 decoder, with no internal buffering.
///
/// The decoder is about 22 KiB: it holds the filterbank state between frames
/// and the working memory for decoding one. Nothing is allocated.
#[derive(Clone)]
pub struct Decoder {
    dec: minimp3::Mp3Dec,
    minimp3_compat: bool,
}


/// The channel formats that may be encoded in an MP3 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channels {
    Mono = 1,
    Stereo
}

impl Channels {
    /// Returns the corresponding number of channels for `self`.
    pub fn num(self) -> u8 {
        self as u8
    }
}

/// Information about a frame found by [`Decoder::decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrameInfo {
    /// The number of PCM samples produced per channel (0 when the frame
    /// produced no audio).
    pub samples_produced: usize,
    /// The number of channels in this frame.
    pub channels: Channels,
    /// Sample rate of this frame, in Hz.
    pub sample_rate: u32,
    /// The frame's bitrate, in kilobits per second (0 for free-format streams).
    pub bitrate: u32,
    /// The MPEG layer: 1, 2 or 3 (MP1, MP2 or MP3).
    pub layer: u8,
}

/// Why [`Decoder::decode`] produced no audio.
///
/// Every variant except `NoFrame` carries the frame's header information.
/// `NoFrame` at the end of the input and `ReservoirUnavailable` right after
/// starting or seeking are part of normal decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The input holds no complete frame, so the consumed bytes (all of the
    /// input, unless it was empty) were skipped as tags or junk. At the end of
    /// a stream this means there's nothing left; mid-stream, the input was
    /// cut short or corrupt.
    NoFrame,
    /// The frame uses bit-reservoir data from earlier frames that weren't
    /// decoded, as happens for the first frame or two after starting in the
    /// middle of a stream or seeking. Later frames decode normally.
    ReservoirUnavailable(FrameInfo),
    /// The frame is corrupt and was skipped.
    Corrupt(FrameInfo),
    /// The frame is Layer I or II and the `layer12` feature is disabled.
    UnsupportedLayer(FrameInfo),
}

impl DecodeError {
    /// The header information of the skipped frame, if there was one.
    pub fn frame_info(&self) -> Option<&FrameInfo> {
        match self {
            DecodeError::NoFrame => None,
            DecodeError::ReservoirUnavailable(info)
            | DecodeError::Corrupt(info)
            | DecodeError::UnsupportedLayer(info) => Some(info),
        }
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            DecodeError::NoFrame => "no complete MPEG audio frame in the input",
            DecodeError::ReservoirUnavailable(_) => "frame depends on bit reservoir data that wasn't decoded",
            DecodeError::Corrupt(_) => "corrupt frame",
            DecodeError::UnsupportedLayer(_) => "Layer I/II frame (the layer12 feature is disabled)",
        })
    }
}

impl core::error::Error for DecodeError {}

impl Decoder {
    /// Instantiates a `Decoder`.
    pub const fn new() -> Self {
        Self { dec: minimp3::Mp3Dec::new(), minimp3_compat: false }
    }

    /// Instantiates a `Decoder` whose output is bit-identical to minimp3's,
    /// including quirks nanomp3 otherwise corrects. Currently that only
    /// affects `i16` output, where minimp3 rounds samples in (−1.5, −0.5] to
    /// 0 instead of −1 (see [`Sample`]).
    pub const fn new_minimp3_compat() -> Self {
        Self { dec: minimp3::Mp3Dec::new(), minimp3_compat: true }
    }

    /// Decodes the next frame of `mp3` into `pcm`. Returns how many bytes of
    /// `mp3` were consumed (always advance past them, whatever the result) and
    /// either the decoded frame's information or why no audio came out.
    ///
    /// `mp3` should hold several frames' worth of data (16 KiB is plenty):
    /// frames are only recognized once the frames after them are seen, and a
    /// frame cut off at the end of the input is skipped as junk.
    ///
    /// Samples are interleaved. `pcm` can hold `f32` samples (nominally in
    /// `[-1.0, 1.0]`, not clipped) or signed or unsigned 8-, 16- or 32-bit
    /// integers; see [`Sample`] for the conversions.
    ///
    /// # Panics
    ///
    /// Panics if `pcm` is less than [`MAX_SAMPLES_PER_FRAME`] long.
    pub fn decode<S: Sample>(&mut self, mp3: &[u8], pcm: &mut [S]) -> (usize, Result<FrameInfo, DecodeError>) {
        assert!(pcm.len() >= MAX_SAMPLES_PER_FRAME, "pcm buffer too small");

        let mut info = minimp3::FrameInfo::default();
        let samples = minimp3::mp3dec_decode_frame(&mut self.dec, mp3, Some(pcm), &mut info, self.minimp3_compat);
        let frame = FrameInfo {
            samples_produced: samples,
            channels: if info.channels == 1 { Channels::Mono } else { Channels::Stereo },
            sample_rate: info.hz,
            bitrate: info.bitrate_kbps,
            layer: info.layer,
        };
        let result = match info.status {
            minimp3::Status::Decoded => Ok(frame),
            minimp3::Status::NoFrame | minimp3::Status::HeaderOnly => Err(DecodeError::NoFrame),
            minimp3::Status::Corrupt => Err(DecodeError::Corrupt(frame)),
            minimp3::Status::ReservoirUnavailable => Err(DecodeError::ReservoirUnavailable(frame)),
            minimp3::Status::UnsupportedLayer => Err(DecodeError::UnsupportedLayer(frame)),
        };
        (info.frame_bytes, result)
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}