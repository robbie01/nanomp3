//! Print a per-frame comparison of C minimp3 vs nanomp3 for one file.
use nanomp3_parity::*;
fn main() {
    let path = std::env::args().nth(1).expect("usage: diff <file>");
    let data = std::fs::read(path).unwrap();
    let c = decode_reference::<f32>(&data, Feed::Whole, false);
    let r = decode_rust::<f32>(&data, Feed::Whole, false);
    let mut pos = 0;
    for (i, (a, b)) in c.iter().zip(&r).enumerate() {
        let maxdiff = a.pcm.iter().zip(&b.pcm).map(|(x, y)| (f32::from_bits(*x) - f32::from_bits(*y)).abs()).fold(0f32, f32::max);
        let hdr = data.get(pos..pos + 4).map(|h| format!("{:02x?}", h)).unwrap_or_default();
        println!("{i:4} @{pos:6} hdr {hdr} C {:?}/{} R {:?}/{} maxdiff {maxdiff:e}", a.info, a.consumed, b.info, b.consumed);
        pos += a.consumed;
        if i > 14 { break; }
    }
}
