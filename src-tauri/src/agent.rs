//! Copilot agent layer (PRD §3.1-C / §5): the bridge between the model's
//! `tool_calls` and the real `tools::*` actuators.
//!
//! - [`tool_defs`] is the OpenAI tool/function schema we advertise to the model.
//! - [`execute_tool`] dispatches one tool call to the matching `tools::*`
//!   function, enforcing the §3.2 permission tier and (transitively, inside
//!   `tools`) the §3.3 hard-forbidden list.
//!
//! The orchestration loop itself lives in `lib.rs` (it needs the `AppHandle` to
//! emit status/state and the `AppState` for stats/memory).

use serde_json::{json, Value};

use crate::tools::{self, ClickTarget, RiskTier, ScrollDir};
use crate::types::PermissionMode;

/// The tool/function definitions advertised to the model. Kept deliberately
/// small and unambiguous — coordinates are in the SENT image's pixel space (the
/// downsampled frame the model sees); the loop restores them to real pixels.
pub fn tool_defs() -> Vec<Value> {
    vec![
        func(
            "get_context",
            "Get the foreground app name, window title and (for browsers) URL. Use this to orient before acting.",
            json!({ "type": "object", "properties": {} }),
        ),
        func(
            "scroll",
            "Scroll the focused view vertically by a few ticks.",
            json!({
                "type": "object",
                "properties": {
                    "direction": { "type": "string", "enum": ["up", "down"] },
                    "amount": { "type": "integer", "description": "scroll ticks, 1-8 (default 4)" }
                },
                "required": ["direction"]
            }),
        ),
        func(
            "click",
            "Left-click at a point in the screenshot you were shown. x/y are pixel coordinates in THAT image.",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" }
                },
                "required": ["x", "y"]
            }),
        ),
        func(
            "type_text",
            "Type text into the currently focused field (e.g. the input box you can see is focused). Does NOT submit.",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        ),
        func(
            "key",
            "Press a key or combo, e.g. \"Tab\", \"Enter\", \"Cmd+V\". Never quit/close (those are blocked).",
            json!({
                "type": "object",
                "properties": { "combo": { "type": "string" } },
                "required": ["combo"]
            }),
        ),
        func(
            "finish",
            "Call when the task is done (or nothing should be done). Provide a short message to show the user.",
            json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            }),
        ),
    ]
}

fn func(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": { "name": name, "description": description, "parameters": parameters }
    })
}

/// i18n key (resolved on the frontend) for the status line shown while a given
/// tool runs. Drives the mascot "working" caption (PRD §6.2).
pub fn status_key(tool: &str) -> &'static str {
    match tool {
        "get_context" => "status.reading",
        "scroll" => "status.scrolling",
        "click" => "status.clicking",
        "type_text" => "status.typing",
        "key" => "status.key",
        _ => "status.acting",
    }
}

/// Risk tier for the §3.2 permission gate.
fn risk(tool: &str) -> RiskTier {
    match tool {
        "get_context" | "scroll" => RiskTier::Low,
        // click / type_text / key actuate the UI → Mid (need confirm in Auto).
        _ => RiskTier::Mid,
    }
}

/// Execute one tool call. Returns the outcome text fed back to the model on
/// success, or `Err(reason)` (also fed back) when blocked/failed. `last_scale`
/// is `(scale_x, scale_y)` from the most recent screenshot, used to restore
/// model-space click coordinates to real screen pixels (PRD §2.3).
///
/// NOTE: per-model pixel calibration (§2.3.1) is NOT applied here yet, so small
/// click targets may be imprecise — that is a tracked follow-up. Typing into an
/// already-focused field needs no coordinates and is reliable today.
pub fn execute_tool(
    name: &str,
    args: &Value,
    permission: PermissionMode,
    // (scale_x, scale_y, origin_x, origin_y) from the last screenshot, mapping
    // model-space pixels to logical screen points: screen = origin + api * scale.
    region: (f64, f64, f64, f64),
) -> std::result::Result<String, String> {
    // §3.2 permission gate (the §3.3 hard-forbidden list is enforced inside the
    // tools themselves). Low runs everywhere; Mid needs YOLO (until the confirm
    // dialog lands).
    tools::check_permission(permission, risk(name), name, args).map_err(|e| e.to_string())?;

    match name {
        "get_context" => {
            let c = tools::get_context();
            Ok(format!(
                "app=\"{}\" title=\"{}\" url=\"{}\"",
                c.app,
                c.title,
                c.url.unwrap_or_default()
            ))
        }
        "scroll" => {
            let dir = match args.get("direction").and_then(Value::as_str) {
                Some("up") => ScrollDir::Up,
                _ => ScrollDir::Down,
            };
            let amount = args.get("amount").and_then(Value::as_i64).unwrap_or(4) as i32;
            tools::scroll(dir, amount).map_err(|e| e.to_string())?;
            Ok(format!("scrolled {dir:?} {amount}"))
        }
        "click" => {
            let ix = args.get("x").and_then(Value::as_f64).unwrap_or(-1.0);
            let iy = args.get("y").and_then(Value::as_f64).unwrap_or(-1.0);
            if ix < 0.0 || iy < 0.0 {
                return Err("click needs integer x and y".to_string());
            }
            // Restore model-space coords to logical screen points (add the
            // captured window's origin so window-relative coords land correctly).
            let (sx, sy, ox, oy) = region;
            let rx = (ox + ix * sx).round() as i32;
            let ry = (oy + iy * sy).round() as i32;
            tools::click(ClickTarget::Coord { x: rx, y: ry }).map_err(|e| e.to_string())?;
            Ok(format!("clicked ({rx},{ry})"))
        }
        "type_text" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return Err("type_text needs non-empty text".to_string());
            }
            tools::type_text(text).map_err(|e| e.to_string())?;
            Ok(format!("typed \"{}\"", truncate(text, 60)))
        }
        "key" => {
            let combo = args.get("combo").and_then(Value::as_str).unwrap_or("");
            if combo.is_empty() {
                return Err("key needs a combo".to_string());
            }
            tools::key(combo).map_err(|e| e.to_string())?;
            Ok(format!("pressed {combo}"))
        }
        other => Err(format!("unknown tool \"{other}\"")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
