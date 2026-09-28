# nanomp3

A pure Rust MPEG audio decoder (MP3, plus MP1/MP2): a safe, bit-exact port of [minimp3](https://github.com/lieff/minimp3) and its `minimp3_ex` helpers.

- **Easy to use.** A seekable reader over any `Read + Seek` (or bytes in memory) skips ID3/APE tags, trims the encoder delay and padding for gapless playback, knows the stream length, and seeks to exact samples.
- **No `unsafe`.** Both crates are `#![forbid(unsafe_code)]`.
- **Bit-exact with minimp3.** Output is identical, bit for bit, to upstream's reference build on every one of its conformance vectors, and the reader behaves exactly like `minimp3_ex`. CI checks this on x86-64 and ARM64. A handful of minimp3 bugs are fixed by default, with minimp3's exact behavior available as an opt-in; see below.
- **Faster than minimp3.** SIMD kernels (via [`wide`](https://crates.io/crates/wide)) are written so each lane does exactly the scalar arithmetic, so the speedup doesn't cost bit-exactness. Upstream's own SIMD build doesn't manage that.
- **`no_std`.** The frame decoder needs no allocator; the in-memory reader needs only `alloc`.
- **Flexible output.** Interleaved `f32`, `i8`, `u8`, `i16`, `u16`, `i32` or `u32` samples.

```rust
let file = std::fs::File::open("song.mp3")?;
let mut reader = nanomp3::Reader::<_, f32>::new(std::io::BufReader::new(file))?;
println!("{:?} samples per channel at {} Hz", reader.total_samples(), reader.sample_rate());

reader.seek(44_100)?; // one second in, at 44.1 kHz
let mut pcm = vec![0f32; 4096]; // or i16, u8, i32, ...
while let n @ 1.. = reader.read(&mut pcm)? {
    let interleaved = &pcm[..n];
    // ...
}
```

See `nanomp3/examples/measure` for a complete program.

## Crates

- **`nanomp3`**: the reader (`Reader`, `SliceReader`), whole-buffer decoding (`decode_all`), frame iteration, VBR tag parsing, format detection and tag skipping. It re-exports everything from `nanomp3-core`.
- **`nanomp3-core`**: the frame decoder alone. It's `no_std` with no allocator and no buffering: you feed it bytes and it decodes one frame at a time, so you keep a read-ahead buffer of several frames (16 KiB is plenty) yourself.

## minimp3 compatibility

nanomp3 reproduces minimp3's behavior, with one policy: where minimp3 does something that is clearly a bug, nanomp3 fixes it by default and keeps the original behavior available through `Options::minimp3_compat` (or `Decoder::new_minimp3_compat()`). Currently:

- `i16` output rounds samples in (−1.5, −0.5] to 0 instead of −1 (about 0.3% of samples in the conformance vectors).
- APEv2 tags without a header are over-trimmed by 32 bytes of audio.
- In I/O mode, minimp3_ex strips trailing tags again every time it reaches the end of the stream, ends its scan at junk longer than its read buffer, misreports how many bytes the decoder may read, and fails seeks near truncated frames. With the fixes, nanomp3's I/O reader behaves exactly like its in-memory one.

`f32` output from the frame decoder is identical either way.

## Features

| feature | crate | default | |
|---|---|---|---|
| `std` | `nanomp3` | yes | `Reader` and the other `*_reader` functions over `Read + Seek`. |
| `alloc` | `nanomp3` | via `std` | `SliceReader` and `decode_all`. |
| `wide` | both | yes | Explicit SIMD via `wide`. Without it, the same kernels run on plain arrays and rely on LLVM's auto-vectorizer (a few percent slower, no dependencies). |
| `layer12` | both | yes | Decode MPEG Layer I/II frames too. Without it they are skipped, like minimp3 built with `MINIMP3_ONLY_MP3`. |

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

`parity/` builds upstream minimp3 and minimp3_ex (a git submodule) in several configurations and compares them against nanomp3.

```sh
git submodule update --init
cargo test -p nanomp3-parity --release     # bit-exact comparisons on the conformance vectors
cargo bench -p nanomp3-parity              # nanomp3 vs. minimp3
cd fuzz && cargo +nightly fuzz run differential   # random input must decode identically
cd fuzz && cargo +nightly fuzz run ex             # random reads and seeks must match minimp3_ex
cd fuzz && cargo +nightly fuzz run decode         # random input must not panic or stall
```
