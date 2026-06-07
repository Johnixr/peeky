//! Screen capture + preprocessing (PRD §1.3 / §2.3).
//!
//! Pipeline for [`capture_screen`]:
//!   1. Grab the primary monitor with `xcap` -> physical-pixel `RgbaImage`
//!      (on Retina this is 2× the logical size, e.g. a 1440p panel yields a
//!      2880-wide buffer).
//!   2. **DPI-normalize**: divide the physical buffer by the monitor's
//!      `backingScaleFactor` so we are reasoning in *logical* points. Skipping
//!      this is the classic "macOS 2× DPI" trap that shifts every click target
//!      (PRD §2.3).
//!   3. **Downsample** to a width chosen by [`Quality`] (Low 960 / Med 1280 /
//!      High 1600, default 1280) preserving aspect ratio — never let the API
//!      do the resize for us.
//!   4. Encode PNG -> base64 (no `data:` prefix).
//!   5. Compute `scale_x = real_screen_px / sent_px` (and `scale_y`) so click
//!      coordinates returned by the model can be restored to true screen
//!      pixels: `screen_px = api_px * scale`.
//!
//! Every public function returns `anyhow::Result` and never panics — capture
//! can legitimately fail (permissions, headless CI, display sleep) and the
//! caller (the 500 ms main loop) must degrade gracefully.

use std::io::Cursor;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use image::{DynamicImage, GenericImageView, GrayImage, ImageFormat};
use xcap::{Monitor, Window};

use crate::types::{CapturedImage, Quality};

/// Side length (px) of the normalized grayscale fingerprint produced by
/// [`to_gray_128`]. Kept here so the trigger module can rely on a fixed size.
pub const GRAY_SIZE: u32 = 128;

/// Map a [`Quality`] knob to a target downsample width in logical pixels
/// (PRD §2.3: Low 960 / Med 1280 / High 1600; project default 1280 = Med).
fn target_width(quality: Quality) -> u32 {
    match quality {
        Quality::Low => 960,
        Quality::Med => 1280,
        Quality::High => 1600,
    }
}

/// Pick the primary monitor; fall back to the first available one so a
/// misreported `is_primary` flag never leaves us with nothing to capture.
fn primary_monitor() -> Result<Monitor> {
    let monitors = Monitor::all().map_err(|e| anyhow!("xcap: failed to enumerate monitors: {e}"))?;
    if monitors.is_empty() {
        return Err(anyhow!("xcap: no monitors found"));
    }

    // Prefer the monitor that reports itself primary; tolerate per-monitor
    // query failures by treating them as "not primary".
    if let Some(m) = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .cloned()
    {
        return Ok(m);
    }

    // No monitor claimed primary — use the first one.
    monitors
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("xcap: no monitors found"))
}

