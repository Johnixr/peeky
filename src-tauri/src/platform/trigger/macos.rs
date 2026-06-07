//! macOS front-app context via AppleScript / `osascript`.

use std::process::Command;

use crate::types::AppContext;

/// L0 event-driven gate: read the frontmost app, its focused window title, and
/// (for known browsers) a best-effort current tab URL via `osascript`. Free,
/// instant, and the primary "did the context actually change?" signal.
pub fn front_app_context() -> AppContext {
    let app = frontmost_app_name();
    let title = focused_window_title(&app);
    let url = if is_browser(&app) {
        browser_active_url(&app)
    } else {
        None
    };
    AppContext { app, title, url }
}

/// Accurate name of the frontmost application (e.g. "Safari", "Code").
pub fn frontmost_app_name() -> String {
    run_osascript(
        r#"tell application "System Events" to get name of first application process whose frontmost is true"#,
    )
    .unwrap_or_default()
}

fn focused_window_title(app: &str) -> String {
    if app.is_empty() {
        return String::new();
    }
    let script = format!(
        r#"tell application "System Events"
    try
        tell process "{app}"
            return value of attribute "AXTitle" of front window
        end tell
    on error
        return ""
    end try
end tell"#,
        app = escape_applescript(app),
    );
    run_osascript(&script).unwrap_or_default()
}

fn is_browser(app: &str) -> bool {
    matches!(
        app,
        "Safari"
            | "Safari Technology Preview"
            | "Google Chrome"
            | "Google Chrome Canary"
            | "Chromium"
            | "Brave Browser"
            | "Microsoft Edge"
            | "Arc"
            | "Vivaldi"
            | "Opera"
    )
}

fn browser_active_url(app: &str) -> Option<String> {
    let esc = escape_applescript(app);
    let script = if app.starts_with("Safari") {
        format!(
            r#"tell application "{app}"
    try
        return URL of front document
    on error
        return ""
    end try
end tell"#,
            app = esc,
        )
    } else {
        format!(
            r#"tell application "{app}"
    try
        return URL of active tab of front window
    on error
        return ""
    end try
end tell"#,
            app = esc,
        )
    };
    run_osascript(&script)
}

fn run_osascript(script: &str) -> Option<String> {
    let output = Command::new("osascript").arg("-e").arg(script).output().ok()?;
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

fn escape_applescript(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', r#"\""#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_applescript_neutralizes_quotes() {
        assert_eq!(escape_applescript(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
