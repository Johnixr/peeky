//! Cheap-first trigger engine (PRD §2.2 L0 + L1).
//!
//! Design rule from the PRD: **use the cheapest signal that can decide.** Never
//! call a model here, never run OCR. Everything in this module is pure integer /
//! float math over a tiny 128×128 grayscale frame, plus a zero-cost probe for
//! the foreground app context (delegated to the per-OS `platform::trigger`).
//!
//! The pipeline this module implements:
//!   1. Perceptual hash (DCT-based pHash) of the whole 128px frame.
//!   2. Hamming distance vs. the previous frame's hash.
//!   3. If something changed, check whether it is a *pure vertical scroll* by
//!      cross-correlating a central vertical strip against the previous frame.
//!      A scroll updates the reading-progress accumulator but does **not**
//!      produce a new utterance (PRD §2.2 L1).
//!   4. Otherwise, decide NoChange vs Meaningful by comparing the hamming
//!      distance against a sensitivity-derived threshold.
//!
//! `front_app_context()` is the L0 event-driven gate: it reads the frontmost
//! app + focused window title (and a best-effort browser tab URL on macOS).
//! Backed by `platform::trigger::{macos,windows}` — see `src-tauri/src/platform/`.

use image::GrayImage;

use crate::types::TriggerDecision;

// Re-export the per-OS front-app context helpers so existing callers can keep
// using `trigger::front_app_context()` / `trigger::frontmost_app_name()`.
pub use crate::platform::trigger::{front_app_context, frontmost_app_name};

/// Side length of the working frame. The main loop hands us a 128×128 grayscale
/// image (see `capture::to_gray_128`); we hard-code the assumption but defend
/// against off-size inputs by reading the actual dimensions where it matters.
#[allow(dead_code)]
const FRAME: u32 = 128;

/// Length (in bits) of the perceptual hash. We use a 32×32 DCT and keep the
/// top-left 8×8 low-frequency block → 64 bits, the classic pHash size.
const PHASH_BITS: u32 = 64;

/// DCT input block size. We downsample the 128px frame to 32×32 before the DCT
/// so the transform stays cheap and the low-frequency block is meaningful.
const DCT_N: usize = 32;
/// Side of the low-frequency block kept from the DCT (8×8 = 64 bits).
const DCT_KEEP: usize = 8;

/// How far (in rows) we search when looking for a vertical scroll shift. A
/// typical scroll moves a noticeable fraction of the frame; ±48 rows on a 128px
/// frame covers small nudges through large flings while staying cheap.
const MAX_SHIFT: i32 = 48;

/// Minimum absolute shift (rows) to call something a scroll. Shifts of 0/±1 are
/// noise/jitter and should fall through to the meaningful-change branch.
const MIN_SCROLL_SHIFT: i32 = 2;

/// The central vertical strip width (columns) used for cross-correlation. A
/// narrow strip down the middle of the frame avoids window chrome on the edges.
const STRIP_W: u32 = 24;

/// How much better the best vertical shift must explain the change than the
/// zero-shift baseline to be accepted as a scroll. Expressed as the maximum
/// ratio of (best mean-abs-diff) / (zero-shift mean-abs-diff). Lower = stricter.
const SCROLL_RATIO: f64 = 0.55;

/// The cheap pixel-change + scroll detector. Holds just enough state to compare
/// the current frame against the previous one. Constructed once and reused
/// across the whole session.
pub struct TriggerEngine {
    /// pHash of the previous frame (64 bits). `None` until the first frame.
    last_phash: Option<u64>,
    /// The previous 128px grayscale frame, kept for vertical cross-correlation.
    last_frame: Option<GrayImage>,
    /// Accumulated vertical scroll, in frame rows, since the last clear. Positive
    /// = content moved up (user scrolled down / forward through a document).
    /// This is the engine's lightweight "reading progress" signal (PRD §2.2 /
    /// §4.3); the copilot reading-assist feature can read it via `reading_progress`.
    reading_progress: i64,
    /// Hamming-distance threshold below which a (non-scroll) change is ignored.
    /// Settable from sensitivity; defaults to the "medium" value.
    threshold: u32,
}

impl Default for TriggerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TriggerEngine {
    /// Create a fresh engine with no history and the medium sensitivity threshold.
    pub fn new() -> Self {
        TriggerEngine {
            last_phash: None,
            last_frame: None,
            reading_progress: 0,
            threshold: sensitivity_threshold(crate::types::Quality::Med),
        }
    }

    /// Override the hamming threshold from the configured change-detection
    /// sensitivity (PRD §8.2). The main loop may call this when config changes;
    /// it is harmless to call every tick. Lower threshold = more sensitive.
    pub fn set_sensitivity(&mut self, sensitivity: crate::types::Quality) {
        self.threshold = sensitivity_threshold(sensitivity);
    }

