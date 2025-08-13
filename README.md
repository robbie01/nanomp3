# nanomp3

A pure Rust MP3 decoding library based on a c2rust translation of [minimp3](https://github.com/lieff/minimp3). `no_std` compatible.

⚠️ minimp3 is somewhat arcane to use and requires maintaining a read-ahead buffer. See `examples/measure` for example usage.
