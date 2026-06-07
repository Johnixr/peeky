//! Windows clipboard paste via `clip.exe` + `powershell.exe` + `Ctrl+V`.
//!
//! Strategy mirrors the macOS path: 1) save the current clipboard text via
//! PowerShell's `Get-Clipboard`, 2) write our text via `clip.exe`, 3) send
//! `Ctrl+V` with enigo (NOT `Cmd+V` — the platform key for paste on Windows
//! is `Key::Control`), 4) wait briefly, 5) restore the original.
//!
//! Image clipboard is not yet handled on either platform; we only round-trip
//! text. If the user's previous clipboard held an image, the restore step
//! silently puts text there instead — acceptable for v1, listed as
//! `peeky-windows-4` in the port plan.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use enigo::{Direction, Keyboard};

use crate::error::AppError;

pub fn paste(text: &str) -> Result<(), AppError> {
    // 1. Save current clipboard text. PowerShell is the only built-in reader
    //    that exposes `Get-Clipboard`; if the host policy blocks it the read
    //    fails, and we fall back to an empty restore (target app will still
    //    receive our paste, just won't get its prior clipboard back).
    let saved = read_clipboard_text().unwrap_or_default();

    // 2. Put our text on the clipboard.
    let mut child = Command::new("clip.exe")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Other(anyhow::anyhow!("spawn clip.exe failed: {e}")))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("clip.exe stdin unavailable")))?
        .write_all(text.as_bytes())
        .map_err(|e| AppError::Other(anyhow::anyhow!("write to clip.exe failed: {e}")))?;
    let _ = child.wait();

    // 3. Simulate Ctrl+V. CRITICAL: must be `Key::Control` on Windows;
    //    reusing `Key::Meta` would either be ignored or insert Meta+V literally.
    {
        let mut enigo = super::new_enigo()?;
        enigo
            .key(enigo::Key::Control, Direction::Press)
            .map_err(|e| AppError::Other(anyhow::anyhow!("ctrl press failed: {e}")))?;
        let v = enigo.key(enigo::Key::Unicode('v'), Direction::Click);
        let _ = enigo.key(enigo::Key::Control, Direction::Release);
        v.map_err(|e| AppError::Other(anyhow::anyhow!("paste key failed: {e}")))?;
    }

    // 4. Give the target app a moment to read the clipboard, then restore.
    thread::sleep(Duration::from_millis(80));
    if !saved.is_empty() {
        // Best-effort restore; if it fails the user just has our paste text
        // in their clipboard until the next copy. Better than panicking.
        let mut restore = Command::new("clip.exe")
            .stdin(Stdio::piped())
            .spawn()
            .ok();
        if let Some(ref mut child) = restore {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(saved.as_bytes());
            }
            let _ = child.wait();
        }
    }
    Ok(())
}

/// Read the current text clipboard via `powershell.exe Get-Clipboard`.
/// Returns `None` on any failure (PowerShell blocked by policy, non-text
/// clipboard, etc.) — the caller treats that as "nothing to restore".
fn read_clipboard_text() -> Option<String> {
    let output = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-Command")
        .arg("Get-Clipboard")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