    /// Evaluate the latest 128px grayscale frame against the previous one.
    ///
    /// Returns:
    /// - `NoChange`    — hamming distance below threshold (nothing worth acting on).
    /// - `Scroll`      — the change is explained by a pure vertical translation;
    ///                   reading progress is updated, no utterance should fire.
    /// - `Meaningful`  — a non-scroll content change worth a model call.
    ///
    /// This is pure CPU math: a 32×32 DCT for the hash plus a 1-D strip
    /// cross-correlation. No allocations beyond the small working buffers, no
    /// model, no OCR.
    pub fn evaluate(&mut self, gray128: &GrayImage) -> TriggerDecision {
        let phash = dct_phash(gray128);

        // First frame ever: nothing to compare against. Seed state, stay quiet.
        let (prev_hash, prev_frame) = match (self.last_phash, self.last_frame.as_ref()) {
            (Some(h), Some(f)) => (h, f),
            _ => {
                self.last_phash = Some(phash);
                self.last_frame = Some(gray128.clone());
                return TriggerDecision::NoChange;
            }
        };

        let dist = hamming(prev_hash, phash);

        // Below threshold → effectively the same screen. Refresh the stored frame
        // anyway so slow drift doesn't accumulate, but report no change.
        if dist < self.threshold {
            self.last_phash = Some(phash);
            self.last_frame = Some(gray128.clone());
            return TriggerDecision::NoChange;
        }

        // Something changed. Is it a pure vertical scroll? Run the strip
        // cross-correlation against the previous frame.
        let decision = match detect_vertical_scroll(prev_frame, gray128) {
            Some(shift) if shift.abs() >= MIN_SCROLL_SHIFT => {
                // Scroll: advance reading progress, do not speak (PRD §2.2 L1).
                self.reading_progress += shift as i64;
                TriggerDecision::Scroll
            }
            _ => TriggerDecision::Meaningful,
        };

        // Commit the new frame as the baseline for the next tick.
        self.last_phash = Some(phash);
        self.last_frame = Some(gray128.clone());
        decision
    }

    /// The engine's accumulated vertical reading progress, in frame rows. Used by
    /// the copilot reading-assist path (PRD §5.3); harmless to ignore elsewhere.
    pub fn reading_progress(&self) -> i64 {
        self.reading_progress
    }

    /// Reset reading progress (e.g. when the foreground document changes).
    pub fn reset_reading_progress(&mut self) {
        self.reading_progress = 0;
    }
}

/// Map the user's sensitivity knob to a hamming-distance threshold over the
/// 64-bit pHash. "High" sensitivity → small threshold (reacts to little
/// changes); "Low" → large threshold (only big changes trip it).
fn sensitivity_threshold(q: crate::types::Quality) -> u32 {
    match q {
        crate::types::Quality::High => 6,
        crate::types::Quality::Med => 10,
        crate::types::Quality::Low => 16,
    }
}

/// Hamming distance between two 64-bit hashes (number of differing bits).
#[inline]
fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ---- Perceptual hash (DCT-based pHash) --------------------------------------

/// Compute a 64-bit DCT-based perceptual hash of a grayscale frame.
///
/// Steps (classic pHash):
///   1. Resize/sample the frame down to DCT_N×DCT_N (32×32) luminance values.
///   2. Apply a separable 2-D DCT-II.
///   3. Keep the top-left DCT_KEEP×DCT_KEEP (8×8) low-frequency coefficients,
///      excluding the DC term (0,0) from the median.
///   4. Set each of the 64 bits to 1 if its coefficient is above the median.
fn dct_phash(img: &GrayImage) -> u64 {
    // Pull a DCT_N×DCT_N luminance grid out of the frame via nearest sampling.
    let grid = sample_grid(img, DCT_N);

    // Precompute the 1-D DCT-II basis: cos[k][n] for k,n in 0..DCT_N.
    let basis = dct_basis();

    // Separable 2-D DCT: rows then columns. We only need the top-left
    // DCT_KEEP×DCT_KEEP coefficients, but computing the full transform on a
    // 32×32 grid is trivially cheap and keeps the code simple/clear.
    let mut rows = [[0.0f64; DCT_N]; DCT_N];
    for (y, row) in grid.iter().enumerate() {
        for k in 0..DCT_N {
            let mut sum = 0.0;
            let b = &basis[k];
            for n in 0..DCT_N {
                sum += row[n] * b[n];
            }
            rows[y][k] = sum;
        }
    }
    let mut coeffs = [[0.0f64; DCT_N]; DCT_N];
    for k in 0..DCT_N {
        let b = &basis[k];
        for x in 0..DCT_N {
            let mut sum = 0.0;
            for y in 0..DCT_N {
                sum += rows[y][x] * b[y];
            }
            coeffs[k][x] = sum;
        }
    }

    // Collect the low-frequency block (excluding the DC term for the median).
    let mut low = [0.0f64; DCT_KEEP * DCT_KEEP];
    let mut idx = 0;
    for v in 0..DCT_KEEP {
        for u in 0..DCT_KEEP {
            low[idx] = coeffs[v][u];
            idx += 1;
        }
    }

    // Median over the 64 values excluding the DC component at index 0.
    let mut sorted: Vec<f64> = low[1..].to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    // Build the 64-bit hash: bit set when the coefficient exceeds the median.
    let mut hash: u64 = 0;
    for (i, &c) in low.iter().enumerate().take(PHASH_BITS as usize) {
        if c > median {
            hash |= 1u64 << i;
        }
    }
    hash
}

