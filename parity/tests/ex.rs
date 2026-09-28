//! nanomp3's minimp3_ex port, in minimp3-compat mode, must behave exactly like
//! minimp3_ex: same detection, frames, decoded buffers, reader output after
//! any sequence of reads and seeks, and the same internal counters.

use nanomp3_parity::ex::*;
use nanomp3_parity::vectors_dir;

fn inputs() -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(vectors_dir())
        .expect("vectors missing: run `git submodule update --init`")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bit"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    v.push(("march".into(), std::fs::read(root.join("../nanomp3-core/src/tests/The Washington Post.mp3")).unwrap()));
    // Edge cases: empty, tiny, and a tag with nothing after it.
    v.push(("empty".into(), Vec::new()));
    v.push(("tiny".into(), vec![0xff, 0xfb, 0x90]));
    v.push(("id3-only".into(), b"ID3\x04\x00\x00\x00\x00\x00\x10".iter().copied().chain([0; 16]).collect()));
    v.extend(quirk_inputs());
    v
}

/// Inputs that trigger the minimp3_ex bugs nanomp3 fixes by default.
fn quirk_inputs() -> Vec<(String, Vec<u8>)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let march = std::fs::read(root.join("../nanomp3-core/src/tests/The Washington Post.mp3")).unwrap();
    let audio = &march[nanomp3::id3v2_len(&march)..];
    let mut v = Vec::new();

    // Junk longer than the I/O buffer between two halves of the stream.
    let mut junk = audio[..audio.len() / 2].to_vec();
    junk.extend(std::iter::repeat_n(0u8, 200 * 1024));
    junk.extend_from_slice(&audio[audio.len() / 2..]);
    v.push(("quirk-junk".into(), junk));

    // Audio whose last 128 bytes happen to start with "TAG", then a real ID3v1
    // tag: stripping twice eats the end of the audio.
    let mut fake = audio.to_vec();
    let n = fake.len();
    fake[n - 128..n - 125].copy_from_slice(b"TAG");
    fake.extend_from_slice(b"TAG");
    fake.extend(std::iter::repeat_n(b' ', 125));
    v.push(("quirk-fake-tag".into(), fake));

    // A header-less APEv2 tag.
    let mut ape = audio.to_vec();
    ape.extend_from_slice(&[1; 20]);
    ape.extend_from_slice(b"APETAGEX");
    ape.extend_from_slice(&2000u32.to_le_bytes());
    ape.extend_from_slice(&(20u32 + 32).to_le_bytes());
    ape.extend_from_slice(&[0; 16]);
    v.push(("quirk-ape-no-header".into(), ape));
    v
}

