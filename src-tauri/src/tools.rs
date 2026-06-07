//! Copilot tool layer (PRD §5). The model plans, calls these tools, the client
//! executes, then re-screenshots to verify. This milestone ships the P1 set
//! fully (`screenshot` / `scroll` / `scroll_and_capture` / `get_context`) and
//! the P2 actuation set (`click` / `type_text` / `key`) as working enigo-based
//! functions guarded by the §3.3 hard-forbidden list.
//!
//! Security red-lines this module enforces:
//! - Every actuating tool runs `guard_intent` first; a forbidden intent
//!   (quit/close, delete, shutdown, sudo, payment, password/secret access,
//!   install/uninstall, browser-data wipe) returns `AppError::Forbidden` and is
//!   NEVER executed (PRD §3.3 — not configurable, all modes).
//! - `type_text` prefers clipboard paste + restore over raw keystrokes for
//!   IME/CJK robustness (PRD §5.1 note).
//!
//! Deeper §3.2 per-step permission gating (YOLO/Auto/Cautious risk tiers and
//! human-in-the-loop confirmation) is intentionally left as a stub hook here —
//! see `TODO(P1)` on `check_permission`.

use std::thread;
use std::time::Duration;

use enigo::{
    Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings as EnigoSettings,
};

use crate::error::{AppError, Result};
use crate::types::{AppContext, CapturedImage, PermissionMode, Quality};
use crate::{capture, trigger};

/// Direction for scroll-based tools. Serialized lowercase so the model's tool
/// arguments map cleanly ("up"/"down").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDir {
    Up,
    Down,
}

// ============================================================================
// §3.3 hard-forbidden list — the safety core of the tool layer.
// ============================================================================

/// Substrings that, if they appear in a tool target / typed text / key combo /
/// task intent, indicate an operation the PRD §3.3 forbids outright. Matched
/// case-insensitively against a normalized intent string. This is deliberately
/// broad and conservative: when in doubt, refuse.
const FORBIDDEN_PATTERNS: &[&str] = &[
    // Delete files / move to trash.
    "rm -rf",
    "rm -r",
    " rm ",
    "delete file",
    "delete folder",
    "delete directory",
    "move to trash",
    "move to the trash",
    "empty trash",
    "废纸篓",
    "删除文件",
    "删除目录",
    "ファイルを削除",
    // Close window / quit app (Cmd+W / Cmd+Q).
    "quit app",
    "quit the app",
    "force quit",
    "close window",
    "close the window",
    "退出应用",
    "关闭窗口",
    "アプリを終了",
    // Power / session.
    "shutdown",
    "shut down",
    "restart computer",
    "reboot",
    "log out",
    "log off",
    "sign out of mac",
    "关机",
    "重启",
    "注销",
    // System settings.
    "system settings",
    "system preferences",
    "修改系统设置",
    "システム設定",
    // Privilege escalation.
    "sudo",
    "administrator password",
    "管理员",
    // Payment / money movement.
    "payment",
    "pay now",
    "checkout",
    "place order",
    "transfer money",
    "wire transfer",
    "支付",
    "转账",
    "下单",
    "決済",
    // Secrets / password managers / key files.
    "password manager",
    "1password",
    "keychain access",
    "reveal password",
    "show password",
    "private key",
    "secret key",
    "api key",
    "密码管理",
    "密钥",
    "パスワード",
    // Install / uninstall software.
    "install software",
    "uninstall",
    "brew install",
    "pip install",
    "npm install -g",
    "安装软件",
    "卸载",
    "インストール",
    // Browser settings / extensions / data wipe.
    "clear browsing data",
    "clear browser data",
    "clear history",
    "manage extensions",
    "browser settings",
    "清除浏览",
    "浏览器扩展",
];

/// Reject any intent that matches the §3.3 hard-forbidden list. Pass a
/// human-readable description of the operation (tool name + arguments). Returns
/// `Err(AppError::Forbidden)` — the caller must propagate it and NOT execute.
pub fn guard_intent(intent: &str) -> Result<()> {
    let hay = intent.to_ascii_lowercase();
    for pat in FORBIDDEN_PATTERNS {
        if hay.contains(&pat.to_ascii_lowercase()) {
            return Err(AppError::Forbidden(format!(
                "operation blocked by hard-forbidden list (PRD §3.3): matched \"{pat}\""
            )));
        }
    }
    Ok(())
}

