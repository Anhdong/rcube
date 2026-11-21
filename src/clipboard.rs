//! Clipboard access without heavy dependencies.
//!
//! Tries `wl-copy` (Wayland), `xclip` / `xsel` (X11) and finally falls back to
//! the OSC 52 terminal escape sequence, which most modern terminals
//! (kitty, alacritty, wezterm, foot, iTerm2, tmux with `set-clipboard on`, ...)
//! understand - this also makes it work over SSH.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long we wait for a clipboard helper before assuming it is serving the
/// selection in the foreground (and carrying on without blocking the UI).
const HELPER_WAIT: Duration = Duration::from_millis(400);

pub fn copy(text: &str) -> Result<&'static str, String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();

    let mut candidates: Vec<(&str, &[&str])> = Vec::new();
    if wayland {
        candidates.push(("wl-copy", &[]));
    }
    if x11 {
        candidates.push(("xclip", &["-selection", "clipboard"]));
        candidates.push(("xsel", &["--clipboard", "--input"]));
    }
    for (cmd, args) in candidates {
        if pipe_to(cmd, args, text) {
            return Ok("clipboard");
        }
    }

    osc52(text).map_err(|e| format!("copy failed: {e}"))?;
    Ok("via terminal, OSC 52")
}

fn pipe_to(cmd: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.wait();
            return false;
        }
        // stdin dropped here -> EOF
    }
    wait_briefly(&mut child)
}

/// The tools normally fork into the background to serve the selection, so the
/// foreground process exits quickly. If one stays in the foreground instead,
/// waiting for it would freeze the whole timer, so give up waiting after a
/// short time and assume it is doing its job.
fn wait_briefly(child: &mut Child) -> bool {
    let deadline = Instant::now() + HELPER_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

fn osc52(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"R U R' U'"), "UiBVIFInIFUn");
    }

    #[test]
    fn slow_helper_does_not_block_forever() {
        // `sleep` ignores stdin and stays in the foreground.
        let mut child = Command::new("sleep")
            .arg("5")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let t = Instant::now();
        assert!(wait_briefly(&mut child));
        assert!(t.elapsed() < Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
    }
}
