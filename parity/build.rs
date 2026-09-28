//! Builds several configurations of upstream minimp3 (and minimp3_ex) from the
//! submodule, each with its symbols suffixed so they can coexist.

/// (name, extra defines) for the frame decoder alone.
const BUILDS: &[(&str, &[&str])] = &[
    // The configuration nanomp3 was originally translated from.
    ("mp3_f32", &["MINIMP3_ONLY_MP3", "MINIMP3_FLOAT_OUTPUT", "MINIMP3_NO_SIMD"]),
    ("mp3_s16", &["MINIMP3_ONLY_MP3", "MINIMP3_NO_SIMD"]),
    ("full_f32", &["MINIMP3_FLOAT_OUTPUT", "MINIMP3_NO_SIMD"]),
    ("full_s16", &["MINIMP3_NO_SIMD"]),
    // Upstream defaults with SSE/NEON intrinsics; for benchmarks, not bit-exact.
    ("simd_f32", &["MINIMP3_ONLY_MP3", "MINIMP3_FLOAT_OUTPUT"]),
];

/// (name, extra defines) for minimp3_ex builds. Layer I/II support and mono/stereo
/// transitions are compile-time options upstream.
const EX_BUILDS: &[(&str, &[&str])] = &[
    ("ex_mp3", &["MINIMP3_ONLY_MP3"]),
    ("ex_mp3_trans", &["MINIMP3_ONLY_MP3", "MINIMP3_ALLOW_MONO_STEREO_TRANSITION"]),
    ("ex_full", &[]),
    ("ex_full_trans", &["MINIMP3_ALLOW_MONO_STEREO_TRANSITION"]),
];

const CORE_FNS: &[&str] = &["mp3dec_init", "mp3dec_decode_frame", "mp3dec_f32_to_s16"];
const EX_FNS: &[&str] = &[
    "mp3dec_detect_buf",
    "mp3dec_detect_cb",
    "mp3dec_load_buf",
    "mp3dec_load_cb",
    "mp3dec_iterate_buf",
    "mp3dec_iterate_cb",
    "mp3dec_ex_open_buf",
    "mp3dec_ex_open_cb",
    "mp3dec_ex_close",
    "mp3dec_ex_seek",
    "mp3dec_ex_read_frame",
    "mp3dec_ex_read",
];

/// Small helpers so Rust doesn't need to mirror minimp3_ex's struct layouts.
const EX_SHIMS: &str = r#"
mp3dec_ex_t *shim_ex_new_NAME(void) { return (mp3dec_ex_t *)calloc(1, sizeof(mp3dec_ex_t)); }
void shim_ex_free_NAME(mp3dec_ex_t *d) { mp3dec_ex_close(d); free(d); }
void shim_ex_state_NAME(const mp3dec_ex_t *d, uint64_t *o)
{
    o[0] = d->samples; o[1] = d->detected_samples; o[2] = d->cur_sample; o[3] = d->vbr_tag_found;
    o[4] = (uint64_t)(int64_t)d->last_error; o[5] = d->info.channels; o[6] = d->info.hz; o[7] = d->info.layer;
}
static void shim_meta(const mp3dec_file_info_t *info, int *meta)
{
    meta[0] = info->channels; meta[1] = info->hz; meta[2] = info->layer; meta[3] = info->avg_bitrate_kbps;
}
int shim_load_buf_NAME(const uint8_t *buf, size_t size, float **pcm, size_t *samples, int *meta)
{
    mp3dec_t dec; mp3dec_file_info_t info;
    int r = mp3dec_load_buf(&dec, buf, size, &info, 0, 0);
    *pcm = info.buffer; *samples = info.samples; shim_meta(&info, meta);
    return r;
}
int shim_load_cb_NAME(mp3dec_io_t *io, float **pcm, size_t *samples, int *meta)
{
    mp3dec_t dec; mp3dec_file_info_t info;
    uint8_t *buf = (uint8_t *)malloc(MINIMP3_IO_SIZE);
    int r = mp3dec_load_cb(&dec, io, buf, MINIMP3_IO_SIZE, &info, 0, 0);
    free(buf);
    *pcm = info.buffer; *samples = info.samples; shim_meta(&info, meta);
    return r;
}
int shim_iterate_cb_NAME(mp3dec_io_t *io, MP3D_ITERATE_CB cb, void *user)
{
    uint8_t *buf = (uint8_t *)malloc(MINIMP3_IO_SIZE);
    int r = mp3dec_iterate_cb(io, buf, MINIMP3_IO_SIZE, cb, user);
    free(buf);
    return r;
}
int shim_detect_cb_NAME(mp3dec_io_t *io)
{
    uint8_t *buf = (uint8_t *)malloc(MINIMP3_BUF_SIZE);
    int r = mp3dec_detect_cb(io, buf, MINIMP3_BUF_SIZE);
    free(buf);
    return r;
}
void shim_free_NAME(void *p) { free(p); }
"#;

