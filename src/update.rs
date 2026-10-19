//! Self-update from GitHub releases.
//!
//! 1. Ask the GitHub API for the latest release.
//! 2. Download `SHA256SUMS.txt` and its minisign signature, and check the
//!    signature with the public key built into this program.
//! 3. Download this platform's program file and check its SHA-256 against
//!    the signed list.
//! 4. Stage it. The user restarts when they choose; then the running program
//!    file is swapped and the new version starts.
//!
//! Nothing is installed without a valid signature from the release key.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sys::net;

pub const REPO: &str = "Car-Role/replay";
const PUBLIC_KEY: &str = include_str!("../assets/replay-update.pub");
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The release asset that holds this platform's program file.
pub fn updater_asset() -> &'static str {
    if cfg!(windows) {
        "replay-windows-x64.exe"
    } else if cfg!(target_os = "macos") {
        "replay-macos-universal"
    } else if cfg!(target_arch = "aarch64") {
        "replay-linux-aarch64"
    } else {
        "replay-linux-x86_64"
    }
}

pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases/latest")
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub notes: String,
    asset_url: Option<String>,
    sums_url: Option<String>,
    sig_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Staged {
    pub version: String,
    pub file: PathBuf,
}

fn parse_version(s: &str) -> Option<[u64; 3]> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some([it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)])
}

/// True when `candidate` is a newer version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn headers() -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/vnd.github+json".into()),
        ("User-Agent".into(), crate::share::USER_AGENT.into()),
    ]
}

/// Returns the latest release when it is newer than this program.
pub fn check() -> Result<Option<Release>, String> {
    check_against(CURRENT)
}

/// Returns the latest release when it is newer than `current`.
pub fn check_against(current: &str) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let r = net::request("GET", &url, &headers(), &[], &|_, _| {})?;
    if r.status == 404 {
        return Ok(None); // No release published yet.
    }
    if r.status != 200 {
        return Err(format!("GitHub returned HTTP {}", r.status));
    }
    let v: serde_json::Value = serde_json::from_slice(&r.body).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or_default().to_string();
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) || !is_newer(&tag, current) {
        return Ok(None);
    }
    let find = |name: &str| {
        v["assets"].as_array().and_then(|a| {
            a.iter().find(|x| x["name"].as_str() == Some(name)).and_then(|x| x["browser_download_url"].as_str()).map(String::from)
        })
    };
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        page: v["html_url"].as_str().map(String::from).unwrap_or_else(releases_page),
        notes: v["body"].as_str().unwrap_or_default().chars().take(2000).collect(),
        asset_url: find(updater_asset()),
        sums_url: find("SHA256SUMS.txt"),
        sig_url: find("SHA256SUMS.txt.minisig"),
    }))
}

/// Why this copy cannot replace itself (for example, installed by a package
/// manager into a system folder), or Ok when it can.
pub fn can_self_update() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("unknown program folder")?;
    if cfg!(target_os = "linux") && (dir.starts_with("/usr") || dir.starts_with("/opt")) {
        return Err("Replay was installed by your package manager. Update it the same way.".into());
    }
    // Writable folder check.
    let probe = dir.join(".replay-update-probe");
    match fs::write(&probe, b"x") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(format!("Replay cannot write to {}. Download the new version from the website.", dir.display())),
    }
}

fn update_dir() -> PathBuf {
    crate::paths::data_dir().join("update")
}

fn verify_sums(sums: &[u8], sig: &str) -> Result<(), String> {
    verify_with(PUBLIC_KEY, sums, sig)
}

