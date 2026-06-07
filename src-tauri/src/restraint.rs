//! The restraint engine (PRD §4 "克制引擎") — the module that keeps Peeky from
//! becoming Clippy. Speaking proactively is gated through [`RestraintEngine::allow_speak`],
//! which combines every built-in restraint mechanism from PRD §4.1:
//!
//!   * **Speech budget** — at most `N` proactive utterances per rolling hour
//!     (`Config::speech_budget_per_hour`, default 6). Over budget → stay silent
//!     and let it accumulate for the next window.
//!   * **Quiet hours** — a user-defined `HH:MM`–`HH:MM` window (overnight-aware).
//!   * **Follow system Focus / DND** — best-effort read of the system focus /
//!     do-not-disturb state; when the system is in DND, the mascot shuts up.
//!     If the state can't be read, it's treated as "not in DND" (fail-open to
//!     speaking is wrong here — but per PRD we only *add* restraint when we're
//!     sure, so unknown = off).
//!   * **Fullscreen / meeting auto-pause** — if the frontmost app is a meeting
//!     or presentation app (Zoom / Teams / 腾讯会议 / Keynote presenting / …),
//!     auto-pause.
//!   * **Ignore-decay (frequency capping)** — after `K` consecutive ignored
//!     utterances, raise the bar (require a cooldown between utterances);
//!     the moment the user interacts, decay resets.
//!
//! `allow_speak` is **only** for the proactive loop. User-initiated triggers
//! (shortcut / double-click) bypass this entirely (PRD §4.1 "主动优先") — the
//! caller simply doesn't consult the engine in that path.
//!
//! Timekeeping uses `chrono::Local::now()` throughout (PRD note).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use std::process::Command;

use chrono::{Local, NaiveTime, Timelike};

use crate::types::{AppContext, Config};

/// Consecutive ignores before ignore-decay kicks in (PRD §4.1 frequency cap).
const IGNORE_DECAY_THRESHOLD: u32 = 3;

/// Extra minimum gap between utterances once ignore-decay is active. Scales up
/// with how many extra ignores past the threshold we've seen, capped.
const DECAY_BASE_COOLDOWN: Duration = Duration::from_secs(60);
const DECAY_MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// How long to cache a (relatively expensive) system-DND probe so we don't
/// shell out on every 500ms tick.
const DND_CACHE_TTL: Duration = Duration::from_secs(10);

/// App-name fragments (lowercased) that mean "the user is in a meeting or
/// presenting — do not interrupt" (PRD §4.1). Matched as substrings against the
/// frontmost app name from [`AppContext`].
///
/// Cross-platform: the macOS-specific entries (`keynote`, `quicktime player`)
/// are harmless on Windows because substring matching only flags positives,
/// never negatives, and these names are unique to macOS. Windows entries
/// (`powerpoint`, `wpp`, `wps presentation`, `impress`, `vlc`, `zoom.exe`,
/// `teams.exe`) are what the model actually needs to catch on that OS.
const MEETING_APP_FRAGMENTS: &[&str] = &[
    // Cross-platform
    "zoom",
    "zoom.exe",
    "microsoft teams",
    "teams",
    "teams.exe",
    "腾讯会议",
    "tencent meeting",
    "wemeet",
    "webex",
    "google meet",
    "feishu meeting",
    "飞书会议",
    "lark",
    // Presentations
    "keynote", // macOS-only
    "powerpoint",
    "powerpoint slide show",
    "wpp", // WPS Presentation on Windows
    "wps presentation",
    "impress", // LibreOffice
    "slideshow",
    // Fullscreen video / playback
    "quicktime player", // macOS-only
    "vlc",
];

/// The core restraint state machine. One instance lives behind a mutex in
/// `AppState`.
pub struct RestraintEngine {
    /// Timestamps (monotonic) of recent *shown* proactive utterances, used for
    /// the rolling-hour speech budget. Pruned to the last hour on each check.
    spoken_at: VecDeque<Instant>,
    /// Consecutive ignores since the last interaction (ignore-decay counter).
    consecutive_ignores: u32,
    /// When we last actually showed an utterance (for the decay cooldown gate).
    last_shown: Option<Instant>,
    /// Cached system-DND probe: (value, when_probed).
    dnd_cache: Option<(bool, Instant)>,
}

