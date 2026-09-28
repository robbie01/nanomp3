//! The minimp3_ex port under random input and random read/seek sequences:
//! in compat mode it must match minimp3_ex exactly, and by default the I/O
//! reader must match the in-memory one.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nanomp3_parity::ex::*;

/// libFuzzer on Windows doesn't write a crash file for Rust panics; save the
/// input before failing.
fn check<T: PartialEq + std::fmt::Debug>(input: &[u8], a: T, b: T, what: &str) {
    if a != b {
        let dir = std::env::var("NANOMP3_FUZZ_ARTIFACTS").unwrap_or_else(|_| ".".into());
        let hash = input.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
        let _ = std::fs::write(format!("{dir}/mismatch-{hash:016x}"), input);
        panic!("{what}");
    }
}

fuzz_target!(|input: &[u8]| {
    // Layout: [config][8 ops x 3 bytes][data...]
    if input.len() < 25 {
        return;
    }
    let (head, data) = input.split_at(25);
    let cfg_bits = head[0];
    let seek_to_byte = cfg_bits & 2 != 0;
    let cfg = ExConfig { io: cfg_bits & 1 != 0, seek_to_byte, do_not_scan: cfg_bits & 4 != 0, transition: cfg_bits & 8 != 0 };
    // Byte seeks past the audio make minimp3's memory reader read out of bounds.
    let audio_end = nanomp3::__compat::strip_trailing_tags(data).len() as u64;
    let mut ops: Vec<ExOp> = head[1..]
        .as_chunks::<3>()
        .0
        .iter()
        .map(|op| {
            let arg = u16::from_le_bytes([op[1], op[2]]) as u64;
            match op[0] % 3 {
                0 => ExOp::Read(arg as usize * 16),
                1 => ExOp::ReadFrame(arg as usize),
                _ if seek_to_byte => ExOp::Seek((arg * 4).min(audio_end)),
                _ => ExOp::Seek(arg * 16 + u64::from(op[0] >> 4)),
            }
        })
        .collect();
    ops.push(ExOp::Read(1 << 20));

    let c = c_ex(data, cfg, &ops);
    let r = rust_ex(data, cfg, &ops, true);
    check(input, &c, &r, &format!("compat reader differs from minimp3_ex ({cfg:?}, {ops:?})"));

    if cfg_bits & 16 != 0 {
        check(input, c_load(data, cfg.io, cfg.transition), rust_load(data, cfg.io, cfg.transition, true), "load");
        check(input, c_iterate(data, cfg.io), rust_iterate(data, cfg.io, true), "iterate");
        check(input, c_detect(data, cfg.io), rust_detect(data, cfg.io, true), "detect");
    }

    if !seek_to_byte {
        let mem = ExConfig { io: false, ..cfg };
        let io = ExConfig { io: true, ..cfg };
        check(input, rust_ex(data, io, &ops, false), rust_ex(data, mem, &ops, false), "default I/O differs from memory");
    }
});
