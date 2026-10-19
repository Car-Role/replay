//! Messages to the running recorder over a Unix socket in the user's
//! runtime folder. One line per connection.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::ipc::RecorderMsg;

pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(crate::paths::data_dir);
    base.join("replay-recorder.sock")
}

/// Sends a message. False when no recorder listens.
pub fn send(msg: RecorderMsg) -> bool {
    match UnixStream::connect(socket_path()) {
        Ok(mut s) => writeln!(s, "{}", msg.to_line()).is_ok(),
        Err(_) => false,
    }
}

pub fn recorder_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// Binds the socket (this process is the recorder). Fails when another
/// recorder already listens. Calls `on_msg` from a background thread.
pub fn listen(on_msg: impl Fn(RecorderMsg) + Send + 'static) -> Result<(), String> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err("Replay is already running".into());
    }
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = UnixListener::bind(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok()
                    && let Some(m) = RecorderMsg::from_line(&line)
                {
                    on_msg(m);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Removes the socket file at exit.
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
