//! Per-OS platform facade (PRD §2.2 L0 + L1 + §5.1).
//!
//! All OS-specific code (AppleScript, Win32 FFI, Windows clipboard, system
//! metadata) lives in the per-OS submodules and is re-exported through this
//! `mod.rs` so the rest of the crate can call into a stable, OS-neutral API.
//!
//! Layout:
//!   * `trigger`   — read the frontmost app + window title (+ best-effort
//!                   browser URL). `front_app_context()` is the L0 event gate
//!                   used by the main loop; the individual accessors are also
//!                   exposed for the copilot `get_context` tool.
//!   * `system`    — host identity: computer name + OS label, fed into the
//!                   quick-shortcut prompt context.
//!   * `clipboard` — `paste_via_clipboard`: save clipboard, write text, send
//!                   paste shortcut, restore. Used by `tools::type_text` for
//!                   IME/CJK robustness.
//!   * `skip_apps` — substring list of "this isn't the user's content" window
//!                   owners (system/overlay). Used by the xcap-based
//!                   `capture::focused_window` and `capture::frontmost_app_name`
//!                   to filter out windows we should never capture.
//!
//! Each submodule is a thin dispatch: `mod.rs` is the only file non-platform
//! code imports, and the per-OS branches are `#[cfg(target_os = …)]`.

pub mod clipboard;
pub mod skip_apps;
pub mod system;
pub mod trigger;

// Stable re-exports used by the rest of the crate. Consumers (trigger.rs,
// capture.rs, tools.rs, lib.rs::system_context) only need these names — they
// never have to touch the per-OS modules directly.
pub use clipboard::paste_via_clipboard;
pub use skip_apps::is_overlay_or_system_window;
pub use system::{computer_name, os_label};
pub use trigger::{front_app_context, frontmost_app_name};
