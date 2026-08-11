use std::{env, path::PathBuf};

const LIBJPEG_INCLUDE_DIR: &str = "FLPDF_LIBJPEG_INCLUDE_DIR";
const LIBJPEG_LIB_DIR: &str = "FLPDF_LIBJPEG_LIB_DIR";

fn main() {
    println!("cargo:rerun-if-changed=csrc/jpeg_compat.h");
    println!("cargo:rerun-if-changed=csrc/jpeg_compat.c");
    println!("cargo:rerun-if-env-changed={LIBJPEG_INCLUDE_DIR}");
    println!("cargo:rerun-if-env-changed={LIBJPEG_LIB_DIR}");

    if env::var_os("CARGO_FEATURE_SYSTEM_LIBJPEG").is_none() {
        return;
    }

    println!("cargo:warning=flpdf-libjpeg-compat requires system libjpeg (jpeglib.h and -ljpeg); no vendored fallback is used");

    let include_dir = env::var_os(LIBJPEG_INCLUDE_DIR).map(PathBuf::from);
    if let Some(directory) = &include_dir {
        if !directory.is_dir() {
            panic!(
                "{LIBJPEG_INCLUDE_DIR}={} is not a directory containing jpeglib.h; install the system libjpeg prerequisite or unset the override (no vendored fallback)",
                directory.display()
            );
        }
        if !directory.join("jpeglib.h").is_file() {
            panic!(
                "{LIBJPEG_INCLUDE_DIR}={} does not contain jpeglib.h; install the system libjpeg prerequisite (no vendored fallback)",
                directory.display()
            );
        }
    }

    let lib_dir = env::var_os(LIBJPEG_LIB_DIR).map(PathBuf::from);
    if let Some(directory) = &lib_dir {
        if !directory.is_dir() {
            panic!(
                "{LIBJPEG_LIB_DIR}={} is not a directory containing libjpeg; install the system libjpeg prerequisite or unset the override (no vendored fallback)",
                directory.display()
            );
        }
    }

    let mut build = cc::Build::new();
    build
        .file("csrc/jpeg_compat.c")
        .include("csrc")
        .warnings(true);
    if let Some(directory) = &include_dir {
        build.include(directory);
    }
    if let Some(directory) = &lib_dir {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    build.compile("flpdf_jpeg_compat");
    println!("cargo:rustc-link-lib=jpeg");
}