impl RestraintEngine {
    /// Create a fresh engine with empty history.
    pub fn new() -> Self {
        RestraintEngine {
            spoken_at: VecDeque::new(),
            consecutive_ignores: 0,
            last_shown: None,
            dnd_cache: None,
        }
    }

    /// The master gate for the proactive loop. Returns `true` only if *every*
    /// restraint mechanism permits speaking right now. Never panics.
    ///
    /// Note: this does **not** mutate the budget — call [`record_shown`] when an
    /// utterance is actually displayed so the budget window advances. (The model
    /// may still return `<SILENT>` after we allow it; that path calls
    /// [`record_ignored`] instead.)
    pub fn allow_speak(&mut self, cfg: &Config, ctx: &AppContext) -> bool {
        let now = Instant::now();

        // 1) Speech budget (rolling hour). Prune, then compare.
        self.prune_budget(now);
        if cfg.speech_budget_per_hour > 0
            && self.spoken_at.len() as u32 >= cfg.speech_budget_per_hour
        {
            return false;
        }

        // 2) Quiet hours (user-defined window).
        if cfg.quiet_hours.enabled && in_quiet_hours(&cfg.quiet_hours.start, &cfg.quiet_hours.end) {
            return false;
        }

        // 3) Follow system Focus / DND (macOS Focus, Windows Focus Assist).
        if cfg.follow_system_dnd && self.system_dnd_active(now) {
            return false;
        }

        // 4) Fullscreen / meeting auto-pause.
        if is_meeting_or_fullscreen(ctx) {
            return false;
        }

        // 5) Ignore-decay cooldown: once we've been ignored K+ times in a row,
        //    enforce a growing minimum gap between utterances.
        if let Some(cooldown) = self.decay_cooldown() {
            if let Some(last) = self.last_shown {
                if now.duration_since(last) < cooldown {
                    return false;
                }
            }
        }

        true
    }

    /// Record that a proactive utterance was actually shown to the user. Advances
    /// the budget window. (Ignore-decay only resets on *interaction*, not on a
    /// mere show, so we don't reset the counter here.)
    pub fn record_shown(&mut self) {
        let now = Instant::now();
        self.spoken_at.push_back(now);
        self.last_shown = Some(now);
        self.prune_budget(now);
    }

    /// Record that the user ignored the mascot (it spoke / had-something but the
    /// user didn't engage), or that the model chose `<SILENT>`. Drives
    /// ignore-decay frequency-capping.
    pub fn record_ignored(&mut self) {
        self.consecutive_ignores = self.consecutive_ignores.saturating_add(1);
    }

    /// Record that the user interacted with the mascot (expanded the bubble,
    /// clicked, used a tool, etc.). Resets ignore-decay so frequency returns to
    /// normal (PRD §4.1 "用户开始互动 → 恢复").
    pub fn record_interacted(&mut self) {
        self.consecutive_ignores = 0;
    }

    /// Number of proactive utterances shown in the last rolling hour.
    pub fn spoken_last_hour(&self) -> u32 {
        self.spoken_at.len() as u32
    }

    /// Drop budget timestamps older than one hour.
    fn prune_budget(&mut self, now: Instant) {
        let hour = Duration::from_secs(3600);
        while let Some(&front) = self.spoken_at.front() {
            if now.duration_since(front) >= hour {
                self.spoken_at.pop_front();
            } else {
                break;
            }
        }
    }

    /// Current ignore-decay cooldown, or `None` if decay isn't active yet.
    fn decay_cooldown(&self) -> Option<Duration> {
        if self.consecutive_ignores < IGNORE_DECAY_THRESHOLD {
            return None;
        }
        // Each extra ignore past the threshold doubles the base cooldown, capped.
        let extra = self.consecutive_ignores - IGNORE_DECAY_THRESHOLD;
        let factor = 1u64 << extra.min(6); // cap the shift so we don't overflow
        let cooldown = DECAY_BASE_COOLDOWN
            .checked_mul(factor as u32)
            .unwrap_or(DECAY_MAX_COOLDOWN)
            .min(DECAY_MAX_COOLDOWN);
        Some(cooldown)
    }