/// Nearest-neighbour sample the frame into an `n`×`n` grid of f64 luminance.
fn sample_grid(img: &GrayImage, n: usize) -> Vec<[f64; DCT_N]> {
    let (w, h) = img.dimensions();
    let w = w.max(1);
    let h = h.max(1);
    let mut grid = vec![[0.0f64; DCT_N]; n];
    for gy in 0..n {
        // Map grid row -> source row (center-of-cell sampling).
        let sy = (((gy as u32) * h) / n as u32).min(h - 1);
        for gx in 0..n {
            let sx = (((gx as u32) * w) / n as u32).min(w - 1);
            grid[gy][gx] = img.get_pixel(sx, sy)[0] as f64;
        }
    }
    grid
}

/// Precompute the DCT-II cosine basis for a DCT_N-length transform.
/// `basis[k][n] = cos(PI/N * (n + 0.5) * k)`. The orthonormal scale factor is
/// irrelevant here: we only threshold against the median, so any positive,
/// k-independent scaling cancels out.
fn dct_basis() -> [[f64; DCT_N]; DCT_N] {
    let mut basis = [[0.0f64; DCT_N]; DCT_N];
    let n = DCT_N as f64;
    for k in 0..DCT_N {
        for x in 0..DCT_N {
            basis[k][x] =
                ((std::f64::consts::PI / n) * (x as f64 + 0.5) * k as f64).cos();
        }
    }
    basis
}

// ---- Vertical scroll detection (1-D strip cross-correlation) ----------------

/// Detect a pure vertical scroll between two frames by sliding a central
/// vertical strip of `cur` against `prev` and finding the row shift that
/// minimizes the mean absolute difference.
///
/// Returns `Some(shift)` when a vertical translation explains the change much
/// better than no shift at all (ratio test), where a positive shift means the
/// content moved *up* between frames (the user scrolled forward/down). Returns
/// `None` when no clear translation dominates (i.e. it's a real content change).
///
/// This is intentionally O(MAX_SHIFT × strip_area): on a 128px frame with a
/// 24px strip and ±48 search range that's well under a million integer ops —
/// microseconds, no model, no OCR (PRD §2.2 L1).
fn detect_vertical_scroll(prev: &GrayImage, cur: &GrayImage) -> Option<i32> {
    let (pw, ph) = prev.dimensions();
    let (cw, ch) = cur.dimensions();
    // Require matching, sane dimensions; otherwise we can't correlate cheaply.
    if pw != cw || ph != ch || pw == 0 || ph == 0 {
        return None;
    }
    let w = pw;
    let h = ph as i32;

    // Central strip column range.
    let strip_w = STRIP_W.min(w);
    let x0 = (w.saturating_sub(strip_w)) / 2;
    let x1 = x0 + strip_w;

    // Baseline: mean abs diff at zero shift over the overlapping strip.
    let baseline = strip_diff(prev, cur, x0, x1, 0, h)?;
    if baseline <= f64::EPSILON {
        // Identical strips → not a scroll (and not meaningful either, but the
        // caller already cleared the hamming threshold). Treat as no scroll.
        return None;
    }

    // Search the shift that minimizes mean abs diff. Skip shift==0 (that's the
    // baseline) — we only want to know if *moving* the strip explains it better.
    let max_shift = MAX_SHIFT.min(h - 1).max(0);
    let mut best_shift = 0i32;
    let mut best_diff = f64::INFINITY;
    for shift in -max_shift..=max_shift {
        if shift == 0 {
            continue;
        }
        if let Some(d) = strip_diff(prev, cur, x0, x1, shift, h) {
            if d < best_diff {
                best_diff = d;
                best_shift = shift;
            }
        }
    }

    if best_diff.is_finite() && best_diff <= baseline * SCROLL_RATIO {
        Some(best_shift)
    } else {
        None
    }
}

