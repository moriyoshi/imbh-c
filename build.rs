//! Regenerate `include/imbh.h` from the `extern "C"` surface on every build.
//!
//! The committed header is the source of truth for C/C++ consumers; the `header_up_to_date` test
//! (tests/roundtrip.rs) fails if it drifts from a fresh generation.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=build.rs");

    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out = Path::new(&crate_dir).join("include").join("imbh.h");

    match cbindgen::generate(&crate_dir) {
        Ok(bindings) => {
            bindings.write_to_file(&out);
        }
        // Don't fail the build if cbindgen can't run (e.g. a transient parse issue); the header check
        // test still guards correctness in CI.
        Err(e) => println!("cargo:warning=cbindgen header generation failed: {e}"),
    }
}