fn check(what: &str, mut f: impl FnMut(&str, &[u8]) -> Option<String>) {
    let mut failures = Vec::new();
    for (name, data) in inputs() {
        if let Some(msg) = f(&name, &data) {
            failures.push(format!("{name}: {msg}"));
        }
    }
    assert!(failures.is_empty(), "{what}: {} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn detect() {
    check("detect", |_, data| {
        for io in [false, true] {
            let (c, r) = (c_detect(data, io), rust_detect(data, io, true));
            if c != r {
                return Some(format!("io={io}: C {c}, Rust {r}"));
            }
        }
        None
    });
}

#[test]
fn iterate() {
    check("iterate", |_, data| {
        for io in [false, true] {
            let (c, r) = (c_iterate(data, io), rust_iterate(data, io, true));
            if c != r {
                let i = c.iter().zip(&r).position(|(a, b)| a != b).unwrap_or(c.len().min(r.len()));
                return Some(format!(
                    "io={io}: {} vs {} frames, first difference at {i}: C {:?} Rust {:?}",
                    c.len(),
                    r.len(),
                    c.get(i),
                    r.get(i)
                ));
            }
        }
        None
    });
}

#[test]
fn load() {
    check("load", |_, data| {
        for io in [false, true] {
            for transition in [false, true] {
                let (c, r) = (c_load(data, io, transition), rust_load(data, io, transition, true));
                if c != r {
                    let i = c.1.iter().zip(&r.1).position(|(a, b)| a != b);
                    return Some(format!(
                        "io={io} transition={transition}: C (ret {}, {} samples, {:?}) vs Rust (ret {}, {} samples, {:?}); first sample difference {i:?}",
                        c.0,
                        c.1.len(),
                        (c.2, c.3, c.4, c.5),
                        r.0,
                        r.1.len(),
                        (r.2, r.3, r.4, r.5)
                    ));
                }
            }
        }
        None
    });
}

fn check_reader(seek_to_byte: bool, do_not_scan: bool, transition: bool) {
    check("reader", |_, data| {
        for io in [false, true] {
            let cfg = ExConfig { io, seek_to_byte, do_not_scan, transition };
            // Size the seeks from what C reports after opening.
            let total = c_ex(data, cfg, &[])[0].2[0];
            // minimp3's idea of where the audio ends (it differs for header-less APE tags).
            let audio_end = nanomp3::__compat::strip_trailing_tags(data).len() as u64;
            let ops = standard_ops(total, audio_end, seek_to_byte);
            let (c, r) = (c_ex(data, cfg, &ops), rust_ex(data, cfg, &ops, true));
            if let Some(i) = (0..c.len().max(r.len())).find(|&i| c.get(i) != r.get(i)) {
                let op = if i == 0 { "open".to_string() } else { format!("{:?}", ops[i - 1]) };
                let show = |e: Option<&ExEvent>| e.map(|(ret, pcm, st)| format!("ret {ret}, {} samples, state {st:?}", pcm.len()));
                return Some(format!("{cfg:?} event {i} ({op}): C {:?} Rust {:?}", show(c.get(i)), show(r.get(i))));
            }
        }
        None
    });
}

#[test]
fn reader_seek_to_sample() {
    check_reader(false, false, false);
}

#[test]
fn reader_seek_to_sample_lazy_index() {
    check_reader(false, true, false);
}

#[test]
fn reader_seek_to_byte() {
    check_reader(true, false, false);
}

#[test]
fn reader_mono_stereo_transition() {
    check_reader(false, false, true);
}

// Default mode: minimp3_ex's bugs are discrepancies between its memory and I/O
// paths (plus header-less APE tags), so the defaults are pinned down by
// matching minimp3_ex in memory, and matching memory mode for I/O.

/// (do_not_scan, transition) reader configurations with sample seeks.
const DEFAULT_CONFIGS: [(bool, bool); 3] = [(false, false), (true, false), (false, true)];

#[test]
fn default_mode_matches_minimp3_in_memory() {
    check("default memory mode", |name, data| {
        if name == "quirk-ape-no-header" {
            return None; // the tag-trimming fix applies here
        }
        if c_detect(data, false) != rust_detect(data, false, false) {
            return Some("detect".into());
        }
        if c_iterate(data, false) != rust_iterate(data, false, false) {
            return Some("iterate".into());
        }
        if c_load(data, false, false) != rust_load(data, false, false, false) {
            return Some("load".into());
        }
        for (do_not_scan, transition) in DEFAULT_CONFIGS {
            let cfg = ExConfig { io: false, seek_to_byte: false, do_not_scan, transition };
            let ops = standard_ops(c_ex(data, cfg, &[])[0].2[0], 0, false);
            if c_ex(data, cfg, &ops) != rust_ex(data, cfg, &ops, false) {
                return Some(format!("reader {cfg:?}"));
            }
        }
        None
    });
}

#[test]
fn default_mode_io_matches_memory() {
    check("default I/O vs memory", |_, data| {
        // buf_size depends on the read buffer, so leave it out.
        let strip = |v: Vec<IterFrame>| v.into_iter().map(|f| (f.0, f.1, f.2, f.4, f.5, f.6, f.7)).collect::<Vec<_>>();
        if strip(rust_iterate(data, true, false)) != strip(rust_iterate(data, false, false)) {
            return Some("iterate".into());
        }
        if rust_load(data, true, false, false) != rust_load(data, false, false, false) {
            return Some("load".into());
        }
        for (do_not_scan, transition) in DEFAULT_CONFIGS {
            let mem = ExConfig { io: false, seek_to_byte: false, do_not_scan, transition };
            let io = ExConfig { io: true, ..mem };
            let ops = standard_ops(rust_ex(data, mem, &[], false)[0].2[0], 0, false);
            if rust_ex(data, io, &ops, false) != rust_ex(data, mem, &ops, false) {
                return Some(format!("reader {mem:?}"));
            }
        }
        None
    });
}

#[test]
fn quirk_inputs_trigger_the_quirks() {
    // Guard against the synthetic inputs silently no longer exercising the
    // bugs: minimp3 (and compat mode) must disagree between memory and I/O.
    for (name, data) in quirk_inputs() {
        let differs = match name.as_str() {
            "quirk-ape-no-header" => rust_load(&data, false, false, true) != rust_load(&data, false, false, false),
            // Only the reader re-strips tags at the end of the stream.
            "quirk-fake-tag" => {
                let mem = ExConfig { io: false, seek_to_byte: false, do_not_scan: false, transition: false };
                let io = ExConfig { io: true, ..mem };
                let ops = vec![ExOp::Read(1 << 20); 16];
                c_ex(&data, io, &ops) != c_ex(&data, mem, &ops)
            }
            _ => c_iterate(&data, true).len() != c_iterate(&data, false).len(),
        };
        assert!(differs, "{name} no longer triggers its quirk");
    }
}
