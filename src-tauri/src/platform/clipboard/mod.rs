//! Cross-platform clipboard paste + restore.
//!
//! Provides direct text clipboard writes plus a paste-and-restore helper.
//! `tools::type_text` uses the latter to keep IME/CJK input robust (raw enigo
//! keystrokes can be unreliable with some IMEs).
//!
//! The two backends are intentionally separate (one `cfg` per file) so they
//! can never silently share an implementation — on Windows the paste shortcut
//! must be `Ctrl+V` (`Key::Control`), not `Cmd+V` (`Key::Meta`), and the
//! clipboard round-trip is `clip.exe` / `powershell.exe`, not `pbcopy` /
//! `pbpaste`.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Construct an `Enigo` instance. Shared by the per-OS backends so they
/// don't each have to know the enigo settings dance.
fn new_enigo() -> anyhow::Result<enigo::Enigo> {
    use enigo::{Enigo, Settings as EnigoSettings};
    Enigo::new(&EnigoSettings::default())
        .map_err(|e| anyhow::anyhow!("input backend unavailable: {e}"))
}

/// Write text to the system clipboard without pasting or restoring.
pub fn write_text(text: &str) -> Result<(), crate::error::AppError> {
    #[cfg(target_os = "macos")]
    {
        macos::write_text(text)
    }
    #[cfg(target_os = "windows")]
    {
        windows::write_text(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = text;
        Err(crate::error::AppError::Other(anyhow::anyhow!(
            "write_text: unsupported platform"
        )))
    }
}

/// Paste text into the active app, then restore the previous text clipboard.
pub fn paste_via_clipboard(text: &str) -> Result<(), crate::error::AppError> {
    #[cfg(target_os = "macos")]
    {
        macos::paste(text)
    }
    #[cfg(target_os = "windows")]
    {
        windows::paste(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = text;
        Err(crate::error::AppError::Other(anyhow::anyhow!(
            "paste_via_clipboard: unsupported platform"
        )))
    }
}
