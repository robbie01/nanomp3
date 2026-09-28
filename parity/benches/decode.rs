use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nanomp3_parity::{CDecoder, Flavor};
use std::hint::black_box;

fn inputs() -> Vec<(&'static str, Vec<u8>)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        // Real music: MPEG-1, 48 kHz, joint stereo, 320 kbps.
        ("march_320k_stereo", std::fs::read(root.join("../nanomp3-core/src/tests/The Washington Post.mp3")).unwrap()),
        // MPEG-2 LSF, 22.05 kHz, all bitrates.
        ("lsf_22k", std::fs::read(root.join("minimp3/vectors/M2L3_bitrate_22_all.bit")).unwrap()),
        // Mono, short blocks, intensity stereo mix.
        ("he_mode", std::fs::read(root.join("minimp3/vectors/l3-he_mode.bit")).unwrap()),
    ]
}

macro_rules! decode_all {
    ($dec:expr, $data:expr, $pcm:expr, |$d:ident, $m:ident, $p:ident| $call:expr) => {{
        let mut input: &[u8] = $data;
        let mut total = 0usize;
        while !input.is_empty() {
            let ($d, $m, $p) = (&mut $dec, input, &mut $pcm[..]);
            let (consumed, samples) = $call;
            if consumed == 0 { break; }
            total += samples;
            input = &input[consumed.min(input.len())..];
        }
        total
    }};
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    for (name, data) in inputs() {
        group.throughput(Throughput::Bytes(data.len() as u64));
        let mut pcm = vec![0f32; nanomp3::MAX_SAMPLES_PER_FRAME];

        group.bench_with_input(BenchmarkId::new("nanomp3", name), &data, |b, data| {
            b.iter(|| {
                let mut dec = nanomp3::Decoder::new();
                black_box(decode_all!(dec, data, pcm, |d, m, p| {
                    let (n, i) = d.decode(m, p);
                    (n, i.map_or(0, |i| i.samples_produced))
                }))
            })
        });
        group.bench_with_input(BenchmarkId::new("baseline", name), &data, |b, data| {
            b.iter(|| {
                let mut dec = nanomp3_baseline::Decoder::new();
                black_box(decode_all!(dec, data, pcm, |d, m, p| {
                    let (n, i) = d.decode(m, p);
                    (n, i.map_or(0, |i| i.samples_produced))
                }))
            })
        });
        for (label, flavor) in [("c_scalar", Flavor::Mp3F32), ("c_simd", Flavor::SimdF32)] {
            group.bench_with_input(BenchmarkId::new(label, name), &data, |b, data| {
                b.iter(|| {
                    let mut dec = CDecoder::new(flavor);
                    black_box(decode_all!(dec, data, pcm, |d, m, p| {
                        let (s, info) = d.decode::<f32>(m, p);
                        (info.frame_bytes as usize, s)
                    }))
                })
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
