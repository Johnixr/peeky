//! Shared application state, managed by Tauri and accessed from the main loop,
//! the global-shortcut handlers, and the `#[tauri::command]` functions.
//!
//! Everything mutable lives behind a `parking_lot::Mutex` (cheap, no poisoning)
//! except the paused flag, which is a lock-free `AtomicBool` so the hot 500ms
//! loop can check it without contending on a lock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::memory::RollingMemory;
use crate::restraint::RestraintEngine;
use crate::trigger::TriggerEngine;
use crate::types::{CapturedImage, Config, TokenStats};

/// A screenshot captured the instant the user pressed the "ask about my screen"
/// shortcut, parked here until they submit (or cancel) their typed question. We
/// capture at press time — before Peeky's input box steals focus — so the image
/// (and `app` context) reflect the REAL foreground app, not Peeky itself.
pub struct PendingShot {
    pub captured: CapturedImage,
    /// Foreground app name at capture time (for context + history).
    pub app: String,
    /// Unix epoch seconds at capture time.
    pub ts: i64,
}

/// A frozen full-screen frame parked while the user drags a region on the
/// freeze-frame selector overlay (quick Explain / Ask shortcuts). The cropped
/// region is computed from `img` once the user releases the mouse.
pub struct RegionShot {
    /// Which one-shot the selection feeds once cropped.
    pub kind: crate::modes::QuickKind,
    /// The frozen, logical-normalized full-screen image (cropped on submit).
    pub img: image::DynamicImage,
    /// Foreground app name at freeze time (for context + history).
    pub app: String,
}

/// The single application state object. Constructed once in `lib.rs::run()` and
/// handed to Tauri via `.manage(...)`. Commands retrieve it with
/// `tauri::State<'_, AppState>`.
pub struct AppState {
    /// The live user configuration. Cloned out when needed by the loop/commands.
    pub config: Mutex<Config>,

    /// Cumulative token usage (PRD §1.4 / §8.2).
    pub stats: Mutex<TokenStats>,

    /// Global pause flag (double-click / Ctrl+Shift+P / quiet hours). The main
    /// loop skips capture entirely while paused (PRD §7).
    pub paused: AtomicBool,

    /// Cheap pixel-change + scroll detector (PRD §2.2 L1).
    pub trigger: Mutex<TriggerEngine>,

    /// Speech-budget / quiet-hours / DND / ignore-decay gate (PRD §4).
    pub restraint: Mutex<RestraintEngine>,

    /// Rolling in-memory context for "don't repeat" + reading progress (PRD §4.3).
    pub memory: Mutex<RollingMemory>,

    /// Wall-clock millis of the last detected meaningful change, used by the
    /// 800ms debounce stability window (PRD §2.2 L2). 0 = none yet.
    pub last_change_ms: AtomicU64,

    /// Guards against overlapping model calls: the loop sets this while a
    /// streaming request is in flight so it doesn't fire a second one.
    pub speaking: AtomicBool,

    /// Screenshot parked by the "ask about my screen" shortcut, awaiting the
    /// user's typed question (PRD-adjacent quick shortcut). `None` when idle.
    pub pending_shot: Mutex<Option<PendingShot>>,

    /// Frozen full-screen frame parked while the region selector overlay is up,
    /// awaiting the user's drag-selected rectangle. `None` when idle.
    pub pending_region: Mutex<Option<RegionShot>>,

    /// True while interactive overlay UI (bubble / ask box / hover toolbar) is
    /// open: the click-through poller then keeps the WHOLE window catching mouse
    /// events so the user can scroll/click/type. When false, only the sprite area
    /// catches clicks and the rest passes through to apps underneath.
    pub overlay_interactive: AtomicBool,

    /// Wall-clock millis of the last break reminder image shown, used by the
    /// reminder loop to enforce the configured interval. 0 = none yet.
    pub last_reminder_ms: AtomicU64,

    /// Guards against overlapping reminder image generations (similar to `speaking`
    /// but for the reminder pipeline) so we don't stack API calls if generation is slow.
    pub reminder_generating: AtomicBool,

    /// The latest generated reminder image as a `data:` URL, parked here for the
    /// reminder window to PULL via `get_reminder_image` once it's shown (mirrors
    /// the proven region-shot pull model, avoiding the event-before-listener race).
    pub pending_reminder: Mutex<Option<String>>,
}

impl AppState {
    /// Build state from an initial (already-loaded) config and stats.
    pub fn new(config: Config, stats: TokenStats) -> Self {
        AppState {
            config: Mutex::new(config),
            stats: Mutex::new(stats),
            paused: AtomicBool::new(false),
            trigger: Mutex::new(TriggerEngine::new()),
            restraint: Mutex::new(RestraintEngine::new()),
            memory: Mutex::new(RollingMemory::new()),
            last_change_ms: AtomicU64::new(0),
            speaking: AtomicBool::new(false),
            pending_shot: Mutex::new(None),
            pending_region: Mutex::new(None),
            overlay_interactive: AtomicBool::new(false),
            last_reminder_ms: AtomicU64::new(0),
            reminder_generating: AtomicBool::new(false),
            pending_reminder: Mutex::new(None),
        }
    }

    /// Cheap snapshot of the current config for read-only use in the loop.
    pub fn config_snapshot(&self) -> Config {
        self.config.lock().clone()
    }

    /// True while the loop should not capture/speak.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Flip the pause flag and return the new value (PRD §8.1 toggle).
    pub fn toggle_paused(&self) -> bool {
        // fetch_xor returns the previous value; the new value is its negation.
        !self.paused.fetch_xor(true, Ordering::Relaxed)
    }

    /// True while a model call is in flight.
    pub fn is_speaking(&self) -> bool {
        self.speaking.load(Ordering::Relaxed)
    }

    /// Try to claim the speaking slot. Returns true if claimed (was free).
    pub fn try_begin_speaking(&self) -> bool {
        self.speaking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the speaking slot.
    pub fn end_speaking(&self) {
        self.speaking.store(false, Ordering::Release);
    }

    /// Try to claim the reminder generation slot. Returns true if claimed.
    pub fn try_begin_reminder(&self) -> bool {
        self.reminder_generating
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the reminder generation slot.
    pub fn end_reminder(&self) {
        self.reminder_generating.store(false, Ordering::Release);
    }
}
