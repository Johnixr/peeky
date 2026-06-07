//! Per-OS host identity (computer name + OS label).
//!
//! Used by `lib.rs::system_context` to build the static portion of the
//! quick-shortcut prompt context. Both functions are best-effort and return
//! an empty string on failure — the caller is responsible for graceful
//! degradation (e.g. "user , computer \"\"." with empty bits is fine).

/// Human-friendly name of the current machine. On macOS this is
/// `scutil --get ComputerName`; on Windows the `%COMPUTERNAME%` env var.
pub fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("scutil")
            .arg("--get")
            .arg("ComputerName")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default()
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        String::new()
    }
}

/// Short, human label for the host OS. Currently returns "macOS" or "Windows".
/// Kept as a free function (not a `cfg!` constant) so the model prompt string
/// is the only thing that ever needs updating if we add a new platform.
pub fn os_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(target_os = "windows")]
    {
        "Windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_label_is_known() {
        let label = os_label();
        assert!(
            label == "macOS" || label == "Windows" || label == "unknown",
            "unexpected OS label: {label}"
        );
    }
}
