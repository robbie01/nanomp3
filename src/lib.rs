#![no_std]

use minimp3::mp3dec_frame_info_t;

mod minimp3;

pub const MAX_SAMPLES_PER_FRAME: usize = 1152*2;

pub struct Decoder(minimp3::mp3dec_t);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channels {
    Mono = 1,
    Stereo
}

impl Channels {
    pub fn num(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FrameInfo {
    pub samples_produced: usize,
    pub channels: Channels,
    pub sample_rate: u32,
    pub bitrate_kbps: u32
}

impl Decoder {
    pub const fn new() -> Self {
        Self(minimp3::mp3dec_t::new())
    }

    // Decode MP3 data into a buffer, returning the amount of MP3 data consumed and any data on decoded samples.
    // mp3 should contain at least several frames worth of data (16KB recommended) to avoid artifacting.
    // pcm MUST be at least MAX_SAMPLES_PER_FRAME long.
    pub fn decode(&mut self, mp3: &[u8], pcm: &mut [i16]) -> (usize, Option<FrameInfo>) {
        if pcm.len() < MAX_SAMPLES_PER_FRAME {
            panic!("pcm buffer too small");
        }

        let mut info = mp3dec_frame_info_t::default();

        let samples = unsafe { minimp3::mp3dec_decode_frame(
            &mut self.0,
            mp3.as_ptr(),
            mp3.len().try_into().unwrap(),
            pcm.as_mut_ptr(),
            &mut info
        ) };

        (
            info.frame_bytes.try_into().unwrap(),
            (samples != 0).then(|| FrameInfo {
                samples_produced: samples.try_into().unwrap(),
                channels: match info.channels {
                    1 => Channels::Mono,
                    2 => Channels::Stereo,
                    _ => unreachable!()
                },
                sample_rate: info.hz.try_into().unwrap(),
                bitrate_kbps: info.bitrate_kbps.try_into().unwrap()
            })
        )
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}