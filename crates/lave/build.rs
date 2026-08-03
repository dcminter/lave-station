//! Compiles the UI and CSS into a `GResource`, and the `GSettings` schema into a
//! directory the binary can fall back to.
//!
//! A schema normally has to be installed under `/usr/share/glib-2.0/schemas` before the
//! application will start at all. Compiling it here as well means an uninstalled build
//! run straight out of `cargo run` still has somewhere to read its settings from; an
//! installed copy takes precedence, which is what a packaged build wants.

use std::path::{Path, PathBuf};

const SCHEMA: &str = "com.paperstack.LaveStation.gschema.xml";

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/lave.gresource.xml", "lave.gresource");
    compile_schemas();
}

fn compile_schemas() {
    println!("cargo:rerun-if-changed=data/{SCHEMA}");

    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        panic!("cargo did not set OUT_DIR");
    };
    let directory = out_dir.join("schemas");

    if let Err(error) = std::fs::create_dir_all(&directory) {
        panic!("could not create {}: {error}", directory.display());
    }
    let source = Path::new("data").join(SCHEMA);
    if let Err(error) = std::fs::copy(&source, directory.join(SCHEMA)) {
        panic!("could not stage {}: {error}", source.display());
    }

    // glib-compile-schemas validates as it goes, so a malformed schema fails the build
    // rather than the first run.
    let outcome = std::process::Command::new("glib-compile-schemas")
        .arg(&directory)
        .status();

    match outcome {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("glib-compile-schemas failed: {status}"),
        Err(error) => panic!(
            "could not run glib-compile-schemas ({error}). It ships with libglib2.0-dev \
             on Debian."
        ),
    }

    // Baked in at compile time: the binary needs no environment variable set to find it.
    println!("cargo:rustc-env=LAVE_SCHEMA_DIR={}", directory.display());
}