/// Best-effort scale factor (backingScaleFactor) of the primary display, read
/// from display METADATA (not screen content), so it works even when actual
/// pixel capture is denied/black. Defaults to 2.0 (Retina) if unavailable.
fn primary_scale_factor() -> f64 {
    primary_monitor()
        .ok()
        .and_then(|m| m.scale_factor().ok())
        .map(|s| s as f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(2.0)
}

/// Grab the primary display as a raw physical-pixel image, plus its scale factor.
///
/// Order matters: try in-process **xcap FIRST**. With a properly-signed app that
/// holds Screen Recording, xcap (CGDisplayCreateImage) returns real pixels and is
/// fast. The macOS `screencapture` CLI is only a FALLBACK — as a spawned
/// subprocess its TCC attribution differs from Peeky's, so it can come back BLACK
/// even when in-process xcap works. (Earlier xcap appeared black only because the
/// app was ad-hoc signed and the grant wasn't honored; a real signature fixed it.)
fn grab_primary() -> Result<(DynamicImage, f64)> {
    let sf = primary_scale_factor();

    // 1. In-process xcap.
    let xcap_img = primary_monitor().ok().and_then(|m| {
        let rgba = m.capture_image().ok()?;
        if rgba.width() == 0 || rgba.height() == 0 {
            None
        } else {
            Some(DynamicImage::ImageRgba8(rgba))
        }
    });
    if xcap_img.as_ref().is_some_and(|i| !is_black(i)) {
        return Ok((xcap_img.unwrap(), sf));
    }

    // 2. xcap missing or black → try the screencapture CLI as a fallback.
    #[cfg(target_os = "macos")]
    {
        if let Ok(img) = mac_screencapture() {
            if !is_black(&img) {
                return Ok((img, sf));
            }
        }
    }

    // 3. Neither produced real pixels: return xcap's (likely black) frame so the
    // caller's is_black check surfaces a clear permission message; else error.
    match xcap_img {
        Some(img) => Ok((img, sf)),
        None => Err(anyhow!("screen capture unavailable")),
    }
}

/// Capture the main display via the macOS `screencapture` CLI (the modern,
/// permission-respecting path that works on macOS 15 where the CG APIs go black).
#[cfg(target_os = "macos")]
fn mac_screencapture() -> Result<DynamicImage> {
    let path = std::env::temp_dir().join("peeky_grab.png");
    let _ = std::fs::remove_file(&path);
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .arg("-x") // silent (no shutter sound / UI)
        .arg("-m") // main display only
        .arg("-t")
        .arg("png")
        .arg(&path)
        .status()
        .context("spawning /usr/sbin/screencapture")?;
    if !status.success() {
        return Err(anyhow!("screencapture exited with {status}"));
    }
    let img = image::open(&path).context("decoding screencapture output")?;
    let _ = std::fs::remove_file(&path);
    Ok(img)
}

/// Capture the primary monitor, DPI-normalize, downsample per `quality`, and
/// return a [`CapturedImage`] ready to send to the vision model.
///
/// See the module docs for the full pipeline. The returned `scale_x`/`scale_y`
/// map sent-image pixels back to **real screen (physical) pixels**, which is
/// what an OS-level click expects.
pub fn capture_screen(quality: Quality) -> Result<CapturedImage> {
    // Physical-pixel grab (Retina => 2× the logical size). On macOS this goes
    // through `screencapture` (works on macOS 15); elsewhere through xcap.
    let (captured, scale_factor) = grab_primary()?;
    let physical_w = captured.width();
    let physical_h = captured.height();
    if physical_w == 0 || physical_h == 0 {
        return Err(anyhow!("captured an empty image"));
    }

    // Logical size in points = physical / backingScaleFactor (PRD §2.3).
    let logical_w = ((physical_w as f64) / scale_factor).round().max(1.0) as u32;

    // We never *upscale*: the eventual sent width is the smaller of the target
    // and the logical width. (Downsampling further from physical happens in one
    // pass via `downsample` to avoid two resamples.)
    let sent_w = target_width(quality).min(logical_w);

    // Resize straight from the physical buffer to the final sent width in a
    // single high-quality pass; aspect ratio is preserved by `downsample`.
    let sent_img = downsample(&captured, sent_w);
    let (sent_w, sent_h) = sent_img.dimensions();
    if sent_w == 0 || sent_h == 0 {
        return Err(anyhow!("downsample produced an empty image"));
    }

    let png_base64 = encode_png_base64(&sent_img).context("PNG encode/base64 of captured frame")?;

    // Coordinate restore: model coords are in the sent image's space; multiply by
    // these to reach LOGICAL screen points (what a CGEvent/enigo click uses — NOT
    // physical Retina pixels, which would land 2× off). Full screen → origin 0,0.
    let scale_x = (physical_w as f64 / scale_factor) / sent_w as f64;
    let scale_y = (physical_h as f64 / scale_factor) / sent_h as f64;

    Ok(CapturedImage {
        png_base64,
        sent_w,
        sent_h,
        scale_x,
        scale_y,
        origin_x: 0.0,
        origin_y: 0.0,
    })
}

/// Capture ONLY the frontmost application's focused window (PRD intent: avoid
/// pulling overlapping apps — and Peeky's own overlay — into the frame, which
/// confuses the model). Window capture returns just that window's own content,
/// so always-on-top windows (the mascot) are never included.
///
/// Falls back to [`capture_screen`] when no suitable focused window is found
/// (e.g. the desktop is focused, or the platform can't report focus).
pub fn capture_active_window(quality: Quality) -> Result<CapturedImage> {
    // macOS 15: per-window xcap capture returns black; the working screencapture
    // path is full-display, so use that (clicks restore against a 0,0 origin).
    #[cfg(target_os = "macos")]
    {
        return capture_screen(quality);
    }

    #[cfg(not(target_os = "macos"))]
    {
        capture_active_window_xcap(quality)
    }
}

/// xcap per-window capture (non-macOS). Kept separate so the macOS path can use
/// the screencapture full-display route above.
#[cfg(not(target_os = "macos"))]
fn capture_active_window_xcap(quality: Quality) -> Result<CapturedImage> {
    let Some(win) = focused_window() else {
        return capture_screen(quality);
    };

    let rgba = win
        .capture_image()
        .map_err(|e| anyhow!("xcap: window capture_image failed: {e}"))?;
    let physical_w = rgba.width();
    let physical_h = rgba.height();
    if physical_w == 0 || physical_h == 0 {
        return capture_screen(quality);
    }
    let captured = DynamicImage::ImageRgba8(rgba);

    let scale_factor = win
        .current_monitor()
        .ok()
        .and_then(|m| m.scale_factor().ok())
        .map(|s| s as f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0);

    let logical_w = ((physical_w as f64) / scale_factor).round().max(1.0) as u32;
    let sent_w = target_width(quality).min(logical_w);
    let sent_img = downsample(&captured, sent_w);
    let (sent_w, sent_h) = sent_img.dimensions();
    if sent_w == 0 || sent_h == 0 {
        return capture_screen(quality);
    }

    let png_base64 = encode_png_base64(&sent_img).context("PNG encode of window frame")?;
    let scale_x = (physical_w as f64 / scale_factor) / sent_w as f64;
    let scale_y = (physical_h as f64 / scale_factor) / sent_h as f64;

    Ok(CapturedImage {
        png_base64,
        sent_w,
        sent_h,
        scale_x,
        scale_y,
        // Window origin in logical screen points, so click coords restore to the
        // right place on screen: screen = origin + api * scale.
        origin_x: win.x().unwrap_or(0) as f64,
        origin_y: win.y().unwrap_or(0) as f64,
    })
}

/// Frontmost "real" application window, NOT one of Peeky's own.
///
/// PERFORMANCE: every `xcap::Window` getter re-enumerates the whole window
/// server, so calling several getters on every window is O(N²) and costs
/// SECONDS. `Window::all()` is already ordered front-to-back, so we take the
/// first non-system, non-Peeky window and call at most ONE getter (`app_name`)
/// per candidate — typically 1-2 total. Returns `None` (→ full-screen fallback)
/// if nothing suitable is near the front.
///
/// The skip-list itself lives in `platform::skip_apps` so it can be shared
/// across `focused_window` / `frontmost_app_name` and across both macOS and
/// Windows (system/overlay process names are platform-specific — see
/// `platform/skip_apps.rs` for the union list).
#[cfg(not(target_os = "macos"))]
fn focused_window() -> Option<Window> {
    let windows = Window::all().ok()?;
    for w in windows.into_iter().take(12) {
        let name = w.app_name().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        if crate::platform::is_overlay_or_system_window(&name) {
            continue;
        }
        return Some(w);
    }
    None
}

/// Fast frontmost-app NAME, without shelling out to `osascript`.
///
/// `trigger::front_app_context()` forks 2-3 AppleScript processes (app name +
/// window title + maybe browser URL) and can take a full second — far too slow
/// on the interactive path that pops the region selector. This reads the name
/// straight from the front window via xcap (one enumeration), in milliseconds.
/// Returns "" when nothing suitable is found.
pub fn frontmost_app_name() -> String {
    let Ok(windows) = Window::all() else {
        return String::new();
    };
    for w in windows.into_iter().take(12) {
        let name = w.app_name().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        if crate::platform::is_overlay_or_system_window(&name) {
            continue;
        }
        return name;
    }
    String::new()
}

/// Encode an image as JPEG (quality ~82) and base64 it. Used for the region
/// selector's freeze-frame BACKGROUND only — JPEG encodes ~10× faster than PNG
/// and yields a ~10× smaller payload, which is what makes the overlay pop fast.
/// The model still receives a clean crop taken from the in-memory image, so this
/// lossy copy never touches answer quality.
pub fn encode_jpeg_base64(img: &DynamicImage) -> Result<String> {
    // JPEG has no alpha; flatten to RGB so the encoder never errors on RGBA.
    // Quality 86 keeps text crisp without an overly large payload.
    use image::ImageEncoder as _;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut buf: Vec<u8> = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 86)
        .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .context("JPEG encode")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Cheap-gate capture (PRD §2.2 L1): grab the primary monitor once and reduce
/// it straight to the 128×128 grayscale fingerprint the trigger engine hashes —
/// **no PNG encode, no base64, no decode roundtrip**.
///
/// This is the 500 ms hot path. [`capture_screen`] is ~10–50× more work per
/// call (full-width resize + PNG + base64) and is only needed once we actually
/// decide to talk to the model, so the perception loop must use *this* instead.
/// DPI normalization is intentionally skipped: [`to_gray_128`] force-squares the
/// frame, so the backing-scale factor is irrelevant to the fingerprint.
pub fn capture_gray_128() -> Result<GrayImage> {
    let monitor = primary_monitor()?;
    let rgba = monitor
        .capture_image()
        .map_err(|e| anyhow!("xcap: capture_image failed: {e}"))?;
    if rgba.width() == 0 || rgba.height() == 0 {
        return Err(anyhow!("xcap: captured an empty image"));
    }
    Ok(to_gray_128(&DynamicImage::ImageRgba8(rgba)))
}

/// Capture the whole primary monitor and return a logical-normalized image plus
/// the monitor's LOGICAL size in points. Used by the region selector's
/// freeze-frame overlay (PRD-adjacent quick shortcuts): the frozen screenshot is
/// shown fullscreen so the user can drag a precise region with a magnifier loupe.
///
/// Returns the RAW physical-pixel image (no resize) plus the monitor's LOGICAL
/// size in points.
///
/// PERF: we deliberately DO NOT downsample the whole frame here. A full-frame
/// resize of a ~6-megapixel Retina screenshot with a quality filter costs
/// *seconds* in a debug build (and is pure waste — the user only sends a small
/// cropped region to the model). Cropping is cheap; the display background is
/// downscaled separately and cheaply by [`encode_display_jpeg`]. So this stays
/// O(capture) and the selector pops fast.
pub fn capture_full() -> Result<(DynamicImage, u32, u32)> {
    let (captured, scale_factor) = grab_primary()?;
    let physical_w = captured.width();
    let physical_h = captured.height();
    if physical_w == 0 || physical_h == 0 {
        return Err(anyhow!("captured an empty image"));
    }
    let logical_w = ((physical_w as f64) / scale_factor).round().max(1.0) as u32;
    let logical_h = ((physical_h as f64) / scale_factor).round().max(1.0) as u32;
    Ok((captured, logical_w, logical_h))
}

/// True if `img` is essentially an all-black frame (no real content). On macOS,
/// `xcap` can return a fully black image even when the Screen Recording toggle
/// is ON — the grant often needs an app RESTART to take effect, and in `tauri
/// dev` it applies to a bare rebuilt binary, which is flaky. A black frame is
/// indistinguishable from a real one to the model, so we detect it explicitly.
pub fn is_black(img: &DynamicImage) -> bool {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return true;
    }
    // Sparse grid sample (~64×64) — cheap, and a real screen has bright pixels.
    let step_x = (w / 64).max(1);
    let step_y = (h / 64).max(1);
    let (mut max, mut sum, mut n) = (0u8, 0u64, 0u64);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let p = img.get_pixel(x, y);
            let v = ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8;
            if v > max {
                max = v;
            }
            sum += v as u64;
            n += 1;
            x += step_x;
        }
        y += step_y;
    }
    let mean = if n > 0 { sum / n } else { 0 };
    // Near-zero brightness with no bright pixel anywhere → not a real capture.
    mean < 4 && max < 16
}

