use nanomp3_parity::*;

fn inputs() -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(vectors_dir())
        .expect("vectors missing: run `git submodule update --init`")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bit"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    let march = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/tests/The Washington Post.mp3");
    v.push(("The Washington Post.mp3".into(), std::fs::read(march).unwrap()));
    v
}

fn check<S: PcmSample>(feed: Feed) {
    let mut failures = Vec::new();
    let mut frames = 0;
    for (name, data) in inputs() {
        let c = decode_c::<S>(&data, feed, Flavor::reference::<S>());
        let r = std::panic::catch_unwind(|| decode_rust::<S>(&data, feed));
        match r {
            Err(_) => failures.push(format!("{name}: nanomp3 panicked")),
            Ok(r) => {
                frames += r.iter().filter(|f| f.info.is_some()).count();
                if let Some(d) = first_difference(&c, &r) {
                    failures.push(format!("{name}: {d}"));
                }
            }
        }
    }
    eprintln!("{frames} audio frames compared");
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn bit_exact_whole_buffer() {
    check::<f32>(Feed::Whole);
}

#[test]
fn bit_exact_16k_window() {
    check::<f32>(Feed::Window(16 * 1024));
}

#[test]
fn bit_exact_tiny_window() {
    // Smaller than a frame: exercises resync and the "not enough data" paths.
    check::<f32>(Feed::Window(700));
}

#[test]
fn bit_exact_i16_whole_buffer() {
    check::<i16>(Feed::Whole);
}

#[test]
fn bit_exact_i16_tiny_window() {
    check::<i16>(Feed::Window(700));
}
