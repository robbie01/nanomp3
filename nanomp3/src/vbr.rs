//! Xing/Info VBR headers with the LAME/Lavc extension (`mp3dec_check_vbrtag`).

use nanomp3_core::__private::{l3_side_info, Header};

/// A Xing or Info tag: the first frame of most encoded MP3 files carries one
/// instead of audio, recording the stream length and, with the LAME/Lavc
/// extension, the encoder delay and padding needed for gapless playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbrTag {
    /// Number of audio frames following the tag frame, if the tag records it.
    pub frames: Option<u32>,
    /// Encoder delay and padding, in samples per channel, from the LAME/Lavc
    /// extension if present.
    pub encoder_delay_padding: Option<(u16, u16)>,
}

impl VbrTag {
    /// Parses the tag from a complete Layer III frame (header included), such
    /// as [`Frame::data`](crate::Frame::data). Returns `None` if the frame
    /// isn't a tag frame.
    pub fn parse(frame: &[u8]) -> Option<VbrTag> {
        if frame.len() < 4 || !Header::read(frame).valid() || Header::read(frame).get_layer() != 1 {
            return None;
        }
        check_vbrtag(frame, frame.len())
    }

    #[cfg(feature = "alloc")]
    /// minimp3's start delay (skipped samples per channel, including the
    /// decoder's own 529-sample delay) and end padding. Both are 0 without the
    /// LAME extension.
    pub(crate) fn delay_padding(&self) -> (i32, i32) {
        match self.encoder_delay_padding {
            Some((delay, padding)) => (i32::from(delay) + 529, i32::from(padding) - 529),
            None => (0, 0),
        }
    }
}

/// `mp3dec_check_vbrtag`: `None` if there is no tag; a tag without a frame
/// count is C's -1.
pub(crate) fn check_vbrtag(frame: &[u8], frame_size: usize) -> Option<VbrTag> {
    const FRAMES_FLAG: u8 = 1;
    const BYTES_FLAG: u8 = 2;
    const TOC_FLAG: u8 = 4;
    const VBR_SCALE_FLAG: u8 = 8;

    if frame_size < 4 + 8 {
        return None;
    }
    // Side info corrupted?
    let (pos, _) = l3_side_info(frame, frame_size)?;

    // C trusts frame_size; never read past the bytes we actually have.
    let frame = &frame[..frame_size.min(frame.len())];
    let size = frame.len();
    let mut off = 4 + (pos / 8) as usize;
    if off > size || size - off < 8 {
        return None;
    }
    let tag = &frame[off..];
    if &tag[..4] != b"Xing" && &tag[..4] != b"Info" {
        return None;
    }
    let flags = tag[7];
    if flags & FRAMES_FLAG == 0 {
        return Some(VbrTag { frames: None, encoder_delay_padding: None });
    }
    off += 8;
    if size - off < 4 {
        return None;
    }
    let frames = u32::from_be_bytes(frame[off..off + 4].try_into().unwrap());
    off += 4;
    for (flag, len) in [(BYTES_FLAG, 4), (TOC_FLAG, 100), (VBR_SCALE_FLAG, 4)] {
        if flags & flag != 0 {
            if size - off < len {
                return None;
            }
            off += len;
        }
    }
    if size - off < 1 {
        return None;
    }
    let mut encoder_delay_padding = None;
    if frame[off] != 0 {
        // Extension (LAME, Lavc, etc.); they share this layout.
        if size - off <= 35 {
            return None;
        }
        let t = &frame[off + 21..];
        let delay = (u16::from(t[0]) << 4) | u16::from(t[1] >> 4);
        let padding = (u16::from(t[1] & 0xF) << 8) | u16::from(t[2]);
        encoder_delay_padding = Some((delay, padding));
    }
    Some(VbrTag { frames: Some(frames), encoder_delay_padding })
}