    /// Best-effort read of the system Focus / Do-Not-Disturb state, cached for a
    /// few seconds. Returns `false` (not in DND) on any failure — we only ever
    /// *add* restraint when we're confident the system is in DND.
    fn system_dnd_active(&mut self, now: Instant) -> bool {
        if let Some((val, at)) = self.dnd_cache {
            if now.duration_since(at) < DND_CACHE_TTL {
                return val;
            }
        }
        let val = probe_system_dnd();
        self.dnd_cache = Some((val, now));
        val
    }
}

impl Default for RestraintEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the current local time falls inside the `[start, end)` quiet window.
/// Both args are "HH:MM" 24h. Handles overnight windows (start > end, e.g.
/// 22:00 → 09:00). Malformed times are treated as "not quiet" (fail-open to
/// speaking, since a broken config shouldn't permanently mute the app).
fn in_quiet_hours(start: &str, end: &str) -> bool {
    let (Some(start_t), Some(end_t)) = (parse_hhmm(start), parse_hhmm(end)) else {
        return false;
    };
    let now = Local::now();
    let now_min = now.hour() * 60 + now.minute();
    let start_min = start_t.hour() * 60 + start_t.minute();
    let end_min = end_t.hour() * 60 + end_t.minute();

    if start_min == end_min {
        // Zero-length / full-day ambiguity: treat as never quiet.
        false
    } else if start_min < end_min {
        // Same-day window, e.g. 13:00–14:00.
        now_min >= start_min && now_min < end_min
    } else {
        // Overnight window, e.g. 22:00–09:00.
        now_min >= start_min || now_min < end_min
    }
}

/// Parse a "HH:MM" string into a `NaiveTime`. Returns `None` if malformed.
fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Whether the frontmost app indicates a meeting / presentation / fullscreen
/// video the mascot must not interrupt (PRD §4.1). Substring match on the app
/// name, plus a heuristic on the window title for Keynote slideshow state.
fn is_meeting_or_fullscreen(ctx: &AppContext) -> bool {
    let app = ctx.app.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();

    if MEETING_APP_FRAGMENTS
        .iter()
        .any(|frag| app.contains(frag) || title.contains(frag))
    {
        return true;
    }

    // Keynote shows a normal window name while editing but enters a borderless
    // fullscreen "slideshow" while presenting; catch common presenting hints.
    if app.contains("keynote") && (title.contains("presentation") || title.is_empty()) {
        return true;
    }

    false
}

/// Best-effort probe of the system's Focus / Do-Not-Disturb state. On macOS
/// we shell out to `defaults` (cheap, no extra dependencies); on Windows we
/// return `false` in v1 because the Focus Assist state lives in a deeply
/// nested `HKCU\Software\Microsoft\Windows\CurrentVersion\CloudStore\…\Value`
/// registry key that requires the `winreg` crate to read. The
/// `follow_system_dnd` user toggle is preserved across platforms — the
/// setting just has no automatic effect on Windows until the registry read
/// is wired up (TODO `peeky-windows-2`).
///
/// We only ever *add* restraint when we're confident the system is in DND:
/// a non-readable state is treated as "not in DND" so a flaky probe never
/// permanently mutes the mascot.
#[cfg(target_os = "macos")]
fn probe_system_dnd() -> bool {
    // Newer macOS (Monterey+) keeps Focus state under the Notification Center
    // assertion store. The most portable cheap signal is the DND assertion in
    // `com.apple.controlcenter` / `com.apple.donotdisturbd`. We read a couple of
    // candidate keys and OR them.
    //
    // Each read is wrapped so a missing key/domain just yields `false`.
    let candidates: &[(&str, &str)] = &[
        ("com.apple.controlcenter", "NSStatusItem Visible FocusModes"),
        ("com.apple.donotdisturbd", "doNotDisturb"),
    ];

    for (domain, key) in candidates {
        if read_defaults_bool(domain, key) == Some(true) {
            return true;
        }
    }

    // Some macOS builds expose the active Focus via the Assertions plist; reading
    // it via `defaults` is unreliable across versions, so we stop here and treat
    // an unreadable state as "not in DND" (per the fail-closed-to-restraint rule
    // we only mute when we are sure).
    false
}

