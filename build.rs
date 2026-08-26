//! Generates the vendored protobuf messages, and stamps the Windows
//! executable with its icon and its name.
//!
//! Without the stamp the app is a nameless default-icon binary in the
//! taskbar, in Explorer and in whatever shortcut an installer makes for
//! it. The icon comes from `assets/AppIcon.ico`, the same artwork the
//! macOS bundle carries as `.icns`.

use std::{fs, path::Path};

/// The messages Cadence speaks that librespot-protocol ships but does not
/// compile. Both are copied unchanged from librespot-protocol's `proto`
/// directory; see `docs/adr/0006-library-order-and-pins-from-the-internal-protocol.md`.
const PROTO_FILES: [&str; 2] = ["collection2v2.proto", "recently_played_backend.proto"];

fn generate_protobuf() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let proto_dir = Path::new(&manifest_dir).join("proto");
    let out_dir = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("protos");
    // Codegen writes a `mod.rs` listing what it generated, so the directory
    // is cleared first: a proto removed from the list must not linger.
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("could not create the protobuf output directory");

    let inputs: Vec<_> = PROTO_FILES
        .iter()
        .map(|file| proto_dir.join(file))
        .collect();
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    protobuf_codegen::Codegen::new()
        .pure()
        .out_dir(&out_dir)
        .inputs(&inputs)
        .include(&proto_dir)
        .run()
        .expect("could not generate the vendored protobuf messages");
}

fn main() -> std::io::Result<()> {
    generate_protobuf();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    println!("cargo:rerun-if-changed=assets/AppIcon.ico");
    winresource::WindowsResource::new()
        .set_icon("assets/AppIcon.ico")
        .set("ProductName", "Cadence")
        .set("FileDescription", "Cadence")
        .set("InternalName", "Cadence")
        .set("OriginalFilename", "Cadence.exe")
        .compile()
}