/// Mean absolute difference between `prev` and `cur` over the central strip,
/// with `cur` shifted vertically by `shift` rows relative to `prev`.
///
/// A positive `shift` aligns `cur` row `y` with `prev` row `y + shift`, i.e. it
/// tests the hypothesis "content in `cur` moved up by `shift` rows versus
/// `prev`". Only the overlapping rows contribute; if too few rows overlap we
/// return `None` so a degenerate alignment can't win the search.
fn strip_diff(
    prev: &GrayImage,
    cur: &GrayImage,
    x0: u32,
    x1: u32,
    shift: i32,
    h: i32,
) -> Option<f64> {
    // Determine the overlapping row range in `cur`'s coordinate space.
    let y_start = if shift < 0 { -shift } else { 0 };
    let y_end = if shift > 0 { h - shift } else { h };
    if y_end - y_start < h / 3 {
        // Less than a third of the strip overlaps — not enough evidence.
        return None;
    }

    let mut acc: u64 = 0;
    let mut count: u64 = 0;
    let mut y = y_start;
    while y < y_end {
        let py = (y + shift) as u32;
        let cy = y as u32;
        let mut x = x0;
        while x < x1 {
            let pv = prev.get_pixel(x, py)[0] as i32;
            let cv = cur.get_pixel(x, cy)[0] as i32;
            acc += (pv - cv).unsigned_abs() as u64;
            count += 1;
            x += 1;
        }
        y += 1;
    }
    if count == 0 {
        None
    } else {
        Some(acc as f64 / count as f64)
    }
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    /// Build a 128×128 frame whose brightness ramps vertically, offset by
    /// `offset` rows. Scrolling the content is modeled as changing `offset`.
    fn ramp_frame(offset: i32) -> GrayImage {
        let mut img = GrayImage::new(FRAME, FRAME);
        for y in 0..FRAME as i32 {
            // Repeating horizontal bands so a vertical shift is well-defined.
            let v = (((y + offset).rem_euclid(64)) * 4) as u8;
            for x in 0..FRAME {
                img.put_pixel(x, y as u32, Luma([v]));
            }
        }
        img
    }

    /// A frame full of pseudo-random noise — no vertical structure to track.
    fn noise_frame(seed: u32) -> GrayImage {
        let mut img = GrayImage::new(FRAME, FRAME);
        let mut s = seed.wrapping_add(1);
        for y in 0..FRAME {
            for x in 0..FRAME {
                // xorshift-ish cheap PRNG.
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                img.put_pixel(x, y, Luma([(s & 0xff) as u8]));
            }
        }
        img
    }

    #[test]
    fn first_frame_is_no_change() {
        let mut eng = TriggerEngine::new();
        assert_eq!(eng.evaluate(&ramp_frame(0)), TriggerDecision::NoChange);
    }

    #[test]
    fn identical_frame_is_no_change() {
        let mut eng = TriggerEngine::new();
        let f = ramp_frame(0);
        eng.evaluate(&f);
        assert_eq!(eng.evaluate(&f), TriggerDecision::NoChange);
    }

    #[test]
    fn vertical_shift_is_scroll_and_updates_progress() {
        let mut eng = TriggerEngine::new();
        eng.evaluate(&ramp_frame(0));
        // Shift the banded content down by 10 rows: a clean vertical scroll.
        let d = eng.evaluate(&ramp_frame(10));
        assert_eq!(d, TriggerDecision::Scroll);
        assert_ne!(eng.reading_progress(), 0, "scroll must move reading progress");
    }

    #[test]
    fn unstructured_change_is_meaningful() {
        let mut eng = TriggerEngine::new();
        eng.evaluate(&noise_frame(1));
        // A totally different noise field has no vertical translation → meaningful.
        assert_eq!(eng.evaluate(&noise_frame(999)), TriggerDecision::Meaningful);
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0b1011, 0b0001), 2);
        assert_eq!(hamming(0, u64::MAX), 64);
    }

    #[test]
    fn phash_stable_for_same_image() {
        let f = ramp_frame(3);
        assert_eq!(dct_phash(&f), dct_phash(&f));
    }

    #[test]
    fn sensitivity_orders_thresholds() {
        use crate::types::Quality::*;
        assert!(sensitivity_threshold(High) < sensitivity_threshold(Med));
        assert!(sensitivity_threshold(Med) < sensitivity_threshold(Low));
    }

    #[test]
    fn reset_reading_progress_clears() {
        let mut eng = TriggerEngine::new();
        eng.evaluate(&ramp_frame(0));
        eng.evaluate(&ramp_frame(20));
        eng.reset_reading_progress();
        assert_eq!(eng.reading_progress(), 0);
    }
}
