//! Replays an ex fuzz input and shows where C and Rust diverge: `ex_repro <file>`.
use nanomp3_parity::ex::*;
fn main() {
    let input = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let (head, data) = input.split_at(25);
    let b = head[0];
    let seek_to_byte = b & 2 != 0;
    let cfg = ExConfig { io: b & 1 != 0, seek_to_byte, do_not_scan: b & 4 != 0, transition: b & 8 != 0 };
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
    println!("{cfg:?}\n{ops:?}");
    let (c, r) = (c_ex(data, cfg, &ops), rust_ex(data, cfg, &ops, true));
    for i in 0..c.len().max(r.len()) {
        let show = |e: Option<&ExEvent>| e.map(|(ret, pcm, st)| format!("ret {ret:7} n {:7} st {st:?}", pcm.len())).unwrap_or_default();
        let mark = if c.get(i) == r.get(i) { " " } else { "*" };
        println!("{mark}{i:2} C    {}\n    Rust {}", show(c.get(i)), show(r.get(i)));
    }
}
