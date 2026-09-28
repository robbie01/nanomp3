//! Builds several configurations of upstream minimp3 from the submodule.

/// (name, extra defines). Every build gets MINIMP3_IMPLEMENTATION.
const BUILDS: &[(&str, &[&str])] = &[
    // The configuration nanomp3 was originally translated from.
    ("mp3_f32", &["MINIMP3_ONLY_MP3", "MINIMP3_FLOAT_OUTPUT", "MINIMP3_NO_SIMD"]),
    ("mp3_s16", &["MINIMP3_ONLY_MP3", "MINIMP3_NO_SIMD"]),
    ("full_f32", &["MINIMP3_FLOAT_OUTPUT", "MINIMP3_NO_SIMD"]),
    ("full_s16", &["MINIMP3_NO_SIMD"]),
    // Upstream defaults with SSE/NEON intrinsics; for benchmarks, not bit-exact.
    ("simd_f32", &["MINIMP3_ONLY_MP3", "MINIMP3_FLOAT_OUTPUT"]),
];

fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let header = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("minimp3/minimp3.h");
    println!("cargo:rerun-if-changed={}", header.display());
    for (name, defines) in BUILDS {
        let mut src = String::new();
        for d in defines.iter().chain(&["MINIMP3_IMPLEMENTATION"]) {
            src += &format!("#define {d}\n");
        }
        for f in ["mp3dec_init", "mp3dec_decode_frame", "mp3dec_f32_to_s16"] {
            src += &format!("#define {f} {f}_{name}\n");
        }
        src += &format!("#include \"{}\"\n", header.display().to_string().replace('\\', "/"));
        src += &format!("unsigned long mp3dec_sizeof_{name}(void) {{ return sizeof(mp3dec_t); }}\n");
        let file = out.join(format!("minimp3_{name}.c"));
        std::fs::write(&file, src).unwrap();
        cc::Build::new()
            .file(&file)
            // Rust never fuses multiply-adds; keep C from doing it either so
            // the scalar builds are bit-exact references on every target.
            .flag_if_supported("-ffp-contract=off")
            // minimp3 leaves its per-frame scratch uninitialized and, on corrupt
            // streams, reads from it. nanomp3 zeroes that memory, so zero it in
            // C too to keep differential fuzzing deterministic.
            .flag_if_supported("-ftrivial-auto-var-init=zero")
            .flag_if_supported("/clang:-ftrivial-auto-var-init=zero")
            .opt_level(3)
            .warnings(false)
            .compile(&format!("minimp3_{name}"));
    }
}
