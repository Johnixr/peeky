//! Cross-platform "this window isn't the user's content" filter.
//!
//! The list merges macOS-specific system/overlay process names
//! (`window server`, `dock`, `control cent`, `spotlight`, …) with
//! Windows-specific ones (`searchui`, `shellexperiencehost`,
//! `textinputhost`, `lockapp`, `action center`, …). All entries are stored
//! lowercased and matched as substrings against the lowercased window owner
//! name reported by xcap. Substring matching is conservative: it can over-skip
//! (we'd rather miss a capture than capture a desktop helper), never under-skip
//! (we never want the model to "see" our own overlay).
//!
//! Consumers: `capture::focused_window` and `capture::frontmost_app_name`.

const SKIP_APPS_MACOS: &[&str] = &[
    "peeky",
    "window server",
    "dock",
    "control cent",
    "systemuiserver",
    "notification cent",
    "spotlight",
    "screenshot",
    "wallpaper",
    "coreautha",
    "universalcontrol",
    "textinputmenuagent",
];

const SKIP_APPS_WINDOWS: &[&str] = &[
    "peeky",
    "searchui",
    "shellexperiencehost",
    "textinputhost",
    "lockapp",
    "action center",
    "taskbar",
    "startmenu",
    "system tray",
    "cortana",
    "windowsinput",
];

/// True if `name` (case-insensitive substring) is a known system/overlay
/// window owner. The list is the union of the macOS and Windows lists, so
/// `is_overlay_or_system_window` is safe to call from any per-OS code path
/// without a `#[cfg]` of its own.
pub fn is_overlay_or_system_window(name: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    SKIP_APPS_MACOS
        .iter()
        .chain(SKIP_APPS_WINDOWS.iter())
        .any(|s| lname.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_macos_system_names() {
        assert!(is_overlay_or_system_window("Window Server"));
        assert!(is_overlay_or_system_window("Dock"));
        assert!(is_overlay_or_system_window("Spotlight"));
        assert!(is_overlay_or_system_window("Peeky"));
    }

    #[test]
    fn filters_windows_system_names() {
        assert!(is_overlay_or_system_window("SearchUI"));
        assert!(is_overlay_or_system_window("ShellExperienceHost"));
        assert!(is_overlay_or_system_window("TextInputHost"));
        assert!(is_overlay_or_system_window("LockApp"));
    }

    #[test]
    fn does_not_filter_real_user_apps() {
        assert!(!is_overlay_or_system_window("Safari"));
        assert!(!is_overlay_or_system_window("Code"));
        assert!(!is_overlay_or_system_window("msedge"));
        assert!(!is_overlay_or_system_window("Slack"));
    }
}
