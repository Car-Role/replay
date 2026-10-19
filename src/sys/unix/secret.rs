//! Secrets in the system keychain (macOS Keychain, Secret Service on Linux).
//!
//! The config file keeps only a marker; the password itself stays in the
//! keychain under the service "Replay".

const SERVICE: &str = "Replay";
const ACCOUNT: &str = "streamable";
const MARKER: &str = "keychain";

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()
}

/// Stores the secret; returns the marker to keep in the config file.
pub fn protect(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    match entry().map(|e| e.set_password(secret)) {
        Some(Ok(())) => MARKER.into(),
        Some(Err(e)) => {
            log::warn!("cannot store the password in the keychain: {e}");
            String::new()
        }
        None => String::new(),
    }
}

pub fn unprotect(stored: &str) -> Option<String> {
    if stored != MARKER {
        return None;
    }
    entry()?.get_password().ok()
}
