//! Embeds the icons and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/generated/replay.ico");
    println!("cargo:rerun-if-changed=assets/generated/replay-recording.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Resource 1 is the app icon Explorer shows; 2 is the tray "recording" icon.
        res.set_icon_with_id("assets/generated/replay.ico", "1");
        res.set_icon_with_id("assets/generated/replay-recording.ico", "2");
        res.set("ProductName", "Replay");
        res.set("FileDescription", "Replay");
        res.set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            panic!("cannot embed Windows resources: {e}");
        }
    }
}