/// Whether a fresh capture actually yields real pixels (not a black frame). Used
/// by the settings "Screen Recording" status so it reflects what WORKS, not just
/// what the TCC toggle claims.
pub fn capture_is_black() -> bool {
    match capture_gray_128() {
        Ok(g) => {
            let (mut max, mut sum, mut n) = (0u8, 0u64, 0u64);
            for p in g.pixels() {
                let v = p[0];
                if v > max {
                    max = v;
                }
                sum += v as u64;
                n += 1;
            }
            let mean = if n > 0 { sum / n.max(1) } else { 0 };
            mean < 4 && max < 16
        }
        Err(_) => true,
    }
}

/// Cheap downscale + JPEG for the selector BACKGROUND only. Uses a NEAREST
/// resize (fast even in debug — no per-pixel filtering) and JPEG (fast encode,
/// small payload). The overlay shows this stretched to fill, so it maps clicks
/// via the FULL image dimensions reported alongside it — the lossy/low-res
/// display copy never touches what's sent to the model (that's cropped from the
/// full image). Returns base64 (no data: prefix).
pub fn encode_display_jpeg(img: &DynamicImage, max_w: u32) -> Result<String> {
    let (w, h) = img.dimensions();
    let display = if w > max_w && max_w > 0 {
        let target_h = ((h as u64 * max_w as u64) / w as u64).max(1) as u32;
        img.resize_exact(max_w, target_h, image::imageops::FilterType::Triangle)
    } else {
        img.clone()
    };
    encode_jpeg_base64(&display)
}

