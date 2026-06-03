//! OpenAI-compatible streaming chat client (PRD §1.5, §1.3, §2.3).
//!
//! This is the single network boundary that talks to the model backend. It
//! speaks the OpenAI `/chat/completions` protocol with `stream:true`, parses
//! the Server-Sent-Events response, and feeds each `delta.content` chunk to a
//! caller-supplied `on_token` closure so the mascot bubble can type out tokens
//! as they arrive.
//!
//! Hard rules baked in (PRD §1.5):
//! - **Never disable TLS verification.** The reqwest client is built with the
//!   default secure verifier; there is no `danger_accept_invalid_certs` path.
//!   If a backend (e.g. the private 4090 endpoint) has an incomplete chain,
//!   the fix is server-side, not here.
//! - Read `delta.content` (the final answer), **never** `delta.reasoning`
//!   (the thinking-mode trace) — some backends force-emit reasoning and it
//!   must not leak into the bubble.
//! - The API key is taken from `Config` (which itself may have been filled
//!   from `PEEKY_API_KEY`); nothing is hardcoded.

use anyhow::{anyhow, Context as _, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::types::{ChatMessage, Config, ReasoningEffort};

/// Merge reasoning/thinking-control keys into a chat body, based on the user's
/// effort setting. Many providers default reasoning ON (adds latency); this lets
/// the user dial it down. We send the standard `reasoning_effort` PLUS a few
/// provider-specific keys (OpenRouter `reasoning`, Qwen `enable_thinking`,
/// DeepSeek/Step `thinking`) — servers ignore keys they don't recognize.
///
/// NOTE: `Off` sends `reasoning_effort:"none"` which some strict servers (e.g.
/// StepFun, which only documents low/medium/high) may reject; the default is
/// `Low`, so the out-of-box path never hits that.
fn apply_reasoning(body: &mut Value, effort: ReasoningEffort) {
    let frag = match effort {
        ReasoningEffort::Off => json!({
            "reasoning_effort": "none",
            "reasoning": { "enabled": false },
            "enable_thinking": false,
            "thinking": { "type": "disabled" },
            "chat_template_kwargs": { "enable_thinking": false },
        }),
        ReasoningEffort::Low => json!({ "reasoning_effort": "low", "reasoning": { "effort": "low" } }),
        ReasoningEffort::Medium => {
            json!({ "reasoning_effort": "medium", "reasoning": { "effort": "medium" } })
        }
        ReasoningEffort::High => json!({ "reasoning_effort": "high", "reasoning": { "effort": "high" } }),
    };
    if let (Some(obj), Some(extra)) = (body.as_object_mut(), frag.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
}

/// Shared HTTP client, built once. Reusing it keeps the TLS connection to the
/// model endpoint warm across calls (connection pooling / keep-alive), so repeat
/// requests skip a fresh handshake — a meaningful latency win for back-to-back
/// quick-shortcut calls. TLS verification stays ON (never weakened — PRD §1.5).
fn http_client() -> Result<reqwest::Client> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder()
        .build()
        .context("failed to build HTTP client")?;
    let _ = CLIENT.set(c.clone());
    Ok(c)
}

/// Build the vision-style user message that carries the screenshot (PRD §2.3:
/// "text first, then image" for higher grounding accuracy).
///
/// `png_base64` is the raw base64 of the PNG (no `data:` prefix) as produced by
/// [`crate::capture::capture_screen`]; we wrap it in a `data:` URI here.
///
/// The resulting `content` is the OpenAI vision array:
/// ```json
/// [
///   { "type": "text", "text": "<instruction>" },
///   { "type": "image_url", "image_url": { "url": "data:image/png;base64,<...>" } }
/// ]
/// ```
pub fn vision_user_message(instruction: &str, png_base64: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: json!([
            { "type": "text", "text": instruction },
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/png;base64,{png_base64}")
                }
            }
        ]),
    }
}

