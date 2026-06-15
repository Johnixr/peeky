//! Mode system: turns a `ModeKind` + resolved language + screenshot + rolling
//! memory into an OpenAI-compatible `Vec<ChatMessage>` ready for `api::stream_chat`.
//!
//! PRD §3.1 (roast / nerd / copilot prompt designs) and §4.3 (memory slot).
//!
//! The three prompt templates live in `src-tauri/prompts/*.md` and are embedded
//! into the binary via `include_str!`, so they ship with the app and never need
//! to be read from disk at runtime. Each template is written LANGUAGE-NEUTRAL:
//! it contains a `{{LANGUAGE}}` placeholder ("Always respond in {{LANGUAGE}}"),
//! a `{{recent_memory}}` slot (PRD §4.3), the `<SILENT>` convention, and — for
//! copilot — a `{{SUB_MODE}}` placeholder plus the §3.2/§3.3 safety instruction.
//!
//! Message assembly follows PRD §2.3: **text first, then image** — the model
//! learns what to look for before it sees the picture, which improves accuracy.

use crate::types::{ChatMessage, ModeKind, ResolvedLang};

/// The exact marker a model must emit when it has nothing to say (PRD §3.1).
/// `lib.rs` compares the trimmed model output against this to decide silence.
pub const SILENT_MARKER: &str = "<SILENT>";

// Prompt templates embedded at compile time. Paths are relative to this source
// file (`src-tauri/src/modes.rs` -> `src-tauri/prompts/*.md`).
const ROAST_PROMPT: &str = include_str!("../prompts/roast.md");
const NERD_PROMPT: &str = include_str!("../prompts/nerd.md");
const COPILOT_PROMPT: &str = include_str!("../prompts/copilot.md");

// One-shot "quick shortcut" prompts (PRD-adjacent: capture → single-turn reply).
const QUICK_EXPLAIN_PROMPT: &str = include_str!("../prompts/quick_explain.md");
const QUICK_ASK_PROMPT: &str = include_str!("../prompts/quick_ask.md");
const QUICK_TRANSLATE_PROMPT: &str = include_str!("../prompts/quick_translate.md");

/// The reusable one-shot screenshot shortcuts.
/// - `Explain`: capture → preset "explain my screen" reply (no user text).
/// - `Ask`: capture → answer the user's typed question about the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickKind {
    Explain,
    Ask,
    /// Translate the selected text into the user's language + a short vocab note.
    Translate,
    /// OCR the selected region locally through an external OCR runtime.
    Ocr,
}

/// Default copilot sub-mode when the caller passes `None` (PRD §3.1-A: reading
/// assist is the P1 entry point for copilot).
const DEFAULT_SUB_MODE: &str = "reading_assist";

/// Human-readable language name injected into `{{LANGUAGE}}`. The model is told
/// to "Always respond in {{LANGUAGE}}", so this must read naturally in-prompt.
fn language_name(lang: ResolvedLang) -> &'static str {
    match lang {
        ResolvedLang::En => "English",
        ResolvedLang::Zh => "中文",
        ResolvedLang::Ja => "日本語",
    }
}

/// Pick the raw template for a mode.
fn template_for(mode: ModeKind) -> &'static str {
    match mode {
        ModeKind::Roast => ROAST_PROMPT,
        ModeKind::Nerd => NERD_PROMPT,
        ModeKind::Copilot => COPILOT_PROMPT,
    }
}

/// Substitute the placeholders in a template. Empty memory is rendered as
/// "(none)" so the model never sees a dangling `[History]` header.
fn render_prompt(
    template: &str,
    lang: ResolvedLang,
    memory: &str,
    sub_mode: Option<&str>,
) -> String {
    let memory_block = if memory.trim().is_empty() {
        "(none)"
    } else {
        memory.trim()
    };

    template
        .replace("{{LANGUAGE}}", language_name(lang))
        .replace("{{recent_memory}}", memory_block)
        .replace("{{SUB_MODE}}", sub_mode.unwrap_or(DEFAULT_SUB_MODE))
}

