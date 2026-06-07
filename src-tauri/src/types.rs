//! Shared types used across every Peeky backend module and serialized to the
//! frontend. Every other module imports these via `crate::types::*`.
//!
//! Serde conventions are load-bearing: the frontend (TypeScript) and the API
//! layer both depend on the exact JSON shapes defined here. Do not change
//! `rename_all` / field names without updating the frontend contract.

use serde::{Deserialize, Serialize};

/// The three built-in personality modes (PRD §3.1).
/// Serialized lowercase: "roast" | "nerd" | "copilot".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeKind {
    Roast,
    Nerd,
    Copilot,
}

impl ModeKind {
    /// Cycle to the next mode (used by the Ctrl+Shift+M shortcut, PRD §8.1).
    pub fn next(self) -> Self {
        match self {
            ModeKind::Roast => ModeKind::Nerd,
            ModeKind::Nerd => ModeKind::Copilot,
            ModeKind::Copilot => ModeKind::Roast,
        }
    }
}

/// Permission / autonomy level for the "doing" side (PRD §3.2).
/// Serialized lowercase: "yolo" | "auto" | "cautious".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Yolo,
    Auto,
    Cautious,
}

/// How hard the model "thinks" before answering (reasoning/CoT effort). Many
/// modern models (StepFun step-3.7-flash, OpenAI GPT-5/o-series, Qwen3, DeepSeek)
/// default reasoning ON, which adds latency — this lets the user trade speed for
/// quality. Sent to the API as `reasoning_effort` (+ provider-specific keys).
/// Serialized lowercase: "off" | "low" | "medium" | "high".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Off,
    Low,
    Medium,
    High,
}

/// UI / output language. `Auto` resolves from the system locale at runtime.
/// Serialized lowercase: "auto" | "en" | "zh" | "ja".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Auto,
    En,
    Zh,
    Ja,
}

/// A concrete resolved language (never `Auto`). Used by the prompt builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedLang {
    En,
    Zh,
    Ja,
}

impl ResolvedLang {
    /// Map a system-locale string (e.g. "zh-Hans-CN", "ja_JP", "en-US") to a
    /// concrete language, defaulting to English for anything unrecognized.
    pub fn from_locale(locale: &str) -> Self {
        let lc = locale.to_ascii_lowercase();
        if lc.starts_with("zh") {
            ResolvedLang::Zh
        } else if lc.starts_with("ja") {
            ResolvedLang::Ja
        } else {
            ResolvedLang::En
        }
    }

    /// Short code used by both the prompt builder and the JS i18n layer.
    pub fn code(self) -> &'static str {
        match self {
            ResolvedLang::En => "en",
            ResolvedLang::Zh => "zh",
            ResolvedLang::Ja => "ja",
        }
    }
}

impl Language {
    /// Resolve a (possibly `Auto`) language into a concrete one. For `Auto`,
    /// the system locale is read via `sys-locale`; failure falls back to En.
    pub fn resolve(&self) -> ResolvedLang {
        match self {
            Language::En => ResolvedLang::En,
            Language::Zh => ResolvedLang::Zh,
            Language::Ja => ResolvedLang::Ja,
            Language::Auto => {
                let locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
                ResolvedLang::from_locale(&locale)
            }
        }
    }
}

/// Generic low/medium/high quality knob, reused for sensitivity and screenshot
/// quality. Serialized lowercase: "low" | "med" | "high".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Med,
    High,
}

/// A custom quiet-hours window (PRD §4.1). Times are local "HH:MM" strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub enabled: bool,
    /// Start time, "HH:MM" 24h local.
    pub start: String,
    /// End time, "HH:MM" 24h local. May wrap past midnight (e.g. 22:00 -> 09:00).
    pub end: String,
}

impl Default for QuietHours {
    fn default() -> Self {
        QuietHours {
            enabled: false,
            start: "22:00".to_string(),
            end: "09:00".to_string(),
        }
    }
}

/// The full user configuration (PRD §1.5 / §8.2). Persisted to
/// `config_dir()/peeky/config.json` and synced to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OpenAI-compatible base URL, e.g. "https://api.stepfun.com/v1".
    pub api_base_url: String,
    /// API key. May be empty here and supplied via PEEKY_API_KEY env instead
    /// (PRD §1.5 secret rule — never hardcode private keys).
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Reasoning/thinking effort. Defaults to `Low` (fast, works everywhere);
    /// `#[serde(default)]` so older saved configs without this field still load.
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: ReasoningEffort,
    pub mode: ModeKind,
    pub permission_mode: PermissionMode,
    pub language: Language,
    /// Change-detection sensitivity (maps to pHash thresholds in trigger.rs).
    pub sensitivity: Quality,
    /// Max proactive utterances per hour (restraint budget, PRD §4.1).
    pub speech_budget_per_hour: u32,
    /// Downsample quality for screenshots sent to the model (PRD §2.3).
    pub screenshot_quality: Quality,
    pub quiet_hours: QuietHours,
    /// Follow the macOS Focus / Do-Not-Disturb state (PRD §4.1, default on).
    pub follow_system_dnd: bool,
    /// Whether the settings UI shows the token-usage stats (PRD §8.2).
    pub show_token_stats: bool,
}

