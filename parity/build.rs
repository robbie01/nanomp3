fn main() {
    for name in ["scalar", "simd"] {
        let src = format!("c/minimp3_{name}.c");
        println!("cargo:rerun-if-changed={src}");
        cc::Build::new()
            .file(&src)
            // Rust never fuses multiply-adds; keep C from doing it either so
            // the scalar build is a bit-exact reference on every target.
            .flag_if_supported("-ffp-contract=off")
            .opt_level(3)
            .warnings(false)
            .compile(&format!("minimp3_{name}"));
    }
    println!("cargo:rerun-if-changed=minimp3/minimp3.h");
}