/// minimp3 reads uninitialized locals on some inputs (e.g. `mp3dec_detect_*`
/// passes an uninitialized `free_format_bytes` to `mp3d_find_frame`, so free-
/// format streams read out of bounds). The reference must zero them, which
/// MSVC's cl.exe can't do; on MSVC targets use clang-cl instead when it's
/// available (it ships with Visual Studio and the GitHub Windows runners).
///
/// This sets `CC_<target>` rather than calling `cc::Build::compiler`, so cc
/// still sets up the MSVC include and library paths for it.
fn prefer_clang_cl_on_msvc() {
    let target = std::env::var("TARGET").unwrap();
    let var = format!("CC_{}", target.replace('-', "_"));
    if !target.contains("msvc") || std::env::var_os(&var).is_some() || std::env::var_os("CC").is_some() {
        return;
    }
    let found = [r"C:\Program Files\LLVM\bin\clang-cl.exe", "clang-cl.exe"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()));
    match found {
        // Build scripts are single-threaded.
        Some(clang_cl) => std::env::set_var(var, clang_cl),
        None => println!(
            "cargo:warning=clang-cl not found: building the minimp3 reference with cl.exe, which can't \
             zero-initialize locals; minimp3_ex comparisons may crash or fail nondeterministically"
        ),
    }
}

fn compile(out: &std::path::Path, name: &str, src: String) {
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

fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("minimp3");
    let path = |f: &str| dir.join(f).display().to_string().replace('\\', "/");
    println!("cargo:rerun-if-changed={}", path("minimp3.h"));
    println!("cargo:rerun-if-changed={}", path("minimp3_ex.h"));
    prefer_clang_cl_on_msvc();

    for (name, defines) in BUILDS {
        let mut src = String::new();
        for d in defines.iter().chain(&["MINIMP3_IMPLEMENTATION"]) {
            src += &format!("#define {d}\n");
        }
        for f in CORE_FNS {
            src += &format!("#define {f} {f}_{name}\n");
        }
        src += &format!("#include \"{}\"\n", path("minimp3.h"));
        src += &format!("unsigned long mp3dec_sizeof_{name}(void) {{ return sizeof(mp3dec_t); }}\n");
        compile(&out, name, src);
    }

    for (name, defines) in EX_BUILDS {
        let mut src = String::from("#include <stdlib.h>\n#include <stdint.h>\n");
        // minimp3_ex reads stale bytes of its malloc'd buffers on some inputs,
        // and past their end when it overstates the bytes available (see
        // nanomp3's io.rs). Zeroed memory with slack makes that deterministic
        // and matches nanomp3's compat mode.
        src += "#define malloc(n) calloc(2, (n))\n";
        let common = ["MINIMP3_IMPLEMENTATION", "MINIMP3_FLOAT_OUTPUT", "MINIMP3_NO_SIMD", "MINIMP3_NO_STDIO"];
        for d in defines.iter().chain(&common) {
            src += &format!("#define {d}\n");
        }
        for f in CORE_FNS.iter().chain(EX_FNS) {
            src += &format!("#define {f} {f}_{name}\n");
        }
        src += &format!("#include \"{}\"\n", path("minimp3_ex.h"));
        src += &EX_SHIMS.replace("NAME", name);
        compile(&out, name, src);
    }
}
