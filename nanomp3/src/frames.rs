//! Walking the frames of a stream without decoding them (`mp3dec_iterate_buf`).

use crate::tags::strip_tags_impl;
use crate::Channels;
use nanomp3_core::__private::{find_frame, FrameInfo as RawInfo, Header};

/// An MPEG audio frame found by [`frames`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Frame<'a> {
    /// Offset of the frame from the start of the buffer given to [`frames`].
    pub offset: usize,
    /// The frame, header included.
    pub data: &'a [u8],
    pub channels: Channels,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// MPEG layer (1, 2 or 3).
    pub layer: u8,
    /// Bitrate in kbps; 0 for free-format streams.
    pub bitrate_kbps: u32,
    /// Samples per channel the frame decodes to.
    pub samples: usize,
}

/// Iterates over the frames in `buf`, skipping tags and junk between frames.
///
/// This is how minimp3 finds frames: a frame counts only if it is followed by
/// consistent frame headers, which rules out most false syncs.
pub fn frames(buf: &[u8]) -> Frames<'_> {
    Frames(RawFrames::new(buf, false))
}

/// Iterator returned by [`frames`].
#[derive(Clone)]
pub struct Frames<'a>(RawFrames<'a>);

impl<'a> Iterator for Frames<'a> {
    type Item = Frame<'a>;

    fn next(&mut self) -> Option<Frame<'a>> {
        let f = self.0.next()?;
        let hdr = Header::read(f.data);
        Some(Frame {
            offset: f.offset as usize,
            data: &f.data[..f.frame_size],
            channels: if f.info.channels == 1 { Channels::Mono } else { Channels::Stereo },
            sample_rate: f.info.hz,
            layer: f.info.layer,
            bitrate_kbps: f.info.bitrate_kbps,
            samples: hdr.frame_samples() as usize,
        })
    }
}

/// What minimp3's iterate callback receives.
#[cfg_attr(not(feature = "alloc"), allow(dead_code))]
pub(crate) struct RawFrame<'a> {
    /// From the frame header to the end of the data available to the decoder,
    /// which may extend past the frame.
    pub data: &'a [u8],
    /// C's `buf_size`: `data.len()`, except where minimp3 overstates it.
    pub buf_size: usize,
    pub frame_size: usize,
    pub free_format_bytes: usize,
    pub offset: u64,
    pub info: RawInfo,
}

pub(crate) fn raw_info(hdr: Header, frame_size: usize) -> RawInfo {
    RawInfo {
        frame_bytes: frame_size,
        channels: if hdr.is_mono() { 1 } else { 2 },
        hz: hdr.sample_rate_hz(),
        layer: 4 - hdr.get_layer(),
        bitrate_kbps: hdr.bitrate_kbps(),
        ..RawInfo::default()
    }
}

/// `mp3dec_iterate_buf`.
#[derive(Clone)]
pub(crate) struct RawFrames<'a> {
    buf: &'a [u8],
    pos: usize,
    base: usize,
}

impl<'a> RawFrames<'a> {
    pub fn new(buf: &'a [u8], minimp3_compat: bool) -> Self {
        Self {
            buf: strip_tags_impl(buf, minimp3_compat),
            pos: 0,
            base: crate::id3v2_len(buf).min(buf.len()),
        }
    }
}

impl<'a> Iterator for RawFrames<'a> {
    type Item = RawFrame<'a>;

    fn next(&mut self) -> Option<RawFrame<'a>> {
        loop {
            let mut free_format_bytes = 0;
            let (i, frame_size) = find_frame(&self.buf[self.pos..], &mut free_format_bytes);
            self.pos += i;
            if i != 0 && frame_size == 0 {
                continue;
            }
            if frame_size == 0 {
                return None;
            }
            let data = &self.buf[self.pos..];
            let offset = (self.base + self.pos) as u64;
            self.pos += frame_size;
            return Some(RawFrame {
                data,
                buf_size: data.len(),
                frame_size,
                free_format_bytes,
                offset,
                info: raw_info(Header::read(data), frame_size),
            });
        }
    }
}
