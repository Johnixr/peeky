//! Windows front-app context via Win32 FFI (`windows-sys 0.59`).
//!
//! The macOS path uses AppleScript for both the front-app name AND the
//! focused window title (System Events' `AXTitle`). On Windows we read
//! each independently from the same HWND:
//!
//!   * front-app name   ← `GetForegroundWindow` →
//!                        `GetWindowThreadProcessId` →
//!                        `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
//!                        → `QueryFullProcessImageNameW` → strip the `.exe`.
//!   * window title     ← `GetWindowTextW` on the same HWND.
//!   * browser URL      ← v1 TODO `peeky-windows-1`. UIA via
//!                        `UIAutomationCore` would give us the Address Bar
//!                        Root's `Value` property, but that's a chunk of
//!                        additional FFI plus a non-trivial COM dance. The
//!                        model is informed via the window title in v1.
//!
//! All of these are best-effort and **never panic**: every FFI call is
//! matched to a sensible default (empty string / `None`). The lockable
//! handles (`HANDLE` from `OpenProcess`) are short-lived — opened,
//! read, closed within the same function — so we don't need a
//! companion cleanup helper.

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::types::AppContext;

/// Read the frontmost app + its focused window title via Win32 FFI. The
/// `url` slot is `None` on Windows in v1 (see module docs).
pub fn front_app_context() -> AppContext {
    let app = frontmost_app_name();
    let title = if app.is_empty() {
        String::new()
    } else {
        focused_window_title()
    };
    AppContext {
        app,
        title,
        url: None, // v1 TODO peeky-windows-1: UIA Address Band Root Value
    }
}

/// Name of the frontmost application, derived from the foreground window's
/// process image path with the `.exe` suffix stripped. Returns "" on any
/// failure (no foreground window, access denied, etc.).
pub fn frontmost_app_name() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return String::new();
    }
    let mut pid: u32 = 0;
    let _tid = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return String::new();
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return String::new();
    }

    // QueryFullProcessImageNameW writes a null-terminated wide-string path.
    // We hand it a generous buffer; 1024 is well above the practical path
    // length on a real system.
    const BUF_LEN: usize = 1024;
    let mut buf = [0u16; BUF_LEN];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            0, // WIN32_PATH_FORMAT
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 || size == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..size as usize]);
    process_name_from_path(&path)
}

/// Title of the foreground window. Returns "" on any failure (no window,
/// non-UTF16 title, etc.).
fn focused_window_title() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return String::new();
    }
    // GetWindowTextW writes up to nMaxCount chars INCLUDING the null
    // terminator, and returns the length NOT including the terminator
    // (or 0 on error).
    const BUF_LEN: usize = 1024;
    let mut buf = [0u16; BUF_LEN];
    let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), BUF_LEN as i32) };
    if len <= 0 {
        return String::new();
    }
    let len = (len as usize).min(BUF_LEN);
    String::from_utf16_lossy(&buf[..len])
}

/// Extract the lowercased file stem from a full process image path. E.g.
/// `C:\Program Files\Google\Chrome\Application\chrome.exe` → `chrome`. Used
/// so the front-app name reads the same on both macOS (`Safari`) and Windows
/// (`msedge`), instead of leaking a path or `.exe`.
fn process_name_from_path(path: &str) -> String {
    // Find the last path separator (`\` or `/`).
    let last_sep = path
        .rfind(|c| c == '\\' || c == '/')
        .map(|i| i + 1)
        .unwrap_or(0);
    let stem = &path[last_sep..];
    // Strip a trailing `.exe` (case-insensitive) for the same reason.
    let stem = if stem.len() >= 4 && stem[stem.len() - 4..].eq_ignore_ascii_case(".exe") {
        &stem[..stem.len() - 4]
    } else {
        stem
    };
    stem.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_name_strips_path_and_exe() {
        assert_eq!(
            process_name_from_path("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"),
            "chrome"
        );
        assert_eq!(process_name_from_path("/usr/bin/something"), "something");
        assert_eq!(process_name_from_path("msedge.EXE"), "msedge");
        assert_eq!(process_name_from_path("Code"), "code");
        assert_eq!(process_name_from_path(""), "");
    }

    /// The Win32 surface is a no-op in headless test runs; this just
    /// confirms the front-app probe doesn't panic when there is no
    /// foreground window. The CI box is a service, so `GetForegroundWindow`
    /// is expected to return `NULL` — empty string is the contract.
    #[test]
    fn frontmost_app_name_does_not_panic_when_no_foreground() {
        let name = frontmost_app_name();
        // The function must not panic; the result is "" or the host's
        // real foreground (in a developer machine). Both are valid.
        let _ = name;
    }

    #[test]
    fn focused_window_title_does_not_panic_when_no_foreground() {
        let title = focused_window_title();
        let _ = title;
    }
}
