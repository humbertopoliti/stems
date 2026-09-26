//! Copying to the clipboard (29): an OSC 52 escape sequence written to the
//! terminal (works over ssh when the terminal allows it), or a clipboard
//! command (`pbcopy`, `wl-copy`, `xclip`) when `clipboard = "command"`.
//! Both are fire-and-forget: the dashboard never waits on the clipboard.

use std::io::Write;
use std::process::{Command, Stdio};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (with `=` padding).
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The OSC 52 sequence setting the clipboard (`c`) to `text`:
/// `ESC ] 52 ; c ; <base64> BEL`.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// The clipboard command for this platform/session, if any is known:
/// `pbcopy` on macOS, `wl-copy` under Wayland, else `xclip`.
pub fn command(env: impl Fn(&str) -> Option<String>) -> (&'static str, &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if env("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    }
}

/// Pipe `text` into the clipboard command on a background thread; errors
/// (no such command) are ignored.
pub fn copy_with_command(text: String) {
    let (prog, args) = command(|k| std::env::var(k).ok());
    std::thread::spawn(move || {
        let Ok(mut child) = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        // RFC 4648 test vectors.
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o, "{i:?}");
        }
        assert_eq!(base64("é✓".as_bytes()), "w6ninJM=");
    }

    #[test]
    fn osc52_sequence() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn command_choice() {
        let (p, _) = command(|k| (k == "WAYLAND_DISPLAY").then(|| "wayland-0".into()));
        if cfg!(target_os = "macos") {
            assert_eq!(p, "pbcopy");
        } else {
            assert_eq!(p, "wl-copy");
            assert_eq!(command(|_| None).0, "xclip");
        }
    }
}
