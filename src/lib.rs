#![no_std]
#![forbid(unsafe_code)]

mod minimp3;

pub use minimp3::Sample;

#[cfg(test)]
mod tests;

/// The minimum length of the PCM output buffer, in samples.
pub const MAX_SAMPLES_PER_FRAME: usize = 1152*2;

/// The core MP3 decoder, with no internal buffering.
///
/// The decoder is about 22 KiB: it holds the filterbank state between frames
/// and the working memory for decoding one. Nothing is allocated.
#[derive(Clone)]
pub struct Decoder(minimp3::Mp3Dec);


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

/// Information about the frame decoded by [`Decoder::decode`]
#[derive(Debug, Clone, Copy)]
pub struct FrameInfo {
    /// The number of PCM samples produced per channel.
    pub samples_produced: usize,
    /// The number of channels in this frame.
    pub channels: Channels,
    /// Sample rate of this frame, in Hz.
    pub sample_rate: u32,
    /// The current MP3 bit rate, in kilobits per second.
    pub bitrate: u32
}

impl Decoder {
    /// Instantiates a `Decoder`.
    pub const fn new() -> Self {
        Self(minimp3::Mp3Dec::new())
    }

    /// Decode MP3 data into a buffer, returning the amount of MP3 data consumed and info about decoded samples.
    /// `mp3` should contain at least several frames worth of data at any given time (16KiB recommended) to avoid artifacting.
    ///
    /// Samples are interleaved. `pcm` can hold `f32` samples (nominally in
    /// `[-1.0, 1.0]`, not clipped) or `i16` samples (rounded and clipped the
    /// same way minimp3 does), which halves the buffer size.
    ///
    /// When no audio is produced (`None`), the consumed bytes were skipped: tags
    /// or garbage before a frame, a frame whose bit reservoir isn't available
    /// yet (e.g. right after a seek), or a corrupt frame.
    ///
    /// # Panics
    ///
    /// Panics if `pcm` is less than [`MAX_SAMPLES_PER_FRAME`] long.
    pub fn decode<S: Sample>(&mut self, mp3: &[u8], pcm: &mut [S]) -> (usize, Option<FrameInfo>) {
        assert!(pcm.len() >= MAX_SAMPLES_PER_FRAME, "pcm buffer too small");

        let mut info = minimp3::FrameInfo::default();
        let samples = minimp3::mp3dec_decode_frame(&mut self.0, mp3, Some(pcm), &mut info);

        (
            info.frame_bytes,
            (samples != 0).then_some(FrameInfo {
                samples_produced: samples,
                channels: match info.channels {
                    1 => Channels::Mono,
                    _ => Channels::Stereo,
                },
                sample_rate: info.hz,
                bitrate: info.bitrate_kbps,
            }),
        )
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}