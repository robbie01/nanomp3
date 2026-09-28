//! Does upstream's SIMD build agree bit-for-bit with its scalar build?
use nanomp3_parity::*;
fn main() {
    let mut same = 0;
    let mut differ = Vec::new();
    for e in std::fs::read_dir(vectors_dir()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|e| e != "bit") { continue; }
        let data = std::fs::read(&p).unwrap();
        let a = decode_c::<f32>(&data, Feed::Whole, Flavor::Mp3F32);
        let b = decode_c::<f32>(&data, Feed::Whole, Flavor::SimdF32);
        match first_difference(&a, &b) {
            None => same += 1,
            Some(d) => differ.push(format!("{}: {d}", p.file_name().unwrap().to_string_lossy())),
        }
    }
    println!("{same} identical, {} differ", differ.len());
    for d in differ.iter().take(5) { println!("  {d}"); }
}