/// Reject dangerous key combinations regardless of surrounding text. These are
/// the keyboard equivalents of the §3.3 list (Cmd+Q quit, Cmd+W close). Matched
/// on a normalized combo string.
fn guard_key_combo(combo: &str) -> Result<()> {
    let norm: String = combo
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // Normalize common aliases to "cmd".
    let norm = norm
        .replace("command", "cmd")
        .replace("meta", "cmd")
        .replace("super", "cmd")
        .replace('⌘', "cmd");
    // Forbidden: Cmd+Q (quit), Cmd+W (close window).
    const BAD: &[&str] = &["cmd+q", "cmd+w"];
    for b in BAD {
        let compact = b.replace('+', "");
        if norm == *b || norm == compact {
            return Err(AppError::Forbidden(format!(
                "key combo \"{combo}\" blocked (quit/close window, PRD §3.3)"
            )));
        }
    }
    Ok(())
}

// ============================================================================
// §3.2 permission gating — stub hook (TODO P1).
// ============================================================================

/// Risk tier of an actuating tool call (PRD §3.2). Low = auto; Mid = confirm in
/// Auto; High = always confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    Low,
    Mid,
    High,
}

/// Decide whether a tool call of a given risk tier may proceed under the active
/// permission mode (PRD §3.2). The hard-forbidden list (§3.3) is checked
/// separately and always wins.
///
/// TODO(P1): wire real human-in-the-loop confirmation. Today this only applies
/// the coarse YOLO/Auto/Cautious policy and never blocks Low-risk tools. Mid/
/// High tools under Auto/Cautious return `Err(Forbidden)` so the caller stops
/// and surfaces the plan to the user instead of silently acting.
pub fn check_permission(mode: PermissionMode, tier: RiskTier) -> Result<()> {
    let needs_confirm = match (mode, tier) {
        (PermissionMode::Yolo, RiskTier::High) => true, // §3.3: YOLO still confirms High.
        (PermissionMode::Yolo, _) => false,
        (PermissionMode::Auto, RiskTier::Low) => false,
        (PermissionMode::Auto, _) => true,
        (PermissionMode::Cautious, _) => true,
    };
    if needs_confirm {
        return Err(AppError::Forbidden(format!(
            "operation requires user confirmation under {mode:?} permission mode \
             (risk {tier:?}); confirmation flow is TODO(P1)"
        )));
    }
    Ok(())
}

// ============================================================================
// P1 tools — perception / navigation (no §3.3 risk on their own).
// ============================================================================

/// `screenshot` tool: capture + preprocess the primary screen (PRD §2.3),
/// delegating to the capture module. Quality is the model-facing screenshot
/// quality knob.
pub fn screenshot(quality: Quality) -> Result<CapturedImage> {
    capture::capture_screen(quality).map_err(AppError::from)
}

/// `get_context` tool: zero-cost foreground App / window title / URL for the
/// model's planner (PRD §5.1). Never panics.
pub fn get_context() -> AppContext {
    trigger::front_app_context()
}

/// `scroll(direction, amount)` tool: scroll the focused view vertically.
/// `amount` is in scroll "lines"/ticks; positive magnitude, direction chosen by
/// `dir`. Low risk, but still passes through `guard_intent` for uniformity.
pub fn scroll(dir: ScrollDir, amount: i32) -> Result<()> {
    guard_intent(&format!("scroll {dir:?} {amount}"))?;
    let mut enigo = new_enigo()?;
    // enigo: positive scroll = down, negative = up (vertical axis).
    let magnitude = amount.unsigned_abs() as i32;
    let length = match dir {
        ScrollDir::Down => magnitude,
        ScrollDir::Up => -magnitude,
    };
    enigo
        .scroll(length, Axis::Vertical)
        .map_err(|e| AppError::Other(anyhow::anyhow!("scroll failed: {e}")))?;
    Ok(())
}