/// Build the OpenAI-compatible message list for one model call.
///
/// - `mode`        — which personality/copilot template to load.
/// - `lang`        — resolved output language, injected into `{{LANGUAGE}}`.
/// - `png_base64`  — preprocessed screenshot (base64 PNG, no data: prefix).
/// - `memory`      — rolling memory snapshot (`RollingMemory::recent()`).
/// - `sub_mode`    — copilot sub-mode ("reading_assist" / "input_assist" /
///                   "task_execute"); ignored by roast/nerd, defaults to
///                   reading_assist for copilot when `None`.
///
/// Returns a single `user` message whose `content` is a vision array:
/// **text first** (the rendered system/instruction prompt) **then the image**
/// (PRD §2.3). Using one user message with text-before-image keeps the ordering
/// guarantee intact across providers that reorder system vs. user roles.
pub fn build_messages(
    mode: ModeKind,
    lang: ResolvedLang,
    png_base64: &str,
    memory: &str,
    sub_mode: Option<&str>,
) -> Vec<ChatMessage> {
    let prompt = render_prompt(template_for(mode), lang, memory, sub_mode);

    // PRD §2.3: text first, then image. The data URI prefix is required by the
    // OpenAI vision `image_url` format; `png_base64` carries no prefix itself.
    let content = serde_json::json!([
        {
            "type": "text",
            "text": prompt,
        },
        {
            "type": "image_url",
            "image_url": {
                "url": format!("data:image/png;base64,{}", png_base64),
            },
        },
    ]);

    vec![ChatMessage {
        role: "user".to_string(),
        content,
    }]
}

