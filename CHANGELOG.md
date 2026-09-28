# Changelog

## 0.2.0

The crate is split in two. **`nanomp3-core`** is the frame decoder that used to be `nanomp3`; **`nanomp3`** re-exports it and adds a port of minimp3's `minimp3_ex` helpers. Both crates are versioned together, and `nanomp3` depends on exactly the same `nanomp3-core` version.

### Breaking changes

- `Decoder::decode` now returns `(usize, Result<FrameInfo, DecodeError>)` instead of `(usize, Option<FrameInfo>)`. `DecodeError` says why no audio came out (`NoFrame`, `ReservoirUnavailable`, `Corrupt`, `UnsupportedLayer`) and, for all but `NoFrame`, carries the frame's `FrameInfo`. Code that used `if let Some(info)` becomes `if let Ok(info)`.
- `Decoder::decode` is generic over the output sample type (`f32`, `i8`, `u8`, `i16`, `u16`, `i32`, `u32`). Buffers whose element type was only pinned down by the old `&mut [f32]` signature (such as `vec![0.0; n]`) need an explicit type.
- `FrameInfo` is `#[non_exhaustive]` and has a new `layer` field.
- The minimum supported Rust version is 1.89 (1.80 without the `wide` feature).

### Behavior changes

- **Mono/stereo switches decode correctly.** The original translation used minimp3's `MINIMP3_NONSTANDARD_BUT_LOGICAL` filterbank update, which deviates from the ISO reference on streams that switch between mono and stereo (51 dB PSNR on the conformance vector, versus 117 dB now). Output now matches minimp3's standard build bit for bit.
- **MPEG Layer I and II (MP1/MP2) frames are decoded** (feature `layer12`, on by default). Previously they were skipped.
- **`i16` rounding is fixed.** minimp3 rounds samples in (−1.5, −0.5] to 0 instead of −1; nanomp3 now rounds half away from zero. `Decoder::new_minimp3_compat()` restores minimp3's exact output.
- Inputs shorter than 4 bytes, and several malformed inputs, no longer panic.

### Added

- `nanomp3`: `Reader` (over any `Read + Seek`) and `SliceReader` (over bytes in memory): seekable, sample-accurate readers that skip tags, trim the encoder delay and padding for gapless playback, and know the stream length. `Options` configures them, including `minimp3_compat`.
- `nanomp3`: `decode_all` / `decode_all_reader` decode a whole stream into a `Vec`.
- `nanomp3`: `frames` / `for_each_frame_reader` iterate over frames without decoding; `VbrTag` parses Xing/Info/LAME tags; `detect` / `detect_reader` recognize MPEG audio; `strip_tags`, `strip_trailing_tags` and `id3v2_len` handle ID3v2, ID3v1 and APEv2 tags.
- `Decoder::new_minimp3_compat()`, `Sample`, `DecodeError`.
- Cargo features: `wide` (SIMD via the `wide` crate) and `layer12` in both crates; `alloc` and `std` in `nanomp3`.

### Fixed minimp3_ex bugs

The `minimp3_ex` port matches minimp3_ex exactly with `Options::minimp3_compat`. By default, these bugs are fixed, and the I/O reader behaves exactly like the in-memory one:

- Header-less APEv2 tags are over-trimmed by 32 bytes of audio.
- The I/O reader strips trailing tags again every time it reaches the end of the stream.
- Junk longer than the 128 KiB read buffer ends the I/O scan early (short length, incomplete seek index).
- The I/O frame iterator overstates the bytes available to the decoder.
- An I/O seek fails when a frame it needs is cut short by the end of the file.
- After a failed seek, minimp3_ex's next read returns memory past its buffer; nanomp3 stays in its error state until the next successful seek.

### Internal

- No `unsafe`: both crates are `#![forbid(unsafe_code)]`.
- Decoding is 36–79% faster than 0.1 and faster than minimp3's own SIMD build, with output still bit-identical to minimp3's reference build.
- `parity/` compares nanomp3 against upstream minimp3 and minimp3_ex (a git submodule) bit for bit; `fuzz/` has differential and robustness fuzz targets.
- Items marked `#[doc(hidden)]` (`nanomp3_core::__private`, `nanomp3::__compat`, `nanomp3::__Raw`, `__raw()`, `Sample::from_synth`) are internal and not covered by semver.
