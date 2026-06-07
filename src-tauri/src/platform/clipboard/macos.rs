//! macOS clipboard paste via `pbcopy`/`pbpaste` + `Cmd+V`.
//!
//! This is the exact algorithm that used to live in `tools::paste_via_clipboard`
//! before the platform split: 1) read the current clipboard with `pbpaste`,
//! 2) put our text on the clipboard with `pbcopy`, 3) simulate `Cmd+V` with
//! enigo, 4) wait briefly for the target app to read the pasteboard,
//! 5) restore the original clipboard contents.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use enigo::{Direction, Keyboard};

use crate::error::AppError;

pub fn paste(text: &str) -> Result<(), AppError> {
    // 1. Save current clipboard (best-effort; empty on failure).
    let saved = Command::new("pbpaste")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // 2. Put our text on the clipboard.
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(AppError::Io)?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("pbcopy stdin unavailable")))?
        .write_all(text.as_bytes())
        .map_err(AppError::Io)?;
    child.wait().map_err(AppError::Io)?;

    // 3. Simulate Cmd+V.
    {
        let mut enigo = super::new_enigo()?;
        enigo
            .key(enigo::Key::Meta, Direction::Press)
            .map_err(|e| AppError::Other(anyhow::anyhow!("cmd press failed: {e}")))?;
        let v = enigo.key(enigo::Key::Unicode('v'), Direction::Click);
        let _ = enigo.key(enigo::Key::Meta, Direction::Release);
        v.map_err(|e| AppError::Other(anyhow::anyhow!("paste key failed: {e}")))?;
    }

    // 4. Give the target app a moment to read the pasteboard, then restore.
    thread::sleep(Duration::from_millis(80));
    let mut restore = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(AppError::Io)?;
    if let Some(stdin) = restore.stdin.as_mut() {
        let _ = stdin.write_all(saved.as_bytes());
    }
    let _ = restore.wait();
    Ok(())
}