/// POST a streaming chat-completion request and drive `on_token` with each text
/// chunk as it streams in.
///
/// Returns `(full_text, prompt_tokens, completion_tokens)`. If the server sends
/// a `usage` block (some do on the final SSE frame when
/// `stream_options.include_usage` is honored), those exact counts are used;
/// otherwise both token counts are estimated locally so [`crate::types::TokenStats`]
/// stays roughly meaningful (PRD §1.4 cost visibility).
///
/// `on_token` is `FnMut(&str)` so the caller can mutate UI state (it is invoked
/// once per non-empty `delta.content` chunk, in stream order).
pub async fn stream_chat(
    config: &Config,
    messages: Vec<ChatMessage>,
    mut on_token: impl FnMut(&str),
) -> Result<(String, u64, u64)> {
    let url = build_endpoint(&config.api_base_url);

    // Body per OpenAI `/chat/completions`. `stream_options.include_usage` asks
    // compatible servers to append a final usage frame; servers that ignore it
    // just won't send one and we fall back to estimation.
    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    apply_reasoning(&mut body, config.reasoning_effort);

    // Shared client (warm keep-alive). TLS verification ON (PRD §1.5).
    let client = http_client()?;

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);

    // Only attach Authorization when a key is present; some local backends
    // (vLLM/Ollama) accept anonymous requests.
    if !config.api_key.is_empty() {
        req = req.bearer_auth(&config.api_key);
    }

    let resp = req
        .send()
        .await
        .context("request to chat-completions endpoint failed")?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "chat-completions returned {}: {}",
            status,
            truncate(&detail, 500)
        ));
    }

    let mut full_text = String::new();
    let mut prompt_tokens: Option<u64> = None;
    let mut completion_tokens: Option<u64> = None;

    // SSE frames can be split across byte chunks; buffer until we see a newline.
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error while reading response stream")?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // Process every complete line currently in the buffer; keep the
        // trailing partial line for the next iteration.
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\r', '\n']).trim();
            if line.is_empty() {
                continue;
            }
            // SSE comments / non-data lines (e.g. ":" keepalives) are ignored.
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                // Drain to end is fine; nothing meaningful follows.
                buf.clear();
                break;
            }

            let parsed: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                // Tolerate the occasional malformed/partial frame rather than
                // aborting a whole utterance.
                Err(_) => continue,
            };

            // Pull the streamed text delta. Use `delta.content`, NOT
            // `delta.reasoning` (PRD §1.5 thinking-mode note).
            if let Some(piece) = parsed
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c0| c0.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !piece.is_empty() {
                    full_text.push_str(piece);
                    on_token(piece);
                }
            }

            // Some non-streaming-compatible servers echo the full message on a
            // single frame under `choices[0].message.content`. Honor it too so
            // we don't silently drop output.
            if full_text.is_empty() {
                if let Some(msg) = parsed
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c0| c0.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !msg.is_empty() {
                        full_text.push_str(msg);
                        on_token(msg);
                    }
                }
            }

            if full_text.len() > 100_000 {
                return Err(anyhow!("response exceeded maximum allowed length"));
            }

            // Capture usage if the server provides it (often on the final frame).
            if let Some(usage) = parsed.get("usage").filter(|u| !u.is_null()) {
                if let Some(pt) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                    prompt_tokens = Some(pt);
                }
                if let Some(ct) = usage.get("completion_tokens").and_then(Value::as_u64) {
                    completion_tokens = Some(ct);
                }
            }
        }
    }

    // Fall back to local estimates when the backend omitted usage.
    let prompt_tokens = prompt_tokens.unwrap_or_else(|| estimate_prompt_tokens(&messages));
    let completion_tokens = completion_tokens.unwrap_or_else(|| estimate_tokens(&full_text));

    Ok((full_text, prompt_tokens, completion_tokens))
}

/// One tool call requested by the model (OpenAI `tool_calls` entry).
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Parsed arguments object (the API delivers it as a JSON string).
    pub arguments: Value,
}

/// The model's reply on a tool-enabled turn: either a final `content` answer, or
/// a set of `tool_calls` to execute (or both). `assistant_message` is the raw
/// message object to append back to the conversation before adding tool results.
#[derive(Debug, Clone)]
pub struct AssistantTurn {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub assistant_message: Value,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Non-streaming chat turn WITH tool definitions, for the copilot agent loop
/// (PRD §3.1-C / §5). Returns the assistant's `content` and/or `tool_calls`.
///
/// `messages` are raw JSON values (so they can carry `tool_calls` on assistant
/// turns and `role:"tool"` results); `tools` is the OpenAI tool/function array.
/// Streaming is intentionally off here — we need the full tool-call list before
/// acting. The final natural-language answer is shown via the bubble typewriter.
pub async fn chat_with_tools(
    config: &Config,
    messages: Vec<Value>,
    tools: &[Value],
) -> Result<AssistantTurn> {
    let url = build_endpoint(&config.api_base_url);

    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "stream": false,
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    apply_reasoning(&mut body, config.reasoning_effort);

    let client = http_client()?;
    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);
    if !config.api_key.is_empty() {
        req = req.bearer_auth(&config.api_key);
    }

