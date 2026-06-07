//! Front-app context (frontmost app + focused window title + best-effort
//! browser URL). One per-OS backend is compiled in:
//!
//!   * macOS — AppleScript / `osascript` (uses System Events for the
//!             authoritative front-app name, plus the AXTitle of the front
//!             window, plus a tab-URL probe for known browsers).
//!   * Windows — Win32 FFI: `GetForegroundWindow` →
//!             `GetWindowThreadProcessId` → `QueryFullProcessImageNameW` for
//!             the front-app name, and `GetWindowTextW` for the window
//!             title. Browser URL is a v1 TODO (`peeky-windows-1`) — UIA
//!             implementation is deferred; the model still gets the title.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Public re-exports: the rest of the crate never touches the per-OS
// submodules directly. The signatures + contracts are identical across
// platforms.
pub use macos_or_windows::*;

#[cfg(target_os = "macos")]
mod macos_or_windows {
    pub use super::macos::{front_app_context, frontmost_app_name};
}

#[cfg(target_os = "windows")]
mod macos_or_windows {
    pub use super::windows::{front_app_context, frontmost_app_name};
}
