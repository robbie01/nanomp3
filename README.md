# nanomp3

A pure Rust MPEG audio decoder (MP3, plus MP1/MP2) based on [minimp3](https://github.com/lieff/minimp3).

- **`no_std`, no allocation.** The decoder is a single ~22 KiB struct.
- **No `unsafe`.** The crate is `#![forbid(unsafe_code)]`.
- **Bit-exact with minimp3.** Output is identical, bit for bit, to upstream's reference (scalar) build on every one of its conformance vectors. CI checks this on x86-64 and ARM64. (One known minimp3 bug is fixed by default and available as an opt-in; see below.)
- **Flexible output.** Interleaved `f32`, `i8`, `u8`, `i16`, `u16`, `i32` or `u32` samples.
- **Faster than minimp3.** SIMD kernels (via [`wide`](https://crates.io/crates/wide)) are written so each lane does exactly the scalar arithmetic, so the speedup doesn't cost bit-exactness. Upstream's own SIMD build doesn't manage that.

```rust
let mut decoder = nanomp3::Decoder::new();
let mut pcm = [0f32; nanomp3::MAX_SAMPLES_PER_FRAME]; // or i16, u8, i32, ...
let mut mp3: &[u8] = &data;
while !mp3.is_empty() {
    let (consumed, info) = decoder.decode(mp3, &mut pcm);
    mp3 = &mp3[consumed..];
    if let Some(info) = info {
        let samples = &pcm[..info.samples_produced * info.channels.num() as usize];
        // ... interleaved samples at info.sample_rate
    }
}
```

⚠️ Like minimp3, the decoder does no internal buffering. When streaming, keep a read-ahead buffer of several frames (16 KiB is plenty) and refill it as it drains. See `examples/measure` for a complete example.

## minimp3 compatibility

nanomp3 reproduces minimp3's behavior, with one policy: where minimp3 does something that is clearly a bug, nanomp3 fixes it by default and keeps the original behavior available through `Decoder::new_minimp3_compat()`. Currently there is one such case: minimp3's `i16` output rounds samples in (−1.5, −0.5] to 0 instead of −1 (about 0.3% of samples in the conformance vectors). `f32` output is identical either way.

## Features

| feature | default | |
|---|---|---|
| `wide` | yes | Explicit SIMD via `wide`. Without it, the same kernels run on plain arrays and rely on LLVM's auto-vectorizer (a few percent slower, no dependencies). |
| `layer12` | yes | Decode MPEG Layer I/II frames too. Without it they are skipped, like minimp3 built with `MINIMP3_ONLY_MP3`. |

MSRV is 1.89 (1.80 without `wide`).

## Performance

Throughput decoding in memory, in MiB/s of MP3 input (higher is better; Windows, x86-64, both C builds compiled with clang `-O3`):

| | nanomp3 | minimp3 (SIMD) | minimp3 (scalar) | nanomp3 0.1 |
|---|---:|---:|---:|---:|
| 48 kHz stereo, 320 kbps | **76.0** | 71.9 | 67.8 | 55.7 |
| 22 kHz MPEG-2, mixed bitrates | **72.7** | 60.3 | 53.0 | 40.6 |
| mode switching, short blocks | **52.6** | 47.5 | 42.5 | 32.6 |

Run `cargo bench -p nanomp3-parity` to reproduce.

## Testing

`parity/` builds upstream minimp3 (a git submodule) in several configurations and compares it against nanomp3.

```sh
git submodule update --init
cargo test -p nanomp3-parity --release     # bit-exact comparison on the conformance vectors
cargo bench -p nanomp3-parity              # nanomp3 vs. minimp3
cd fuzz && cargo +nightly fuzz run differential   # random input must decode identically
cd fuzz && cargo +nightly fuzz run decode         # random input must not panic or stall
```