/// Build the message list for a one-shot quick shortcut (Explain / Ask).
///
/// - `kind`     — which preset (explain vs answer-a-question).
/// - `lang`     — resolved output language, injected into `{{LANGUAGE}}`.
/// - `context`  — runtime context string (local time / active app / system),
///                injected into `{{CONTEXT}}`.
/// - `png_base64` — the captured screenshot (base64 PNG, no data: prefix).
/// - `question` — the user's typed question (Ask only; empty for Explain).
///
/// Returns a `system` message (the rendered preset) followed by a vision `user`
/// message (text-first, then image — PRD §2.3). For Ask the user text is the
/// question; for Explain it's a short fixed instruction.
pub fn build_quick_messages(
    kind: QuickKind,
    lang: ResolvedLang,
    context: &str,
    png_base64: &str,
    question: &str,
) -> Vec<ChatMessage> {
    let template = match kind {
        QuickKind::Explain => QUICK_EXPLAIN_PROMPT,
        QuickKind::Ask => QUICK_ASK_PROMPT,
        QuickKind::Translate | QuickKind::Ocr => QUICK_TRANSLATE_PROMPT,
    };
    // The system prompt is STABLE per language (only {{LANGUAGE}} is filled) so
    // its KV cache stays warm across calls. All per-call/variable content — the
    // runtime context, the user's directive, and the image — goes LAST, in the
    // user message (KV-cache friendly + keeps the directive next to the image).
    let system = template.replace("{{LANGUAGE}}", language_name(lang));

    let directive = match kind {
        QuickKind::Ask if !question.trim().is_empty() => question.trim().to_string(),
        QuickKind::Ask => "Answer my question about this screen.".to_string(),
        QuickKind::Explain => "Explain what's on my screen, concisely.".to_string(),
        QuickKind::Translate => "Translate the text in this image.".to_string(),
        QuickKind::Ocr => "Extract the text in this image.".to_string(),
    };
    let ctx = context.trim();
    let user_text = if ctx.is_empty() {
        directive
    } else {
        format!("{directive}\n\n[Context: {ctx}]")
    };

    // The quick-shortcut crop is JPEG-encoded (see capture::crop_to_captured).
    let content = serde_json::json!([
        { "type": "text", "text": user_text },
        {
            "type": "image_url",
            "image_url": { "url": format!("data:image/jpeg;base64,{}", png_base64) },
        },
    ]);

    vec![
        ChatMessage::text("system", &system),
        ChatMessage {
            role: "user".to_string(),
            content,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_messages_are_system_then_vision() {
        let msgs = build_quick_messages(
            QuickKind::Ask,
            ResolvedLang::Zh,
            "time: now",
            "QUJD",
            "what is this?",
        );
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[0].content.as_str().unwrap().contains("中文"));
        // Context lives in the USER message now (KV-cache friendly), not system.
        assert!(!msgs[0].content.as_str().unwrap().contains("time: now"));
        let arr = msgs[1].content.as_array().expect("vision array");
        let user_text = arr[0]["text"].as_str().unwrap();
        assert!(user_text.contains("what is this?"));
        assert!(user_text.contains("time: now"));
        assert_eq!(arr[1]["type"], "image_url");
    }

    #[test]
    fn quick_explain_has_no_unresolved_placeholders() {
        let msgs = build_quick_messages(QuickKind::Explain, ResolvedLang::En, "ctx", "AA", "");
        let sys = msgs[0].content.as_str().unwrap();
        assert!(!sys.contains("{{LANGUAGE}}"));
        assert!(!sys.contains("{{CONTEXT}}"));
        assert!(sys.contains("English"));
    }

    #[test]
    fn silent_marker_is_stable() {
        assert_eq!(SILENT_MARKER, "<SILENT>");
    }

    #[test]
    fn templates_have_no_unresolved_placeholders() {
        for mode in [ModeKind::Roast, ModeKind::Nerd, ModeKind::Copilot] {
            let prompt = render_prompt(template_for(mode), ResolvedLang::En, "", None);
            assert!(!prompt.contains("{{LANGUAGE}}"), "{:?} LANGUAGE", mode);
            assert!(!prompt.contains("{{recent_memory}}"), "{:?} memory", mode);
            assert!(!prompt.contains("{{SUB_MODE}}"), "{:?} sub_mode", mode);
        }
    }

    #[test]
    fn language_is_injected_per_lang() {
        let zh = render_prompt(ROAST_PROMPT, ResolvedLang::Zh, "", None);
        assert!(zh.contains("中文"));
        let ja = render_prompt(NERD_PROMPT, ResolvedLang::Ja, "", None);
        assert!(ja.contains("日本語"));
        let en = render_prompt(COPILOT_PROMPT, ResolvedLang::En, "", None);
        assert!(en.contains("English"));
    }

    #[test]
    fn each_template_keeps_silent_convention() {
        for tmpl in [ROAST_PROMPT, NERD_PROMPT, COPILOT_PROMPT] {
            assert!(tmpl.contains(SILENT_MARKER));
        }
    }

    #[test]
    fn empty_memory_renders_as_none() {
        let p = render_prompt(ROAST_PROMPT, ResolvedLang::En, "   ", None);
        assert!(p.contains("(none)"));
    }

    #[test]
    fn sub_mode_placeholder_is_substituted() {
        // Exercise render_prompt's {{SUB_MODE}} substitution directly with a
        // literal template, so the test doesn't depend on whether a *shipped*
        // prompt still carries the placeholder (copilot.md now drives sub-mode
        // via real tool-calling and no longer templates it).
        let tmpl = "mode={{SUB_MODE}} lang={{LANGUAGE}}";
        let p = render_prompt(tmpl, ResolvedLang::En, "", Some("task_execute"));
        assert!(p.contains("task_execute"));
        let default = render_prompt(tmpl, ResolvedLang::En, "", None);
        assert!(default.contains(DEFAULT_SUB_MODE));
    }

    #[test]
    fn build_messages_is_text_first_then_image() {
        let msgs = build_messages(ModeKind::Roast, ResolvedLang::En, "QUJD", "", None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        let arr = msgs[0].content.as_array().expect("vision array");
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[1]["type"], "image_url");
        let url = arr[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,QUJD"));
    }
}