/// Crop a region (in `img` pixel coordinates) out of an already-captured image,
/// downsample it to the model target width, and package it as a [`CapturedImage`]
/// ready to send. Used after the user drags a selection on the freeze-frame
/// overlay. Coordinates are clamped to the image bounds.
pub fn crop_to_captured(
    img: &DynamicImage,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    quality: Quality,
) -> Result<CapturedImage> {
    let (iw, ih) = img.dimensions();
    if iw == 0 || ih == 0 {
        return Err(anyhow!("source image is empty"));
    }
    let x0 = x.max(0.0).min((iw - 1) as f64);
    let y0 = y.max(0.0).min((ih - 1) as f64);
    let cw = w.max(1.0).min(iw as f64 - x0);
    let ch = h.max(1.0).min(ih as f64 - y0);

    let crop = img.crop_imm(x0 as u32, y0 as u32, cw.max(1.0) as u32, ch.max(1.0) as u32);
    let sent_w = target_width(quality).min(crop.width().max(1));
    let sent = downsample(&crop, sent_w);
    let (sw, sh) = sent.dimensions();
    if sw == 0 || sh == 0 {
        return Err(anyhow!("crop produced an empty image"));
    }
    // JPEG (not PNG): ~5-10× smaller, which means a faster encode AND a smaller
    // vision payload for the model to ingest. `png_base64` carries the JPEG
    // bytes; the quick-shortcut message builder labels it image/jpeg.
    let png_base64 = encode_jpeg_base64(&sent).context("JPEG encode of cropped region")?;
    // These map back into the FROZEN image's pixel space (not live screen). The
    // quick shortcuts don't click, so that's sufficient; origin is the crop TL.
    let scale_x = cw / sw as f64;
    let scale_y = ch / sh as f64;
    Ok(CapturedImage {
        png_base64,
        sent_w: sw,
        sent_h: sh,
        scale_x,
        scale_y,
        origin_x: x0,
        origin_y: y0,
    })
}

