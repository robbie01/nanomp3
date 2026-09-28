# nanomp3-core

The frame decoder behind [nanomp3](https://crates.io/crates/nanomp3): a `no_std`, allocation-free, `#![forbid(unsafe_code)]` Rust port of [minimp3](https://github.com/lieff/minimp3) whose output is bit-identical to upstream's.

Most users want `nanomp3`, which re-exports everything here and adds tag skipping, VBR tag parsing (duration, gapless playback), frame iteration, and a sample-accurate seekable reader. Use this crate directly if you only need the frame decoder.
