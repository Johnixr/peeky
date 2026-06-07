//! Tauri command surface (PRD §8). Every `#[tauri::command]` here is registered
//! in `lib.rs`'s `invoke_handler`. Commands are the frontend's only way to read/
//! mutate backend state, so they operate on `tauri::State<AppState>` and (where
//! they emit events or read the OS) the `AppHandle`.
//!
//! Conventions:
//! - Reads return owned, serializable copies (the frontend gets plain JSON).
//! - `set_config` persists to disk AND emits `peeky://config-changed` so every
//!   listener (mascot, settings panel) stays in sync.
//! - Anything that talks to the model is `async` and uses the live `Config`.

use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::settings;
use crate::state::AppState;
use crate::types::{mascot_state, Config, HistoryEntry, ModeKind, PermissionMode, TokenStats};
use crate::{api, run_trigger_now};

/// Event name kept in sync with `lib.rs` (the frontend listens for this).
const EV_CONFIG_CHANGED: &str = "peeky://config-changed";
const EV_STATE: &str = "peeky://state";

/// Return the live user configuration (PRD §8.2). The key may have been
/// overlaid from `PEEKY_API_KEY` at load time (settings.rs).
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Config {
    state.config_snapshot()
}

/// Replace the configuration, persist it, and broadcast the change.
/// Persisting + emitting keeps the on-disk file, in-memory state, and every
/// frontend listener consistent (PRD §8.2).
#[tauri::command]
pub fn set_config<R: Runtime>(app: AppHandle<R>, config: Config) {
    {
        let state = app.state::<AppState>();
        let mut guard = state.config.lock();
        *guard = config.clone();
    }
    settings::save_config(&config);
    let _ = app.emit(EV_CONFIG_CHANGED, &config);
}

/// Resolve the system locale to one of the three supported short codes
/// ("zh" | "ja" | "en"), defaulting to "en" (PRD i18n contract). Used by the
/// frontend i18n layer when `Config.language == Auto`.
#[tauri::command]
pub fn get_system_locale() -> String {
    let locale = sys_locale::get_locale().unwrap_or_else(|| "en".to_string());
    let lc = locale.to_ascii_lowercase();
    if lc.starts_with("zh") {
        "zh".to_string()
    } else if lc.starts_with("ja") {
        "ja".to_string()
    } else {
        "en".to_string()
    }
}

/// Manual trigger (PRD §4 主动优先 / §8.1 Ctrl+Shift+Space): capture + call the
/// model immediately, bypassing the restraint engine. Delegates to the shared
/// pipeline in `lib.rs`. Errors are surfaced to the caller as a string.
#[tauri::command]
pub async fn trigger_now<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    run_trigger_now(app).await.map_err(|e| format!("{e:#}"))
}

/// Toggle the global pause flag and reflect it in the mascot state
/// (PRD §6.2/§7). Returns the new paused value.
#[tauri::command]
pub fn pause_toggle<R: Runtime>(app: AppHandle<R>) -> bool {
    let state = app.state::<AppState>();
    let now_paused = state.toggle_paused();
    let _ = app.emit(
        EV_STATE,
        serde_json::json!({
            "state": if now_paused { mascot_state::PAUSED } else { mascot_state::IDLE }
        }),
    );
    now_paused
}

/// Set the active personality mode (PRD §3.1). Persists + broadcasts.
#[tauri::command]
pub fn set_mode<R: Runtime>(app: AppHandle<R>, mode: ModeKind) {
    let new_cfg = {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock();
        cfg.mode = mode;
        cfg.clone()
    };
    settings::save_config(&new_cfg);
    let _ = app.emit(EV_CONFIG_CHANGED, &new_cfg);
}

/// Set the permission / autonomy mode (PRD §3.2). Persists + broadcasts.
#[tauri::command]
pub fn set_permission_mode<R: Runtime>(app: AppHandle<R>, mode: PermissionMode) {
    let new_cfg = {
        let state = app.state::<AppState>();
        let mut cfg = state.config.lock();
        cfg.permission_mode = mode;
        cfg.clone()
    };
    settings::save_config(&new_cfg);
    let _ = app.emit(EV_CONFIG_CHANGED, &new_cfg);
}

/// Return cumulative token-usage stats for the settings panel (PRD §1.4/§8.2).
#[tauri::command]
pub fn get_token_stats(state: State<'_, AppState>) -> TokenStats {
    state.stats.lock().clone()
}

/// Test the configured API endpoint with a tiny non-streaming round-trip
/// (PRD §8.2 "Test connection" button). On success returns the configured model
/// name; on failure returns a human-readable error string.
///
/// Delegates to `api::test_connection` (the dedicated lightweight probe) so we
/// exercise the real TLS/auth path without spending a full streaming call.
#[tauri::command]
pub async fn test_api_connection(state: State<'_, AppState>) -> Result<String, String> {
    let cfg = state.config_snapshot();

    if cfg.api_base_url.trim().is_empty() {
        return Err("Base URL is empty — set it in settings (PRD §1.5).".to_string());
    }
    if cfg.api_key.trim().is_empty() {
        return Err(
            "API key is empty — set it in settings or via the PEEKY_API_KEY environment variable."
                .to_string(),
        );
    }

    match api::test_connection(&cfg).await {
        Ok(_reply) => Ok(cfg.model.clone()),
        Err(e) => Err(format!("connection test failed: {e:#}")),
    }
}