/// Default reasoning effort for new configs + missing-field migration: `Low` is
/// the documented fast floor that every reasoning provider accepts (StepFun only
/// supports low/medium/high), so it never errors and keeps replies quick.
fn default_reasoning_effort() -> ReasoningEffort {
    ReasoningEffort::Low
}

impl Default for Config {
    /// PRD §1.5 default-fill, ready-to-run out of the box. The API key is left
    /// empty on purpose: it must come from the user or PEEKY_API_KEY.
    fn default() -> Self {
        Config {
            api_base_url: "https://api.stepfun.com/v1".to_string(),
            api_key: String::new(),
            model: "step-3.7-flash".to_string(),
            // 512 (not 300) so models that emit a hidden "thinking" pass before
            // the real answer don't exhaust the budget and return empty content
            // (PRD §1.5). Replies are still naturally short; this is only a cap.
            max_tokens: 512,
            temperature: 0.7,
            reasoning_effort: ReasoningEffort::Low,
            mode: ModeKind::Roast,
            permission_mode: PermissionMode::Auto,
            language: Language::Auto,
            sensitivity: Quality::Med,
            speech_budget_per_hour: 6,
            screenshot_quality: Quality::Med,
            quiet_hours: QuietHours::default(),
            follow_system_dnd: true,
            show_token_stats: true,
        }
    }
}

/// A captured + preprocessed screenshot ready to send to the model (PRD §2.3).
/// `scale_*` map sent-image pixels back to real screen pixels for click restore:
/// `screen_px = api_px * scale`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedImage {
    /// PNG bytes, base64-encoded (no data: URI prefix).
    pub png_base64: String,
    pub sent_w: u32,
    pub sent_h: u32,
    /// Maps a sent-image x to LOGICAL screen pixels: `screen_x = origin_x + api_x * scale_x`.
    pub scale_x: f64,
    /// Maps a sent-image y to LOGICAL screen pixels: `screen_y = origin_y + api_y * scale_y`.
    pub scale_y: f64,
    /// Logical-screen coordinate of the captured region's top-left. 0,0 for a
    /// full-screen capture; the window's origin for a single-window capture.
    #[serde(default)]
    pub origin_x: f64,
    #[serde(default)]
    pub origin_y: f64,
}

/// Zero-cost foreground context used by the event-driven gate (PRD §2.2 L0).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppContext {
    pub app: String,
    pub title: String,
    /// Best-effort current URL for browsers; None otherwise.
    pub url: Option<String>,
}

/// Result of the cheap pixel-level trigger evaluation (PRD §2.2 L1).
/// `Meaningful` carries the foreground context captured alongside the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum TriggerDecision {
    /// Nothing changed enough to act on.
    NoChange,
    /// A pure vertical scroll — update reading progress, do not speak.
    Scroll,
    /// A meaningful, non-scroll content change — candidate for speaking.
    Meaningful,
}

/// One message in an OpenAI-style chat request. `content` is either a plain
/// string or a vision array of `{type:"text"|"image_url", ...}` objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value,
}

impl ChatMessage {
    /// Convenience: a plain-text message.
    pub fn text(role: &str, content: &str) -> Self {
        ChatMessage {
            role: role.to_string(),
            content: serde_json::Value::String(content.to_string()),
        }
    }
}

/// Cumulative token-usage statistics shown in settings (PRD §1.4 / §8.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    /// Total model calls made.
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// How many calls returned the <SILENT> marker (no utterance shown).
    pub silent: u64,
}

/// One past utterance, persisted so the user can review history (settings →
/// History tab). `ts` is Unix epoch seconds (local clock at record time).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub ts: i64,
    /// Mode that produced it ("roast" | "nerd" | "copilot").
    pub mode: String,
    pub text: String,
    /// Foreground app when it spoke, if known (context for the review).
    #[serde(default)]
    pub app: Option<String>,
}

/// Mascot animation states (PRD §6.2). String constants kept in sync with the
/// `MascotState` type strings the frontend listens for.
pub mod mascot_state {
    pub const IDLE: &str = "idle";
    pub const SCANNING: &str = "scanning";
    pub const THINKING: &str = "thinking";
    pub const TALKING: &str = "talking";
    pub const HAS_SOMETHING: &str = "has-something";
    pub const WORKING: &str = "working";
    pub const PAUSED: &str = "paused";
}
