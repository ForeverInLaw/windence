//! Stamps the Windows executable with its icon and its name.
//!
//! Without this the app is a nameless default-icon binary in the taskbar,
//! in Explorer and in whatever shortcut an installer makes for it. The
//! icon comes from `assets/AppIcon.ico`, the same artwork the macOS
//! bundle carries as `.icns`.

fn main() -> std::io::Result<()> {
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