/// Show + focus the dedicated, opaque settings window (PRD §8.2). It is declared
/// in `tauri.conf.json` as a hidden second window labeled "settings"; we just
/// reveal it on demand (from the gear button, the `Ctrl+Shift+S` shortcut, or
/// `open_settings`). The mascot's own transparent overlay window is untouched,
/// so the sprite stays on screen while settings are open.
pub fn show_settings_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        // Tell the page to reload the live config (the user may have changed it
        // elsewhere since the window was last shown).
        let _ = win.emit("peeky://settings-shown", ());
    }
}

/// Command wrapper so the webview (mascot gear button) can open settings.
#[tauri::command]
pub fn open_settings<R: Runtime>(app: AppHandle<R>) {
    show_settings_window(&app);
}

/// Return the persisted utterance history, newest first (PRD review feature).
#[tauri::command]
pub fn get_history() -> Vec<HistoryEntry> {
    let mut hist = settings::load_history();
    hist.reverse(); // stored oldest-first; the UI wants newest at the top
    hist
}

/// Wipe the utterance history.
#[tauri::command]
pub fn clear_history() {
    settings::clear_history();
}

/// One-shot "explain my screen" (Ctrl+Shift+E equivalent): capture + concise
/// preset explanation. Also exposed as a command so the webview can trigger it.
#[tauri::command]
pub async fn quick_explain<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    crate::run_quick_explain(app).await.map_err(|e| format!("{e:#}"))
}

/// Step 2 of the "ask about my screen" shortcut: the user submitted their typed
/// question; pair it with the parked screenshot and stream the answer.
#[tauri::command]
pub async fn ask_submit<R: Runtime>(app: AppHandle<R>, question: String) -> Result<(), String> {
    crate::run_quick_ask(app, question)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// The user dismissed the "ask" input box: drop the parked screenshot so it
/// isn't paired with a later, unrelated question.
#[tauri::command]
pub fn ask_cancel(state: State<'_, AppState>) {
    *state.pending_shot.lock() = None;
}

/// The freeze-frame selector requests the parked screenshot to display as its
/// dimmable, magnifiable background. Returns null if nothing is parked.
#[tauri::command]
pub fn get_region_shot<R: Runtime>(app: AppHandle<R>) -> Option<serde_json::Value> {
    crate::region_shot_payload(&app)
}

/// The user released a selection rectangle on the freeze-frame overlay. `x/y/w/h`
/// are in the FROZEN image's pixel space (the overlay maps screen points → image
/// pixels). Crops + routes to Explain / Ask.
#[tauri::command]
pub fn region_submit<R: Runtime>(app: AppHandle<R>, x: f64, y: f64, w: f64, h: f64) {
    crate::finish_region(app, x, y, w, h);
}

/// The user pressed Esc on the freeze-frame overlay: drop the parked frame and
/// hide the selector window.
#[tauri::command]
pub fn region_cancel<R: Runtime>(app: AppHandle<R>) {
    {
        let state = app.state::<AppState>();
        *state.pending_region.lock() = None;
    }
    if let Some(win) = app.get_webview_window("capture") {
        let _ = win.hide();
    }
}

/// Screen Recording health for the settings panel. `authorized` = the TCC toggle
/// (CGPreflight on macOS, the system capture prompt on Windows); `working` = a
/// fresh capture actually returns real (non-black) pixels. They diverge in the
/// common "granted but still black until restart" case, so the UI must show
/// `working`, not just `authorized`. `settings_open_supported` tells the
/// frontend whether the "Open System Settings → Screen Recording" button has a
/// destination — true on macOS, false on Windows (no per-app toggle exists).
#[tauri::command]
pub fn screen_capture_health() -> serde_json::Value {
    let authorized = crate::permission::screen_capture_authorized();
    // Only probe when authorized — if not, it's black anyway, and the probe
    // itself could trip a permission prompt.
    let working = authorized && !crate::capture::capture_is_black();
    let settings_open_supported = cfg!(target_os = "macos");
    serde_json::json!({
        "authorized": authorized,
        "working": working,
        "settings_open_supported": settings_open_supported,
    })
}

/// Backwards-compatible alias for `screen_capture_health`. Older frontends
/// call this name; we add the new `settings_open_supported` field on top so
/// they degrade gracefully (the field is simply ignored if the UI doesn't
/// read it).
#[tauri::command]
pub fn screen_capture_status() -> serde_json::Value {
    screen_capture_health()
}

/// Trigger the one-time system Screen Recording prompt (effective only on first
/// ask). Returns the resulting grant state.
#[tauri::command]
pub fn request_screen_capture() -> bool {
    crate::permission::request_screen_capture()
}

/// Tell the click-through poller that interactive overlay UI (bubble / ask box /
/// hover toolbar) is open or closed. While open, the whole window catches mouse
/// events; while closed, only the sprite area does (the rest passes through).
#[tauri::command]
pub fn set_overlay_interactive<R: Runtime>(app: AppHandle<R>, on: bool) {
    {
        let state = app.state::<AppState>();
        state
            .overlay_interactive
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }
    // Make "on" responsive immediately (don't wait for the next poll tick).
    if on {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_ignore_cursor_events(false);
        }
    }
}

/// Open System Settings directly at the Screen Recording privacy pane so the
/// user can toggle the grant (needed after the first prompt is dismissed).
#[tauri::command]
pub fn open_screen_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .spawn();
    }
}