/// Downsample `img` to `target_w` width, preserving aspect ratio.
///
/// If the image is already at or below `target_w` it is returned unchanged
/// (we never upscale — see PRD §2.3, the API should receive the smaller image).
/// Uses a triangle (bilinear) filter: a good quality/speed trade-off for UI
/// screenshots where text legibility matters but we are time-budgeted.
pub fn downsample(img: &DynamicImage, target_w: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || target_w == 0 || w <= target_w {
        return img.clone();
    }
    let target_h = ((h as u64 * target_w as u64) / w as u64).max(1) as u32;
    img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle)
}

/// Produce the fixed 128×128 grayscale fingerprint the trigger engine hashes
/// (PRD §2.2 L1). The square resize is intentional: pHash / cross-correlation
/// operate on a normalized canvas, so distorting aspect ratio here is fine and
/// keeps the comparison cheap and dimension-independent.
pub fn to_gray_128(img: &DynamicImage) -> GrayImage {
    img.resize_exact(GRAY_SIZE, GRAY_SIZE, image::imageops::FilterType::Triangle)
        .to_luma8()
}

/// Encode a [`DynamicImage`] as PNG and base64 it (no `data:` URI prefix —
/// callers wrap it for the OpenAI vision payload).
fn encode_png_base64(img: &DynamicImage) -> Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .context("write PNG")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgba([10, 20, 30, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn downsample_preserves_aspect_and_never_upscales() {
        let img = solid(3200, 1800); // 16:9 physical-ish
        let out = downsample(&img, 1280);
        assert_eq!(out.width(), 1280);
        assert_eq!(out.height(), 720);

        // Already small enough -> unchanged.
        let small = solid(800, 600);
        let out2 = downsample(&small, 1280);
        assert_eq!(out2.dimensions(), (800, 600));
    }

    #[test]
    fn gray_128_is_fixed_square() {
        let g = to_gray_128(&solid(2880, 1620));
        assert_eq!(g.dimensions(), (GRAY_SIZE, GRAY_SIZE));
    }

    #[test]
    fn target_width_matches_prd() {
        assert_eq!(target_width(Quality::Low), 960);
        assert_eq!(target_width(Quality::Med), 1280);
        assert_eq!(target_width(Quality::High), 1600);
    }

    #[test]
    fn png_base64_roundtrips() {
        let b64 = encode_png_base64(&solid(8, 8)).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.dimensions(), (8, 8));
    }
}