    let resp = req
        .send()
        .await
        .context("request to chat-completions endpoint failed")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "chat-completions returned {}: {}",
            status,
            truncate(&text, 500)
        ));
    }

    let parsed: Value =
        serde_json::from_str(&text).context("endpoint returned a non-JSON response")?;
    let message = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("message"))
        .cloned()
        .ok_or_else(|| anyhow!("response had no choices[0].message"))?;

    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let mut tool_calls = Vec::new();
    if let Some(arr) = message.get("tool_calls").and_then(Value::as_array) {
        for tc in arr {
            let id = tc.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args_str = func
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
            if !name.is_empty() {
                tool_calls.push(ToolCall { id, name, arguments });
            }
        }
    }

    let prompt_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| content.as_deref().map(estimate_tokens).unwrap_or(0));

    Ok(AssistantTurn {
        content,
        tool_calls,
        assistant_message: message,
        prompt_tokens,
        completion_tokens,
    })
}

/// Lightweight connectivity check used by the `test_api_connection` command
/// (PRD §8.2 "Test connection" button). Sends a tiny non-streaming request and
/// reports success or a human-readable failure. Returns the model's reply text
/// (trimmed) on success so the UI can show something concrete.
pub async fn test_connection(config: &Config) -> Result<String> {
    let url = build_endpoint(&config.api_base_url);
    let body = json!({
        "model": config.model,
        "messages": [
            { "role": "user", "content": "ping" }
        ],
        "max_tokens": 8,
        "temperature": 0.0,
        "stream": false,
    });

    let client = http_client()?;

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);
    if !config.api_key.is_empty() {
        req = req.bearer_auth(&config.api_key);
    }

    let resp = req
        .send()
        .await
        .context("could not reach the configured endpoint")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("endpoint returned {}: {}", status, truncate(&text, 300)));
    }

    let parsed: Value =
        serde_json::from_str(&text).context("endpoint returned a non-JSON response")?;
    let reply = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(reply)
}

/// Join the configured base URL with the chat-completions path, tolerating a
/// trailing slash. We do not invent a `/v1` segment — the user's `api_base_url`
/// is expected to already include it (PRD §1.5 example values).
fn build_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    format!("{trimmed}/chat/completions")
}

/// Rough token estimate (~4 chars/token heuristic) used only when the backend
/// does not return real usage. Intentionally cheap, not exact.
fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

/// Estimate prompt tokens by summing the textual content of all messages.
/// Images are billed very differently per backend; we add a flat nominal cost
/// per image so the figure isn't wildly low when usage is missing (PRD §1.4
/// noted ~1500 tokens for a downsampled frame).
fn estimate_prompt_tokens(messages: &[ChatMessage]) -> u64 {
    let mut total = 0u64;
    for m in messages {
        match &m.content {
            Value::String(s) => total += estimate_tokens(s),
            Value::Array(parts) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                total += estimate_tokens(t);
                            }
                        }
                        Some("image_url") => total += 1500,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    total
}

/// Truncate a string to at most `max` chars for error messages, appending an
/// ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_handles_trailing_slash() {
        assert_eq!(
            build_endpoint("https://x.com/v1"),
            "https://x.com/v1/chat/completions"
        );
        assert_eq!(
            build_endpoint("https://x.com/v1/"),
            "https://x.com/v1/chat/completions"
        );
    }

    #[test]
    fn vision_message_shape() {
        let m = vision_user_message("find the button", "AAAA");
        assert_eq!(m.role, "user");
        let arr = m.content.as_array().expect("array content");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "find the button");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(
            arr[1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn estimates_are_nonzero() {
        assert!(estimate_tokens("hello world") >= 1);
        let msgs = vec![
            ChatMessage::text("system", "you are a bot"),
            vision_user_message("look", "BBBB"),
        ];
        // Image flat cost dominates.
        assert!(estimate_prompt_tokens(&msgs) >= 1500);
    }
}