/// `scroll_and_capture(direction)` tool: the reading-assist core (PRD §5.2/§5.3
/// strategy A — small-step scroll, settle, capture, repeat). Returns the
/// sequence of frames collected so the model can read a long article end to end.
///
/// Defaults are tuned for "scroll a page of long-form content": ~6 small steps
/// with a short settle delay before each capture to avoid motion smear.
pub fn scroll_and_capture(dir: ScrollDir, quality: Quality) -> Result<Vec<CapturedImage>> {
    const STEPS: usize = 6;
    const STEP_AMOUNT: i32 = 4; // scroll ticks per step (small, per strategy A).
    const SETTLE_MS: u64 = 180; // brief stability window before each capture.

    let mut frames = Vec::with_capacity(STEPS + 1);
    // Capture the starting frame first.
    frames.push(screenshot(quality)?);
    for _ in 0..STEPS {
        scroll(dir, STEP_AMOUNT)?;
        thread::sleep(Duration::from_millis(SETTLE_MS));
        frames.push(screenshot(quality)?);
    }
    Ok(frames)
}

// ============================================================================
// P2 actuation tools — click / type_text / key. Guarded; enigo-backed.
// ============================================================================

/// A click target. Element-first targeting (Accessibility role/title) is the
/// PRD §5.2 ideal; this milestone implements the coordinate-fallback path. The
/// coordinates are in REAL screen pixels — callers that have model-space
/// coordinates must restore them via `CapturedImage::scale_*` first.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum ClickTarget {
    /// Real-screen-pixel coordinate fallback (PRD §5.2 coordinate path).
    Coord { x: i32, y: i32 },
    /// Accessibility element by description. TODO(P2): resolve via AX tree;
    /// today this is rejected so we never silently mis-click.
    Element { description: String },
}

/// `click(target)` tool (PRD §5.1). Element targeting is preferred but not yet
/// implemented; coordinate clicks work via enigo. Mid risk (PRD §3.2).
pub fn click(target: ClickTarget) -> Result<()> {
    match target {
        ClickTarget::Coord { x, y } => {
            guard_intent(&format!("click at screen coordinate {x},{y}"))?;
            let mut enigo = new_enigo()?;
            enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| AppError::Other(anyhow::anyhow!("move_mouse failed: {e}")))?;
            enigo
                .button(Button::Left, Direction::Click)
                .map_err(|e| AppError::Other(anyhow::anyhow!("click failed: {e}")))?;
            Ok(())
        }
        ClickTarget::Element { description } => {
            // Still guard, so a forbidden element description (e.g. "Pay") is
            // refused even though resolution isn't implemented.
            guard_intent(&format!("click element {description}"))?;
            Err(AppError::Other(anyhow::anyhow!(
                "element-based click not yet implemented; TODO(P2) Accessibility targeting"
            )))
        }
    }
}

/// `type_text(text)` tool (PRD §5.1). Prefers clipboard paste + restore over raw
/// keystrokes for IME/CJK robustness; falls back to direct keystrokes if the
/// clipboard path is unavailable. Mid risk (PRD §3.2).
pub fn type_text(text: &str) -> Result<()> {
    guard_intent(&format!("type text: {text}"))?;
    // Preferred path: clipboard paste + restore (PRD §5.1 note). The per-OS
    // implementation lives in `crate::platform::clipboard` — macOS uses
    // `pbcopy`/`pbpaste` + `Cmd+V`, Windows uses `clip.exe`/PowerShell +
    // `Ctrl+V`. On any platform the function returns Err and we fall through
    // to the raw-keystroke path.
    if crate::platform::paste_via_clipboard(text).is_ok() {
        return Ok(());
    }
    // Fallback: direct unicode keystrokes (may be unreliable with some IMEs).
    let mut enigo = new_enigo()?;
    enigo
        .text(text)
        .map_err(|e| AppError::Other(anyhow::anyhow!("type_text (keystroke) failed: {e}")))?;
    Ok(())
}

/// `key(combo)` tool: send a key combo such as "Tab", "Enter", "Cmd+V"
/// (PRD §5.1). Rejects forbidden combos (Cmd+Q / Cmd+W) via `guard_key_combo`.
/// Mid risk (PRD §3.2).
pub fn key(combo: &str) -> Result<()> {
    guard_key_combo(combo)?;
    guard_intent(&format!("press key combo {combo}"))?;
    let mut enigo = new_enigo()?;
    let (mods, main) = parse_combo(combo)?;
    // Press modifiers down, click the main key, release modifiers.
    for m in &mods {
        enigo
            .key(*m, Direction::Press)
            .map_err(|e| AppError::Other(anyhow::anyhow!("modifier press failed: {e}")))?;
    }
    let click_result = enigo.key(main, Direction::Click);
    // Always release modifiers even if the main click errored.
    for m in mods.iter().rev() {
        let _ = enigo.key(*m, Direction::Release);
    }
    click_result.map_err(|e| AppError::Other(anyhow::anyhow!("key click failed: {e}")))?;
    Ok(())
}

