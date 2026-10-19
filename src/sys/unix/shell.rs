//! Desktop helpers for macOS and Linux: open, reveal, clipboard, trash,
//! start at login, file pickers.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ipc::RecorderMsg;

fn run_quiet(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn open_path(p: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener).arg(p).spawn();
}

pub fn open_url(url: &str) {
    if url.starts_with("https://") {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = Command::new(opener).arg(url).spawn();
    }
}

/// Shows the file in the file manager, selected where supported.
pub fn reveal(p: &Path) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg("-R").arg(p).spawn();
        return;
    }
    // The FileManager1 D-Bus interface selects the file (Nautilus, Dolphin, Nemo...).
    let uri = format!("file://{}", p.display());
    let ok = run_quiet(
        "dbus-send",
        &[
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ],
    );
    if !ok && let Some(dir) = p.parent() {
        open_path(dir);
    }
}

pub fn copy_text(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut c) => c.set_text(text.to_string()).is_ok(),
        Err(_) => false,
    }
}

/// Puts the file itself on the clipboard so it can be pasted into chat apps.
pub fn copy_file(p: &Path) -> bool {
    if cfg!(target_os = "macos") {
        let script = format!("set the clipboard to (POSIX file \"{}\")", p.display().to_string().replace('"', "\\\""));
        return run_quiet("osascript", &["-e", &script]);
    }
    let uri = format!("file://{}\n", p.display());
    for (cmd, args) in [("wl-copy", vec!["--type", "text/uri-list"]), ("xclip", vec!["-selection", "clipboard", "-t", "text/uri-list"])] {
        if let Ok(mut child) = Command::new(cmd).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(uri.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Moves files to the Trash (restorable).
pub fn recycle(paths: &[PathBuf]) -> bool {
    paths.iter().all(|p| trash_one(p))
}

#[cfg(target_os = "macos")]
fn trash_one(p: &Path) -> bool {
    let script = format!(
        "tell application \"Finder\" to delete (POSIX file \"{}\" as alias)",
        p.display().to_string().replace('"', "\\\"")
    );
    run_quiet("osascript", &["-e", &script])
}

/// freedesktop.org Trash specification: move into `Trash/files` and write a `.trashinfo`.
#[cfg(target_os = "linux")]
fn trash_one(p: &Path) -> bool {
    let Some(base) = dirs::data_local_dir().map(|d| d.join("Trash")) else { return false };
    let (files, info) = (base.join("files"), base.join("info"));
    if std::fs::create_dir_all(&files).is_err() || std::fs::create_dir_all(&info).is_err() {
        return false;
    }
    let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let mut target = name.clone();
    let mut n = 1;
    while files.join(&target).exists() || info.join(format!("{target}.trashinfo")).exists() {
        n += 1;
        target = format!("{n}.{name}");
    }
    let t = crate::util::local_time();
    let encoded: String = p
        .to_string_lossy()
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let text = format!(
        "[Trash Info]\nPath={encoded}\nDeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}\n",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    if std::fs::write(info.join(format!("{target}.trashinfo")), text).is_err() {
        return false;
    }
    if std::fs::rename(p, files.join(&target)).is_ok() {
        return true;
    }
    // Different filesystem: copy, then delete.
    let ok = std::fs::copy(p, files.join(&target)).is_ok() && std::fs::remove_file(p).is_ok();
    if !ok {
        let _ = std::fs::remove_file(info.join(format!("{target}.trashinfo")));
    }
    ok
}

/// Starts the recorder at login.
pub fn set_autostart(on: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    #[cfg(target_os = "macos")]
    {
        let Some(dir) = dirs::home_dir().map(|h| h.join("Library/LaunchAgents")) else { return false };
        let file = dir.join("io.github.car-role.replay.plist");
        if !on {
            return std::fs::remove_file(&file).is_ok() || !file.exists();
        }
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>io.github.car-role.replay</string>\n<key>ProgramArguments</key><array><string>{}</string></array>\n<key>RunAtLoad</key><true/>\n</dict></plist>\n",
            exe.display()
        );
        std::fs::create_dir_all(&dir).is_ok() && std::fs::write(file, plist).is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        let Some(dir) = dirs::config_dir().map(|c| c.join("autostart")) else { return false };
        let file = dir.join("replay.desktop");
        if !on {
            return std::fs::remove_file(&file).is_ok() || !file.exists();
        }
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=Replay\nComment=Instant replay for games\nExec=\"{}\"\nIcon=replay\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::create_dir_all(&dir).is_ok() && std::fs::write(file, text).is_ok()
    }
}

pub fn pick_folder() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return output("osascript", &["-e", "POSIX path of (choose folder with prompt \"Choose the clips folder\")"]).map(PathBuf::from);
    }
    output("zenity", &["--file-selection", "--directory", "--title=Choose the clips folder"])
        .or_else(|| output("kdialog", &["--getexistingdirectory"]))
        .map(PathBuf::from)
}

/// Picks a program (an app bundle on macOS).
pub fn pick_exe() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let p = output("osascript", &["-e", "POSIX path of (choose application with prompt \"Choose a game\" as alias)"])?;
        // Use the executable name inside the bundle, which is what detection compares.
        let app = PathBuf::from(p.trim_end_matches('/'));
        let stem = app.file_stem()?.to_string_lossy().into_owned();
        return Some(app.join("Contents/MacOS").join(stem));
    }
    output("zenity", &["--file-selection", "--title=Choose a game"]).or_else(|| output("kdialog", &["--getopenfilename"])).map(PathBuf::from)
}

/// Thumbnails come from the system only on Windows; the list shows a placeholder.
pub fn thumbnail(_p: &Path, _w: i32, _h: i32) -> Option<(usize, usize, Vec<u8>)> {
    None
}

pub fn recorder_running() -> bool {
    super::ipc::recorder_running()
}

pub fn send_to_recorder(msg: RecorderMsg) -> bool {
    super::ipc::send(msg)
}
