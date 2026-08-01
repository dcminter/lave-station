//! Compiles UI and CSS into a `GResource` bundled in the binary.

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/lave.gresource.xml", "lave.gresource");
}