#[cfg(not(target_os = "macos"))]
fn probe_system_dnd() -> bool {
    false
}

/// Read a boolean `defaults` value, returning `None` if the domain/key is
/// missing or the value isn't clearly a bool. Never panics.
#[cfg(target_os = "macos")]
fn read_defaults_bool(domain: &str, key: &str) -> Option<bool> {
    let output = Command::new("defaults")
        .args(["read", domain, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    match s.trim() {
        "1" | "true" | "YES" => Some(true),
        "0" | "false" | "NO" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModeKind, PermissionMode, QuietHours, Quality};

    fn test_cfg(budget: u32) -> Config {
        let mut c = Config::default();
        c.speech_budget_per_hour = budget;
        c.follow_system_dnd = false; // don't shell out in unit tests
        c.quiet_hours = QuietHours {
            enabled: false,
            start: "22:00".into(),
            end: "09:00".into(),
        };
        // silence unused-import style warnings by touching enums
        let _ = (ModeKind::Roast, PermissionMode::Auto, Quality::Med);
        c
    }

    fn neutral_ctx() -> AppContext {
        AppContext {
            app: "TextEdit".into(),
            title: "untitled".into(),
            url: None,
        }
    }

    #[test]
    fn budget_blocks_after_n() {
        let mut e = RestraintEngine::new();
        let cfg = test_cfg(2);
        let ctx = neutral_ctx();
        assert!(e.allow_speak(&cfg, &ctx));
        e.record_shown();
        assert!(e.allow_speak(&cfg, &ctx));
        e.record_shown();
        // Third should be blocked: budget exhausted.
        assert!(!e.allow_speak(&cfg, &ctx));
    }

    #[test]
    fn zero_budget_means_unlimited_not_zero() {
        // Budget 0 is treated as "no limit" so the loop isn't permanently muted.
        let mut e = RestraintEngine::new();
        let cfg = test_cfg(0);
        let ctx = neutral_ctx();
        for _ in 0..50 {
            assert!(e.allow_speak(&cfg, &ctx));
            e.record_shown();
        }
    }

    #[test]
    fn meeting_app_pauses() {
        let mut e = RestraintEngine::new();
        let cfg = test_cfg(10);
        let mut ctx = neutral_ctx();
        ctx.app = "zoom.us".into();
        assert!(!e.allow_speak(&cfg, &ctx));
    }

    #[test]
    fn keynote_presenting_pauses() {
        assert!(is_meeting_or_fullscreen(&AppContext {
            app: "Keynote".into(),
            title: "".into(),
            url: None,
        }));
    }

    #[test]
    fn quiet_hours_overnight() {
        // 00:00–23:59 forced window is "always quiet" except the exact end.
        assert!(in_quiet_hours("00:00", "23:59"));
        // Reversed overnight window covering now: 00:00 start, 23:59 end already
        // tested; test wrap logic directly.
        // A window of 12:00->11:00 (overnight wrap) covers nearly all day.
        // We can't assert exact now, but parsing must succeed.
        assert!(parse_hhmm("22:00").is_some());
        assert!(parse_hhmm("9:5").is_some());
        assert!(parse_hhmm("bogus").is_none());
        assert!(parse_hhmm("25:00").is_none());
    }

    #[test]
    fn quiet_hours_equal_is_never() {
        assert!(!in_quiet_hours("10:00", "10:00"));
    }

    #[test]
    fn ignore_decay_then_recover() {
        let mut e = RestraintEngine::new();
        let cfg = test_cfg(10);
        let ctx = neutral_ctx();

        // Speak once, then get ignored enough to trigger decay.
        assert!(e.allow_speak(&cfg, &ctx));
        e.record_shown();
        for _ in 0..IGNORE_DECAY_THRESHOLD {
            e.record_ignored();
        }
        // Cooldown now active and last_shown was just now → blocked.
        assert!(e.decay_cooldown().is_some());
        assert!(!e.allow_speak(&cfg, &ctx));

        // Interaction clears the decay → allowed again immediately.
        e.record_interacted();
        assert!(e.decay_cooldown().is_none());
        assert!(e.allow_speak(&cfg, &ctx));
    }
}
