//! Export the zolo_rt_* symbols from this crate's TEST binaries, so the
//! integration test can exercise the plugin-side lazy shim end-to-end (the
//! test bin plays the role of a plugin-hosting executable). Real hosts emit
//! the same args from their own build.rs via zolo-alloc-exports.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    zolo_alloc_exports::emit("rustc-link-arg-tests");
}