// ============================================================================
// Internal helpers.
// ============================================================================

/// Construct an Enigo instance. Errors map to a generic tool error (e.g. when
/// Accessibility permission hasn't been granted yet).
fn new_enigo() -> Result<Enigo> {
    Enigo::new(&EnigoSettings::default())
        .map_err(|e| AppError::Other(anyhow::anyhow!("input backend unavailable: {e}")))
}

/// Parse a "Mod+Mod+Key" combo into (modifier keys, main key) for enigo.
/// Recognizes Cmd/Command/Meta, Ctrl/Control, Alt/Option, Shift, plus named
/// keys (Tab, Enter/Return, Esc, Space, arrows) and single characters.
fn parse_combo(combo: &str) -> Result<(Vec<enigo::Key>, enigo::Key)> {
    use enigo::Key;
    let parts: Vec<&str> = combo.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(AppError::Other(anyhow::anyhow!("empty key combo")));
    }
    let mut mods = Vec::new();
    let main_raw = *parts.last().unwrap();
    for &p in &parts[..parts.len() - 1] {
        let m = match p.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "⌘" => Key::Meta,
            "ctrl" | "control" | "^" => Key::Control,
            "alt" | "option" | "opt" | "⌥" => Key::Alt,
            "shift" | "⇧" => Key::Shift,
            other => {
                return Err(AppError::Other(anyhow::anyhow!(
                    "unknown modifier in combo: {other}"
                )))
            }
        };
        mods.push(m);
    }
    let main = parse_named_key(main_raw)?;
    Ok((mods, main))
}

/// Map a single key token to an enigo `Key`.
fn parse_named_key(token: &str) -> Result<enigo::Key> {
    use enigo::Key;
    let key = match token.to_ascii_lowercase().as_str() {
        "tab" => Key::Tab,
        "enter" | "return" => Key::Return,
        "esc" | "escape" => Key::Escape,
        "space" | "spacebar" => Key::Space,
        "backspace" | "delete" => Key::Backspace,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        s if s.chars().count() == 1 => Key::Unicode(s.chars().next().unwrap()),
        other => {
            return Err(AppError::Other(anyhow::anyhow!(
                "unsupported key token: {other}"
            )))
        }
    };
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_intents_are_rejected() {
        assert!(guard_intent("rm -rf /Users/john/important").is_err());
        assert!(guard_intent("please pay now with my card").is_err());
        assert!(guard_intent("open 1Password and reveal password").is_err());
        assert!(guard_intent("brew install something").is_err());
        assert!(guard_intent("关机").is_err());
    }

    #[test]
    fn benign_intents_pass() {
        assert!(guard_intent("scroll down 3").is_ok());
        assert!(guard_intent("click the search box").is_ok());
        assert!(guard_intent("type hello world").is_ok());
    }

    #[test]
    fn dangerous_key_combos_are_rejected() {
        assert!(guard_key_combo("Cmd+Q").is_err());
        assert!(guard_key_combo("cmd + w").is_err());
        assert!(guard_key_combo("Cmd+V").is_ok());
        assert!(guard_key_combo("Tab").is_ok());
    }

    #[test]
    fn combo_parsing() {
        let (mods, _main) = parse_combo("Cmd+Shift+S").unwrap();
        assert_eq!(mods.len(), 2);
        assert!(parse_combo("Tab").unwrap().0.is_empty());
    }

    #[test]
    fn permission_tiers() {
        assert!(check_permission(PermissionMode::Yolo, RiskTier::Low).is_ok());
        assert!(check_permission(PermissionMode::Yolo, RiskTier::High).is_err());
        assert!(check_permission(PermissionMode::Auto, RiskTier::Low).is_ok());
        assert!(check_permission(PermissionMode::Auto, RiskTier::Mid).is_err());
        assert!(check_permission(PermissionMode::Cautious, RiskTier::Low).is_err());
    }
}
