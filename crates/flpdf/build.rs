use std::env;

fn main() {
    println!("cargo:rerun-if-changed=src/jpeg_compat.h");
    println!("cargo:rerun-if-changed=src/jpeg_compat.c");

    if env::var_os("CARGO_FEATURE_QPDF_LIBJPEG_COMPAT").is_none() {
        return;
    }

    cc::Build::new()
        .file("src/jpeg_compat.c")
        .include("src")
        .warnings(true)
        .compile("flpdf_jpeg_compat");
    println!("cargo:rustc-link-lib=jpeg");
}
