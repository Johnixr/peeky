//! Local OCR adapter for region shortcuts.
//!
//! Peeky keeps OCR out of the hot 500 ms perception loop. This module only runs
//! after the user explicitly presses the OCR shortcut and selects a region.
//! The default adapter calls a PaddleOCR-compatible CLI (`paddleocr`) so the
//! heavy OCR runtime can be installed and updated outside the Tauri binary.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;

/// Run OCR for a cropped JPEG/PNG image encoded as base64.
///
/// Defaults to:
/// `paddleocr ocr -i <temp-file> --lang ch --use_textline_orientation true`
///
/// Runtime knobs:
/// - `PEEKY_OCR_CMD`: command name/path, default `paddleocr`
/// - `PEEKY_OCR_EXTRA_ARGS`: extra whitespace-separated CLI args appended after
///   the default PaddleOCR arguments.
pub fn recognize_image_base64(image_base64: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image_base64.as_bytes())
        .context("decode selected OCR image")?;
    if bytes.is_empty() {
        return Err(anyhow!("selected OCR image is empty"));
    }

    let path = temp_image_path();
    std::fs::write(&path, &bytes).context("write OCR input image")?;
    let cleanup = ScopeCleanup(path.clone());

    let cmd = std::env::var("PEEKY_OCR_CMD").unwrap_or_else(|_| "paddleocr".to_string());
    let mut command = Command::new(&cmd);
    command
        .arg("ocr")
        .arg("-i")
        .arg(&path)
        .arg("--lang")
        .arg("ch")
        .arg("--use_textline_orientation")
        .arg("true");

    if let Ok(extra) = std::env::var("PEEKY_OCR_EXTRA_ARGS") {
        for arg in extra.split_whitespace().filter(|s| !s.is_empty()) {
            command.arg(arg);
        }
    }

    let output = command
        .output()
        .with_context(|| format!("run OCR command `{cmd}`"))?;
    drop(cleanup);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(anyhow!(
            "OCR command `{cmd}` exited with {}. stderr: {}",
            output.status,
            stderr.trim()
        ));
    }

    let combined_output = format!("{stdout}\n{stderr}");
    let text = extract_readable_text(&combined_output);
    if text.trim().is_empty() {
        return Err(anyhow!("OCR produced no text. stderr: {}", stderr.trim()));
    }
    Ok(text)
}

fn temp_image_path() -> std::path::PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    std::env::temp_dir().join(format!("peeky_ocr_{}_{}.jpg", std::process::id(), now))
}

/// PaddleOCR output differs by version. Prefer quoted text fragments from the
/// common tuple/list formats, but fall back to cleaned raw lines.
fn extract_readable_text(raw: &str) -> String {
    let rec_texts = raw
        .lines()
        .filter_map(rec_texts_fragments)
        .flatten()
        .collect();
    let rec_texts = dedupe_lines(rec_texts);
    if !rec_texts.is_empty() {
        return rec_texts.join("\n");
    }

    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if should_skip_ocr_output_line(trimmed) {
            continue;
        }

        let quoted = quoted_fragments(trimmed);
        if quoted.is_empty() {
            lines.push(trimmed.to_string());
        } else {
            lines.extend(quoted);
        }
    }
    dedupe_lines(lines).join("\n")
}

fn should_skip_ocr_output_line(line: &str) -> bool {
    line.is_empty()
        || line.contains("NotOpenSSLWarning")
        || line.contains("warnings.warn(")
        || line.starts_with("Creating model:")
        || line.starts_with("Model files already exist.")
        || line.starts_with("[")
            && (line.contains("dt_boxes")
                || line.contains("rec_res")
                || line.contains("Predict")
                || line.contains("paddleocr INFO"))
        || line.starts_with("{") && line.contains("'res':")
}

fn rec_texts_fragments(line: &str) -> Option<Vec<String>> {
    let marker = "'rec_texts':";
    let marker_start = line.find(marker)?;
    let after_marker = &line[marker_start + marker.len()..];
    let list_start = after_marker.find('[')?;
    let list = &after_marker[list_start..];
    let list_end = list.find(']')?;
    Some(quoted_fragments(&list[..=list_end]))
}

fn quoted_fragments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();
    for ch in line.chars() {
        if ch == '\'' || ch == '"' {
            if in_quote {
                let text = current.trim();
                if text.len() > 1
                    && !text.starts_with('[')
                    && !text.starts_with('{')
                    && !text.contains(".jpg")
                    && !text.contains(".png")
                {
                    out.push(text.to_string());
                }
                current.clear();
                in_quote = false;
            } else {
                in_quote = true;
            }
        } else if in_quote {
            current.push(ch);
        }
    }
    out
}

fn dedupe_lines(lines: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in lines {
        let normalized = line.trim();
        if normalized.is_empty() || !seen.insert(normalized.to_string()) {
            continue;
        }
        out.push(normalized.to_string());
    }
    out
}

struct ScopeCleanup(std::path::PathBuf);

impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::extract_readable_text;

    #[test]
    fn extracts_text_from_common_paddleocr_tuple_output() {
        let raw = "[[[1, 2], [3, 4]], ('Hello OCR', 0.98)]\n[[[1, 2]], ('第二行', 0.93)]";
        let text = extract_readable_text(raw);
        assert!(text.contains("Hello OCR"));
        assert!(text.contains("第二行"));
    }

    #[test]
    fn extracts_text_from_paddleocr_v3_rec_texts_output() {
        let raw = "{'res': {'input_path': '/tmp/image.png', 'rec_texts': ['Hello OCR', '第二行'], 'rec_scores': array([0.99, 0.98])}}";
        let text = extract_readable_text(raw);
        assert_eq!(text, "Hello OCR\n第二行");
    }

    #[test]
    fn ignores_paddleocr_v3_metadata_when_no_text_is_detected() {
        let raw = "{'res': {'input_path': '/tmp/image.png', 'rec_texts': [], 'rec_scores': array([], dtype=float64)}}";
        let text = extract_readable_text(raw);
        assert!(text.is_empty());
    }

    #[test]
    fn extracts_text_from_paddleocr_v3_stderr_with_warning() {
        let raw = "/.venv-ocr/lib/python3.9/site-packages/urllib3/__init__.py:35: NotOpenSSLWarning: urllib3 v2 only supports OpenSSL 1.1.1+\n  warnings.warn(\nCreating model: ('PP-OCRv6_medium_rec', None, None)\n[2026/06/16 03:49:30] paddleocr INFO: Processed item 0 in 2537 ms\n{'res': {'input_path': '/tmp/image.png', 'rec_texts': ['Peeky OCR'], 'rec_scores': array([0.99])}}";
        let text = extract_readable_text(raw);
        assert_eq!(text, "Peeky OCR");
    }
}
