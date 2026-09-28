//! Skipping ID3v2, ID3v1 and APEv2 tags (`mp3dec_skip_id3*` in minimp3_ex).

/// Bytes needed to recognize an ID3v2 tag header.
pub(crate) const ID3_DETECT_SIZE: usize = 10;

/// Returns the length of the ID3v2 tag at the start of `buf` (header, body and
/// footer), or 0 if there isn't one. Only the first 10 bytes are examined, so
/// the length can exceed `buf.len()`.
pub fn id3v2_len(buf: &[u8]) -> usize {
    if buf.len() >= ID3_DETECT_SIZE
        && &buf[..3] == b"ID3"
        && buf[5] & 15 == 0
        && buf[6..10].iter().all(|&b| b & 0x80 == 0)
    {
        let size = buf[6..10].iter().fold(0, |acc, &b| (acc << 7) | usize::from(b & 0x7f)) + 10;
        // footer
        return if buf[5] & 16 != 0 { size + 10 } else { size };
    }
    0
}

/// Removes ID3v1 (including the extended "TAG+" block) and APEv2 tags from
/// the end of `buf`.
pub fn strip_trailing_tags(buf: &[u8]) -> &[u8] {
    strip_trailing_tags_impl(buf, false)
}

pub(crate) fn strip_trailing_tags_impl(buf: &[u8], minimp3_compat: bool) -> &[u8] {
    let mut n = buf.len();
    if n >= 128 && &buf[n - 128..n - 125] == b"TAG" {
        n -= 128;
        if n >= 227 && &buf[n - 227..n - 223] == b"TAG+" {
            n -= 227;
        }
    }
    if n > 32 && &buf[n - 32..n - 24] == b"APETAGEX" {
        let footer = &buf[n - 32..n];
        let tag_size = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as usize;
        let has_header = footer[23] & 0x80 != 0;
        // The size field counts the items and the footer, not the optional
        // header. minimp3 removes the footer and then `tag_size` more, which is
        // right only when a header is present; otherwise it eats 32 bytes of
        // audio.
        let rest = if minimp3_compat {
            tag_size
        } else {
            tag_size.saturating_sub(32) + if has_header { 32 } else { 0 }
        };
        n -= 32;
        if n >= rest {
            n -= rest;
        }
    }
    &buf[..n]
}

/// Removes an ID3v2 tag from the start of `buf` and ID3v1/APEv2 tags from its
/// end.
pub fn strip_tags(buf: &[u8]) -> &[u8] {
    strip_tags_impl(buf, false)
}

pub(crate) fn strip_tags_impl(buf: &[u8], minimp3_compat: bool) -> &[u8] {
    let id3v2 = id3v2_len(buf).min(buf.len());
    strip_trailing_tags_impl(&buf[id3v2..], minimp3_compat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3v2() {
        let mut tag = *b"ID3\x04\x00\x00\x00\x00\x01\x05";
        assert_eq!(id3v2_len(&tag), 128 + 5 + 10);
        tag[5] = 0x10; // footer present
        assert_eq!(id3v2_len(&tag), 128 + 5 + 20);
        tag[7] = 0x80; // not syncsafe
        assert_eq!(id3v2_len(&tag), 0);
        assert_eq!(id3v2_len(b"ID3"), 0);
    }

    fn ape_footer(size: u32, header: bool) -> [u8; 32] {
        let mut f = [0u8; 32];
        f[..8].copy_from_slice(b"APETAGEX");
        f[12..16].copy_from_slice(&size.to_le_bytes());
        if header {
            f[23] = 0x80;
        }
        f
    }

    #[test]
    fn apev2() {
        let audio = [0xAAu8; 100];
        for header in [false, true] {
            let mut file = audio.to_vec();
            if header {
                file.extend_from_slice(&[0; 32]);
            }
            file.extend_from_slice(&[1; 10]); // items
            file.extend_from_slice(&ape_footer(10 + 32, header));
            assert_eq!(strip_trailing_tags(&file), &audio[..], "header: {header}");
            let compat = strip_trailing_tags_impl(&file, true).len();
            assert_eq!(compat, if header { 100 } else { 68 });
        }
    }

    #[test]
    fn id3v1() {
        let mut file = [0xAAu8; 500].to_vec();
        file.extend_from_slice(b"TAG");
        file.extend_from_slice(&[0; 125]);
        assert_eq!(strip_trailing_tags(&file).len(), 500);
    }
}
