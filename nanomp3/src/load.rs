//! Decoding a whole stream into memory (`mp3dec_load_*` in minimp3_ex).

use alloc::vec::Vec;

use crate::frames::raw_info;
use crate::reader::Options;
use crate::tags::strip_tags_impl;
use crate::vbr::check_vbrtag;
use crate::{Channels, Decoder, Sample, VbrTag, MAX_SAMPLES_PER_FRAME};
use nanomp3_core::__private::{self as core_private, find_frame, FrameInfo as RawInfo, Header};

/// A decoded stream, from [`decode_all`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Decoded<S> {
    /// Interleaved samples, with the encoder delay and padding trimmed when
    /// the stream has a LAME/Lavc tag.
    pub samples: Vec<S>,
    /// Channels of the stream; `None` if there was no audio, or the stream
    /// switched between mono and stereo (see
    /// [`Options::allow_mono_stereo_transition`]).
    pub channels: Option<Channels>,
    /// Sample rate in Hz (0 if there was no audio).
    pub sample_rate: u32,
    /// MPEG layer (1, 2 or 3; 0 if there was no audio).
    pub layer: u8,
    /// Average of the frame bitrates, in kbps.
    pub avg_bitrate_kbps: u32,
    /// Decoding stopped early because the sample rate, layer or (unless
    /// allowed) channel count changed; `samples` holds the audio before that.
    pub format_changed: bool,
}

impl<S> Decoded<S> {
    pub(crate) fn empty() -> Self {
        Self { samples: Vec::new(), channels: None, sample_rate: 0, layer: 0, avg_bitrate_kbps: 0, format_changed: false }
    }
}

/// Decodes all of `data` (`mp3dec_load_buf`). Tags are skipped, and the
/// encoder delay and padding are trimmed when the stream has a LAME/Lavc tag.
pub fn decode_all<S: Sample>(data: &[u8]) -> Decoded<S> {
    decode_all_with(data, Options::default())
}

/// [`decode_all`] with options. [`Options::skip_scan`] has no effect.
pub fn decode_all_with<S: Sample>(data: &[u8], options: Options) -> Decoded<S> {
    let compat = options.minimp3_compat;
    let mut out = Decoded::empty();
    let mut buf = strip_tags_impl(data, compat);
    if buf.is_empty() {
        return out;
    }

    // Find the first frame and check it for a VBR tag.
    let mut to_skip = 0;
    let mut detected = 0;
    let mut frame_info;
    loop {
        let (i, frame_size) = find_frame(buf, &mut 0);
        buf = &buf[i..];
        if i != 0 && frame_size == 0 {
            continue;
        }
        if frame_size == 0 {
            return out;
        }
        frame_info = raw_info(Header::read(buf), frame_size);
        if frame_info.layer != 3 {
            break;
        }
        let tag = check_vbrtag(buf, frame_size);
        if let Some(tag @ VbrTag { frames: Some(frames), .. }) = tag {
            let samples = u64::from(Header::read(buf).frame_samples()) * u64::from(frame_info.channels);
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
                return out;
            }
        }
        if tag.is_some() {
            buf = &buf[frame_size..];
        }
        break;
    }

    let mut decoder = if compat { Decoder::new_minimp3_compat() } else { Decoder::new() };
    load_body(&mut out, &mut decoder, &mut frame_info, to_skip, detected, options, |dec, pcm, info| {
        let n = core_private::decode_frame(dec, &buf[..buf.len().min(i32::MAX as usize)], Some(pcm), info);
        buf = &buf[info.frame_bytes..];
        Some(n)
    });
    out
}

/// The decode loop shared by [`decode_all`] and
/// [`decode_all_reader`](crate::decode_all_reader). `decode` decodes the next
/// frame into `pcm`, or returns `None` to stop (on an I/O error).
pub(crate) fn load_body<S: Sample>(
    out: &mut Decoded<S>,
    dec: &mut Decoder,
    frame_info: &mut RawInfo,
    mut to_skip: i32,
    detected: u64,
    options: Options,
    mut decode: impl FnMut(&mut Decoder, &mut [S], &mut RawInfo) -> Option<usize>,
) {
    let mut channels = frame_info.channels;
    out.sample_rate = frame_info.hz;
    out.layer = frame_info.layer;
    let (mut len, mut bitrate_sum, mut frames) = (0usize, 0u64, 0u64);
    let zero = S::from_synth(0.0, false);
    loop {
        if out.samples.len() < len + MAX_SAMPLES_PER_FRAME {
            out.samples.resize(len + MAX_SAMPLES_PER_FRAME, zero);
        }
        let Some(samples) = decode(dec, &mut out.samples[len..], frame_info) else {
            break;
        };
        if samples != 0 {
            if out.sample_rate != frame_info.hz || out.layer != frame_info.layer {
                out.format_changed = true;
                break;
            }
            if channels != 0 && channels != frame_info.channels {
                if options.allow_mono_stereo_transition {
                    channels = 0; // mark the stream as mixed
                } else {
                    out.format_changed = true;
                    break;
                }
            }
            let mut samples = samples * usize::from(frame_info.channels);
            if to_skip > 0 {
                let skip = samples.min(to_skip as usize);
                to_skip -= skip as i32;
                samples -= skip;
                // minimp3 moves the rest to the very start of the buffer, which
                // is where it belongs: nothing is kept while skipping.
                out.samples.copy_within(len + skip..len + skip + samples, 0);
            }
            len += samples;
            bitrate_sum += u64::from(frame_info.bitrate_kbps);
            frames += 1;
        }
        if frame_info.frame_bytes == 0 {
            break;
        }
    }
    if detected != 0 && len as u64 > detected {
        len = detected as usize; // cut the padding
    }
    out.samples.truncate(len);
    out.channels = match channels {
        1 => Some(Channels::Mono),
        2 => Some(Channels::Stereo),
        _ => None,
    };
    if let Some(avg) = bitrate_sum.checked_div(frames) {
        out.avg_bitrate_kbps = avg as u32;
    }
}