fn verify_with(public_key: &str, sums: &[u8], sig: &str) -> Result<(), String> {
    let pk_line = public_key.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("untrusted comment:")).ok_or("no public key")?;
    let pk = minisign_verify::PublicKey::from_base64(pk_line).map_err(|e| format!("bad public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(sig).map_err(|e| format!("bad signature file: {e}"))?;
    pk.verify(sums, &sig, false).map_err(|_| "the release signature is not valid; the update was not installed".to_string())
}

fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let file = it.next()?.trim_start_matches('*');
        (file == name).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads and verifies the update, then stages it.
pub fn download(rel: &Release, progress: &dyn Fn(u64, u64)) -> Result<Staged, String> {
    can_self_update()?;
    let (Some(asset), Some(sums_url), Some(sig_url)) = (&rel.asset_url, &rel.sums_url, &rel.sig_url) else {
        return Err(format!("release {} has no update file for this system", rel.version));
    };
    let get = |url: &str| -> Result<Vec<u8>, String> {
        let r = net::request("GET", url, &headers(), &[], &|_, _| {})?;
        if r.status != 200 {
            return Err(format!("download failed with HTTP {}", r.status));
        }
        Ok(r.body)
    };
    let sums = get(sums_url)?;
    let sig = get(sig_url)?;
    verify_sums(&sums, &String::from_utf8_lossy(&sig))?;
    let sums = String::from_utf8_lossy(&sums);
    let want = expected_hash(&sums, updater_asset()).ok_or("the signed file list does not include this system's update")?;

    let dir = update_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{}.part", updater_asset()));
    net::download(asset, &headers(), &part, progress)?;
    let got = sha256_file(&part)?;
    if got != want {
        let _ = fs::remove_file(&part);
        return Err("the downloaded file does not match the signed hash; the update was not installed".into());
    }
    let file = dir.join(format!("{}-{}", rel.version, updater_asset()));
    fs::rename(&part, &file).map_err(|e| e.to_string())?;
    let staged = Staged { version: rel.version.clone(), file };
    fs::write(dir.join("pending.json"), serde_json::to_vec_pretty(&staged).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    log::info!("update {} downloaded and verified", rel.version);
    Ok(staged)
}

/// An update that is downloaded, verified and newer than this program.
pub fn staged() -> Option<Staged> {
    let text = fs::read(update_dir().join("pending.json")).ok()?;
    let s: Staged = serde_json::from_slice(&text).ok()?;
    (is_newer(&s.version, CURRENT) && s.file.exists()).then_some(s)
}

/// Swaps in the staged program file and starts it with `args`.
/// The caller must exit right after this returns Ok.
pub fn apply_and_restart(staged: &Staged, args: &[&str]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        // A running exe cannot be overwritten, but it can be renamed.
        let old = old_exe_path(&exe);
        let _ = fs::remove_file(&old);
        fs::rename(&exe, &old).map_err(|e| format!("cannot replace {}: {e}", exe.display()))?;
        if let Err(e) = fs::copy(&staged.file, &exe) {
            let _ = fs::rename(&old, &exe);
            return Err(format!("cannot install the update: {e}"));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp = exe.with_extension("new");
        fs::copy(&staged.file, &tmp).map_err(|e| format!("cannot install the update: {e}"))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &exe).map_err(|e| format!("cannot replace {}: {e}", exe.display()))?;
    }
    let _ = fs::remove_file(update_dir().join("pending.json"));
    let pid = std::process::id().to_string();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--after-update").arg(&pid).args(args);
    cmd.spawn().map_err(|e| format!("cannot start the new version: {e}"))?;
    log::info!("updated to {}; restarting", staged.version);
    Ok(())
}

#[cfg(windows)]
fn old_exe_path(exe: &Path) -> PathBuf {
    let stem = exe.file_stem().unwrap_or_default().to_string_lossy();
    exe.with_file_name(format!("{stem}.old.exe"))
}

/// Runs at every start: cleans up after a finished update.
pub fn finish_pending() {
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(windows)]
        let _ = fs::remove_file(old_exe_path(&exe));
        let _ = exe;
    }
    let dir = update_dir();
    if let Ok(text) = fs::read(dir.join("pending.json"))
        && let Ok(s) = serde_json::from_slice::<Staged>(&text)
        && !is_newer(&s.version, CURRENT)
    {
        let _ = fs::remove_file(dir.join("pending.json"));
    }
    // Remove downloads that are no longer needed.
    let keep = staged().map(|s| s.file);
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n != "pending.json") && Some(&p) != keep.as_ref() {
                let _ = fs::remove_file(p);
            }
        }
    }
}

/// Waits (up to 15 s) for the previous version's process to exit.
pub fn wait_for_exit(pid: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) => {
                let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                alive
            }
            Err(_) => false,
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn finds_hash_lines() {
        let sums = "abc123  replay-windows-x64.exe\nDEF456 *replay-linux-x86_64\n";
        assert_eq!(expected_hash(sums, "replay-windows-x64.exe").as_deref(), Some("abc123"));
        assert_eq!(expected_hash(sums, "replay-linux-x86_64").as_deref(), Some("def456"));
        assert_eq!(expected_hash(sums, "nope"), None);
    }

    #[test]
    fn public_key_parses() {
        let line = PUBLIC_KEY.lines().find(|l| !l.starts_with("untrusted")).unwrap();
        assert!(minisign_verify::PublicKey::from_base64(line.trim()).is_ok());
    }

    #[test]
    fn accepts_minisign_signatures_and_rejects_tampering() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk = kp.pk.to_box().unwrap().into_string();
        let data = b"abc  replay-windows-x64.exe
";
        let sig = minisign::sign(None, &kp.sk, &data[..], Some("file:SHA256SUMS.txt"), None).unwrap().into_string();
        assert!(verify_with(&pk, data, &sig).is_ok());
        assert!(verify_with(&pk, b"abd  replay-windows-x64.exe
", &sig).is_err());
        // A signature from another key must fail against the built-in key.
        assert!(verify_sums(data, &sig).is_err());
    }

    #[test]
    fn rejects_bad_signature() {
        let bad = "untrusted comment: x\nRUScCVL9ehOESAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\ntrusted comment: x\nAAAA\n";
        assert!(verify_sums(b"data", bad).is_err());
    }
}
