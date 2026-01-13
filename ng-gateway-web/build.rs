//! Build script for `ng-gateway-web`.
//!
//! This crate supports an optional `ui-embedded` feature which embeds a pre-built
//! `ui-dist.zip` into the binary.
//!
//! In normal development workflows, `ui-dist.zip` may not exist until you run
//! `cargo xtask build` (UI build is ON by default; use `--without-ui` to skip).
//!
//! To keep `cargo check/clippy --all-features` working in all environments,
//! we always create `${OUT_DIR}/ui-dist.zip`:
//! - If `${CARGO_MANIFEST_DIR}/ui-dist.zip` exists, we copy it into OUT_DIR.
//! - Otherwise we create an empty placeholder file in OUT_DIR.
//!
//! Runtime behavior:
//! - If the placeholder is embedded (feature enabled without a real zip),
//!   the server will return a clear error when trying to load embedded assets.

use std::{env, fs, path::Path};

fn main() {
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(v) => v,
        Err(e) => {
            panic!("Missing CARGO_MANIFEST_DIR in build script env: {e}");
        }
    };
    let out_dir = match env::var("OUT_DIR") {
        Ok(v) => v,
        Err(e) => {
            panic!("Missing OUT_DIR in build script env: {e}");
        }
    };

    let src_zip = Path::new(&manifest_dir).join("ui-dist.zip");
    let out_zip = Path::new(&out_dir).join("ui-dist.zip");

    // Re-run when the source zip changes (if present).
    println!("cargo:rerun-if-changed={}", src_zip.display());

    if src_zip.exists() {
        if let Err(e) = fs::copy(&src_zip, &out_zip) {
            panic!(
                "Failed to copy ui-dist.zip from {} to {}: {e}",
                src_zip.display(),
                out_zip.display()
            );
        }
    } else {
        // Create an empty placeholder so `include_bytes!` can always compile.
        // If the `ui-embedded` feature is enabled without a real zip, runtime
        // loading will fail with a clear error message.
        if let Err(e) = fs::write(&out_zip, []) {
            panic!(
                "Failed to create placeholder ui-dist.zip at {}: {e}",
                out_zip.display()
            );
        }
    }
}
