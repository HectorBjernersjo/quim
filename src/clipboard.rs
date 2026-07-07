// Clipboard without external crates: pick the first tool that works for the
// environment (clip.exe on WSL, wl-copy/xclip/xsel on Linux, pbcopy on macOS),
// falling back to the OSC 52 escape sequence for remote/odd terminals.
use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy(text: &str) -> Result<(), String> {
    for attempt in candidates() {
        match attempt {
            Backend::Cmd(cmd, args, utf16) => {
                if pipe_to(cmd, args, text, utf16).is_ok() {
                    return Ok(());
                }
            }
            Backend::Osc52 => return osc52(text),
        }
    }
    Err("no clipboard method worked".into())
}

enum Backend {
    Cmd(&'static str, &'static [&'static str], bool),
    Osc52,
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

fn candidates() -> Vec<Backend> {
    let mut out = vec![];
    if cfg!(target_os = "macos") {
        out.push(Backend::Cmd("pbcopy", &[], false));
    }
    if is_wsl() {
        // clip.exe autodetects UTF-16LE (no BOM — it would end up in the
        // clipboard content); plain UTF-8 mangles å/ä/ö.
        out.push(Backend::Cmd("clip.exe", &[], true));
    }
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        out.push(Backend::Cmd("wl-copy", &[], false));
    }
    out.push(Backend::Cmd("xclip", &["-selection", "clipboard"], false));
    out.push(Backend::Cmd("xsel", &["-ib"], false));
    out.push(Backend::Osc52);
    out
}

fn pipe_to(cmd: &str, args: &[&str], text: &str, utf16: bool) -> Result<(), String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        let stdin = child.stdin.as_mut().ok_or("no stdin")?;
        if utf16 {
            let mut bytes: Vec<u8> = Vec::with_capacity(text.len() * 2);
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            stdin.write_all(&bytes).map_err(|e| e.to_string())?;
        } else {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} failed"))
    }
}

fn osc52(text: &str) -> Result<(), String> {
    let payload = base64(text.as_bytes());
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{payload}\x07").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
