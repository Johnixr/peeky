//! Peeky library crate root.
//!
//! Owns: module wiring, the Tauri builder, global-shortcut registration, the
//! command invoke handler, and the 500ms background "perceive → maybe speak"
//! main loop (PRD §2.1 / §2.2 / §4). The loop calls into the other modules
//! strictly through their contract signatures; Integration reconciles any
//! drift.

// ---- Module declarations (one per ownership boundary) -----------------------
pub mod agent;
pub mod api;
pub mod capture;
pub mod commands;
pub mod error;
pub mod memory;
pub mod modes;
pub mod permission;
pub mod restraint;
pub mod settings;
pub mod state;
pub mod tools;
pub mod trigger;
pub mod platform;
pub mod types;

use std::time::Duration;

use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

use crate::state::AppState;
use crate::types::{mascot_state, Config, ModeKind};

// ---- Event names (Rust app.emit -> JS listen) -------------------------------
const EV_STATE: &str = "peeky://state";
const EV_SPEAK: &str = "peeky://speak";
const EV_TOKEN: &str = "peeky://token";
const EV_SILENT: &str = "peeky://silent";
const EV_CONFIG_CHANGED: &str = "peeky://config-changed";
const EV_ERROR: &str = "peeky://error";
const EV_STATUS: &str = "peeky://status";
/// Tells the overlay to show the "ask about my screen" input box (shortcut B).
const EV_ASK: &str = "peeky://ask";
/// Tells the freeze-frame selector window to (re)load the parked screenshot.
const EV_REGION_INIT: &str = "peeky://region-init";

/// Show a transient "doing a tool" status line in the bubble (copilot mode).
/// `key` is an i18n key the frontend localizes; `detail` is appended raw.
fn emit_status<R: tauri::Runtime>(app: &tauri::AppHandle<R>, key: &str, detail: &str) {
    let _ = app.emit(EV_STATUS, serde_json::json!({ "key": key, "detail": detail }));
}

/// Helper: emit a mascot state-machine transition.
fn emit_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &str) {
    let _ = app.emit(EV_STATE, serde_json::json!({ "state": state }));
}

/// Surface a problem to the user via the mascot bubble. `key` is an i18n key the
/// frontend localizes (e.g. "error.config_incomplete"); `detail` is an optional
/// raw message (network/model text) appended after it. Also logged to stderr so
/// `pnpm tauri dev` shows what went wrong.
fn emit_error<R: tauri::Runtime>(app: &tauri::AppHandle<R>, key: &str, detail: &str) {
    eprintln!("[peeky] error {key}: {detail}");
    let _ = app.emit(EV_ERROR, serde_json::json!({ "key": key, "detail": detail }));
}

/// Application entrypoint, invoked from `main.rs`.
pub fn run() {
    // Load persisted config (falls back to Config::default(), and overlays the
    // PEEKY_API_KEY env if present — see settings.rs). Never panics.
    let config = settings::load_config();
    let stats = settings::load_stats();
    let app_state = AppState::new(config, stats);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(build_global_shortcut_plugin())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::get_system_locale,
            commands::trigger_now,
            commands::pause_toggle,
            commands::set_mode,
            commands::set_permission_mode,
            commands::get_token_stats,
            commands::test_api_connection,
            commands::open_settings,
            commands::get_history,
            commands::clear_history,
            commands::quick_explain,
            commands::ask_submit,
            commands::ask_cancel,
            commands::get_region_shot,
            commands::region_submit,
            commands::region_cancel,
            commands::screen_capture_status,
            commands::screen_capture_health,
            commands::request_screen_capture,
            commands::open_screen_settings,
            commands::set_overlay_interactive,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // Spawn the perceive loop on Tauri's async (tokio) runtime.
            tauri::async_runtime::spawn(async move {
                main_loop(handle).await;
            });

            // Pre-warm the static context (computer name) off the hot path,
            // so the first quick-shot doesn't pay it at submit time.
            tauri::async_runtime::spawn(async {
                let _ = tauri::async_runtime::spawn_blocking(|| {
                    let _ = system_context("");
                })
                .await;
            });

            // Click-through poller for the transparent overlay (so empty areas of
            // the window don't steal clicks meant for apps underneath).
            let ct_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                overlay_click_through_loop(ct_handle).await;
            });


            // Settings window: intercept the native close button so it HIDES
            // instead of being destroyed — otherwise Ctrl+Shift+S / the gear
            // could not reopen it (the labeled window would no longer exist).
            if let Some(settings_win) = app.get_webview_window("settings") {
                let w = settings_win.clone();
                settings_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            // The freeze-frame selector hides (not destroys) on close so it can be
            // reused on the next shortcut press; dropping the parked frame too.
            if let Some(capture_win) = app.get_webview_window("capture") {
                let w = capture_win.clone();
                let handle = app.handle().clone();
                capture_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                        let state = handle.state::<AppState>();
                        *state.pending_region.lock() = None;
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Peeky");
}

/// Build the global-shortcut plugin with the PRD §8.1 bindings. The handler
/// fires on key-press only and dispatches to the matching command behavior.
fn build_global_shortcut_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    // Ctrl+Shift+<key>: Space=trigger, M=cycle mode, P=pause, S=settings,
    // E=explain (one-shot), B=ask (one-shot w/ text), T=translate (one-shot).
    let mods = Modifiers::CONTROL | Modifiers::SHIFT;
    let sc_trigger = Shortcut::new(Some(mods), Code::Space);
    let sc_mode = Shortcut::new(Some(mods), Code::KeyM);
    let sc_pause = Shortcut::new(Some(mods), Code::KeyP);
    let sc_settings = Shortcut::new(Some(mods), Code::KeyS);
    let sc_explain = Shortcut::new(Some(mods), Code::KeyE);
    let sc_ask = Shortcut::new(Some(mods), Code::KeyB);
    let sc_translate = Shortcut::new(Some(mods), Code::KeyT);

    tauri_plugin_global_shortcut::Builder::new()
        .with_shortcuts([
            sc_trigger, sc_mode, sc_pause, sc_settings, sc_explain, sc_ask, sc_translate,
        ])
        .expect("invalid global shortcut definition")
        .with_handler(move |app, shortcut, event| {
            // Only react on the initial press to avoid auto-repeat storms.
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let handle = app.clone();
            if shortcut == &sc_trigger {
                // Manual trigger bypasses restraint (PRD §4 主动优先).
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = run_trigger_now(handle).await {
                        eprintln!("[peeky] trigger_now (shortcut) failed: {e:#}");
                    }
                });
            } else if shortcut == &sc_mode {
                cycle_mode(&handle);
            } else if shortcut == &sc_pause {
                toggle_pause(&handle);
            } else if shortcut == &sc_settings {
                // Show the dedicated, opaque settings window (PRD §8.2).
                commands::show_settings_window(&handle);
            } else if shortcut == &sc_explain {
                // One-shot: freeze the screen → drag a region → explain it.
                begin_region_select(handle, modes::QuickKind::Explain);
            } else if shortcut == &sc_ask {
                // One-shot: freeze the screen → drag a region → ask about it.
                begin_region_select(handle, modes::QuickKind::Ask);
            } else if shortcut == &sc_translate {
                // One-shot: freeze the screen → drag a region → translate it.
                begin_region_select(handle, modes::QuickKind::Translate);
            }
        })
        .build()
}

/// Cycle the active mode and notify the frontend via config-changed.
fn cycle_mode<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let state = app.state::<AppState>();
    let new_cfg = {
        let mut cfg = state.config.lock();
        cfg.mode = cfg.mode.next();
        cfg.clone()
    };
    settings::save_config(&new_cfg);
    let _ = app.emit(EV_CONFIG_CHANGED, &new_cfg);
}

/// Toggle pause; update the mascot to/from the sleeping state (PRD §6.2/§7).
fn toggle_pause<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let state = app.state::<AppState>();
    let now_paused = state.toggle_paused();
    emit_state(
        app,
        if now_paused {
            mascot_state::PAUSED
        } else {
            mascot_state::IDLE
        },
    );
}

/// The shared body behind both the Ctrl+Shift+Space shortcut and the
/// `trigger_now` command: capture immediately and speak, bypassing restraint.
pub async fn run_trigger_now<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    // Don't stack calls on top of an in-flight one.
    if !state.try_begin_speaking() {
        return Ok(());
    }
    let result = perceive_and_speak(&app, true).await;
    state.end_speaking();
    result
}

// ---- One-shot "quick shortcut" features -------------------------------------
//
// Reusable, single-turn (NOT multi-turn) helpers, independent of the ambient
// perceive loop and the personality modes. Each freezes the screen and lets the
// user drag a region with the magnifier loupe, then:
//   • Explain   (Ctrl+Shift+E): a preset, concise explanation of the region.
//   • Ask       (Ctrl+Shift+B): pop an input box → answer the user's question.
//   • Translate (Ctrl+Shift+T): translate the region's text + a short vocab note.
// All inject runtime context (local time / active app / system) and stream the
// reply into the same mascot bubble.

/// Build the runtime context string handed to the quick prompts. The static
/// parts (user / computer / OS) are resolved once and cached; only the local
/// time and active-app name are recomputed per call (kept cheap — speed matters).
///
/// Cross-platform: `USER` is a POSIX env var so it's also populated by
/// Git-Bash / WSL on Windows (where the mascot may legitimately be running
/// inside a WSL distribution); on a stock Windows host it will be empty
/// and we just skip that segment. The OS label and computer name are
/// resolved via the `platform::system` module so neither of them bakes
/// macOS-only shell-outs in.
fn system_context(active_app: &str) -> String {
    use std::sync::OnceLock;
    static STATIC_CTX: OnceLock<String> = OnceLock::new();
    let stat = STATIC_CTX.get_or_init(|| {
        let user = std::env::var("USER").unwrap_or_default();
        let computer = crate::platform::system::computer_name();
        let os = crate::platform::system::os_label();
        let mut parts = Vec::new();
        if !user.is_empty() {
            parts.push(format!("user {user}"));
        }
        if !computer.is_empty() {
            parts.push(format!("computer \"{computer}\""));
        }
        parts.push(format!("OS {os}"));
        parts.join(", ")
    });
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    if active_app.is_empty() {
        format!("Local time {time}; {stat}.")
    } else {
        format!("Local time {time}; active app \"{active_app}\"; {stat}.")
    }
}

/// Ctrl+Shift+E: capture the frontmost window and stream a concise explanation.
pub async fn run_quick_explain<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    if !state.try_begin_speaking() {
        return Ok(());
    }
    if !screen_permission_ok(&app, true) {
        emit_state(&app, mascot_state::IDLE);
        state.end_speaking();
        return Ok(());
    }
    let cfg = state.config_snapshot();
    emit_state(&app, mascot_state::SCANNING);
    let ctx_app = trigger::front_app_context().app;
    let captured = match capture::capture_active_window(cfg.screenshot_quality) {
        Ok(c) => c,
        Err(e) => {
            emit_error(&app, "error.capture", &format!("{e:#}"));
            emit_state(&app, mascot_state::IDLE);
            state.end_speaking();
            return Ok(());
        }
    };
    let ts = (now_ms() / 1000) as i64;
    let res = quick_stream(
        &app,
        &cfg,
        modes::QuickKind::Explain,
        "",
        &captured,
        &ctx_app,
        ts,
    )
    .await;
    state.end_speaking();
    res
}

/// Quick-shortcut step 1 (both Explain and Ask): freeze the whole screen NOW
/// (before any focus shifts, so the frame is exactly what the user saw), park it,
/// then show the fullscreen freeze-frame selector overlay where the user drags a
/// precise region with a magnifier loupe. Step 2 is `finish_region` on release.
pub fn begin_region_select<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    kind: modes::QuickKind,
) {
    tauri::async_runtime::spawn(async move {
        let t0 = std::time::Instant::now();
        let state = app.state::<AppState>();

        // Permission gate FIRST: a black frame (no Screen Recording grant) would
        // just make the model hallucinate. Guide the user instead.
        if !screen_permission_ok(&app, true) {
            return;
        }

        // Resolve the ACCURATE foreground app name (osascript) CONCURRENTLY with
        // the capture, so the overlay isn't delayed waiting on it. It's only
        // needed later at submit time (for prompt context).
        let name_task = tauri::async_runtime::spawn_blocking(trigger::frontmost_app_name);

        let cap_t = std::time::Instant::now();
        let (img, logical_w, logical_h) = match capture::capture_full() {
            Ok(v) => v,
            Err(e) => {
                emit_error(&app, "error.capture", &format!("{e:#}"));
                return;
            }
        };
        eprintln!(
            "[peeky] region capture: {} ms (raw {}x{}, logical {}x{})",
            cap_t.elapsed().as_millis(),
            {
                use image::GenericImageView as _;
                img.dimensions().0
            },
            {
                use image::GenericImageView as _;
                img.dimensions().1
            },
            logical_w,
            logical_h
        );

        // The TCC toggle can read "granted" while the capture is still BLACK
        // (grant not yet effective for this process). Don't show a black overlay
        // or send a black image to the model — guide the user to restart.
        if capture::is_black(&img) {
            eprintln!("[peeky] region capture is BLACK despite permission — needs app restart");
            emit_error(&app, "error.screen_black", "");
            return;
        }

        {
            let mut slot = state.pending_region.lock();
            *slot = Some(crate::state::RegionShot {
                kind,
                img,
                app: String::new(), // filled in below once the name resolves
            });
        }

        // Show the fullscreen selector IMMEDIATELY (don't wait for the app name).
        if let Some(win) = app.get_webview_window("capture") {
            let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition {
                x: 0.0,
                y: 0.0,
            }));
            let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize {
                width: logical_w as f64,
                height: logical_h as f64,
            }));
            let _ = win.show();
            let _ = win.set_focus();
            let _ = app.emit(EV_REGION_INIT, serde_json::json!({}));
            eprintln!(
                "[peeky] region overlay shown: {} ms (since keypress)",
                t0.elapsed().as_millis()
            );
        } else {
            *state.pending_region.lock() = None;
            emit_error(&app, "error.capture", "selector window missing");
            return;
        }

        // Backfill the app name when the concurrent osascript finishes (well
        // before the user finishes dragging). Skip if already submitted/cleared.
        if let Ok(name) = name_task.await {
            let mut g = state.pending_region.lock();
            if let Some(rs) = g.as_mut() {
                if rs.app.is_empty() {
                    rs.app = name;
                }
            }
        }
        eprintln!("[peeky] region app-name ready: {} ms", t0.elapsed().as_millis());
    });
}

/// Verify Screen Recording permission before a capture the user is waiting on.
/// When missing: surface a localized guide in the bubble and (first run) trigger
/// the system prompt. Returns false if capture must NOT proceed.
fn screen_permission_ok<R: tauri::Runtime>(app: &tauri::AppHandle<R>, announce: bool) -> bool {
    if permission::screen_capture_authorized() {
        return true;
    }
    if announce {
        emit_error(app, "error.screen_permission", "");
        // Show the one-time system dialog so the app is added to the list.
        let _ = permission::request_screen_capture();
    }
    false
}

/// Quick-shortcut step 2: the user released a selection rectangle (in frozen-image
/// pixel coordinates). Crop it out of the parked frame and route it: Explain →
/// stream an explanation; Ask → park the crop + pop the question input box.
pub fn finish_region<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    if !x.is_finite()
        || !y.is_finite()
        || !w.is_finite()
        || !h.is_finite()
        || w <= 0.0
        || h <= 0.0
    {
        eprintln!(
            "[peeky] ignoring invalid region rect: x={x:.1} y={y:.1} w={w:.1} h={h:.1}"
        );
        return;
    }

    let state = app.state::<AppState>();
    let shot = match state.pending_region.lock().take() {
        Some(s) => s,
        None => return,
    };
    // Hide the selector overlay immediately — the freeze-frame is done.
    if let Some(win) = app.get_webview_window("capture") {
        let _ = win.hide();
    }

    let cfg = state.config_snapshot();
    let captured = match capture::crop_to_captured(&shot.img, x, y, w, h, cfg.screenshot_quality) {
        Ok(c) => c,
        Err(e) => {
            emit_error(&app, "error.capture", &format!("{e:#}"));
            return;
        }
    };
    // Keep a copy of the exact image sent to the model (support diagnostic).
    {
        use base64::Engine as _;
        if let Ok(bytes) =
            base64::engine::general_purpose::STANDARD.decode(captured.png_base64.as_bytes())
        {
            let _ = std::fs::write(
                std::env::temp_dir().join("peeky_last_capture.jpg"),
                &bytes,
            );
        }
    }
    let ts = (now_ms() / 1000) as i64;

    let kind = shot.kind;
    match kind {
        // Explain + Translate both stream a one-shot answer (no typed input).
        modes::QuickKind::Explain | modes::QuickKind::Translate => {
            let app2 = app.clone();
            let app_name = shot.app;
            tauri::async_runtime::spawn(async move {
                let state = app2.state::<AppState>();
                if !state.try_begin_speaking() {
                    return;
                }
                let cfg = state.config_snapshot();
                let res = quick_stream(&app2, &cfg, kind, "", &captured, &app_name, ts).await;
                state.end_speaking();
                if let Err(e) = res {
                    eprintln!("[peeky] region {kind:?} failed: {e:#}");
                }
            });
        }
        modes::QuickKind::Ask => {
            {
                let mut slot = state.pending_shot.lock();
                *slot = Some(crate::state::PendingShot {
                    captured,
                    app: shot.app,
                    ts,
                });
            }
            // Focus the overlay so the question input box can receive keys.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
            let _ = app.emit(EV_ASK, serde_json::json!({}));
        }
    }
}

/// Encode the parked freeze-frame as a JPEG data URL for the selector overlay to
/// show as its dimmable, magnifiable background. JPEG (not PNG) keeps this off
/// the slow path. Returns `None` if nothing is parked.
pub fn region_shot_payload<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<serde_json::Value> {
    let state = app.state::<AppState>();
    let guard = state.pending_region.lock();
    let shot = guard.as_ref()?;
    // Report the FULL image dimensions: the overlay maps the user's selection to
    // these pixels (and crops from the full image), while the displayed JPEG is a
    // cheap low-res copy stretched to fill — its own resolution is irrelevant to
    // the crop math.
    let (w, h) = {
        use image::GenericImageView as _;
        shot.img.dimensions()
    };
    // Near-native resolution (only downscales screens wider than 3840) so the
    // frozen frame laid over the desktop stays crisp, not blurry.
    let enc_t = std::time::Instant::now();
    let b64 = capture::encode_display_jpeg(&shot.img, 3840).ok()?;
    eprintln!(
        "[peeky] region display jpeg: {} ms ({} KB)",
        enc_t.elapsed().as_millis(),
        b64.len() / 1024
    );
    Some(serde_json::json!({
        "url": format!("data:image/jpeg;base64,{b64}"),
        "w": w,
        "h": h
    }))
}

/// Ctrl+Shift+B step 2: the user submitted a question — pair it with the parked
/// screenshot and stream the answer. No-ops if nothing is parked (canceled).
pub async fn run_quick_ask<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    question: String,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let pending = { state.pending_shot.lock().take() };
    let Some(shot) = pending else {
        return Ok(());
    };
    if !state.try_begin_speaking() {
        return Ok(());
    }
    let cfg = state.config_snapshot();
    let res = quick_stream(
        &app,
        &cfg,
        modes::QuickKind::Ask,
        &question,
        &shot.captured,
        &shot.app,
        shot.ts,
    )
    .await;
    state.end_speaking();
    res
}

/// Shared one-shot streaming path for the quick shortcuts: build messages,
/// stream tokens to the bubble, record stats + history. Independent of rolling
/// memory (these are isolated single-turn answers, not the ambient narration).
async fn quick_stream<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    cfg: &Config,
    kind: modes::QuickKind,
    question: &str,
    captured: &crate::types::CapturedImage,
    ctx_app: &str,
    ts: i64,
) -> anyhow::Result<()> {
    if cfg.api_base_url.trim().is_empty()
        || cfg.api_key.trim().is_empty()
        || cfg.model.trim().is_empty()
    {
        emit_state(app, mascot_state::IDLE);
        emit_error(app, "error.config_incomplete", "");
        return Ok(());
    }

    emit_state(app, mascot_state::THINKING);
    let prep_t = std::time::Instant::now();
    let lang = cfg.language.resolve();
    let context = system_context(ctx_app);
    let messages =
        modes::build_quick_messages(kind, lang, &context, &captured.png_base64, question);
    eprintln!(
        "[peeky] quick prep→send: {} ms (image {} KB)",
        prep_t.elapsed().as_millis(),
        captured.png_base64.len() / 1024
    );

    let _ = app.emit(EV_SPEAK, serde_json::json!({ "mode": mode_str(cfg.mode) }));
    let app_for_tokens = app.clone();
    let on_token = move |chunk: &str| {
        let _ = app_for_tokens.emit(EV_TOKEN, serde_json::json!({ "text": chunk, "done": false }));
    };

    let model_t = std::time::Instant::now();
    let (full_text, prompt_tokens, completion_tokens) =
        match api::stream_chat(cfg, messages, on_token).await {
            Ok(v) => v,
            Err(e) => {
                emit_error(app, "error.api", &format!("{e:#}"));
                emit_state(app, mascot_state::IDLE);
                return Ok(());
            }
        };
    eprintln!("[peeky] quick model: {} ms", model_t.elapsed().as_millis());

    let trimmed = full_text.trim();
    let is_silent = trimmed.is_empty() || trimmed == modes::SILENT_MARKER;

    let state = app.state::<AppState>();
    {
        let mut stats = state.stats.lock();
        stats.calls += 1;
        stats.prompt_tokens += prompt_tokens;
        stats.completion_tokens += completion_tokens;
        if is_silent {
            stats.silent += 1;
        }
    }

    if is_silent {
        let _ = app.emit(EV_SILENT, ());
        emit_state(app, mascot_state::IDLE);
        if trimmed.is_empty() {
            emit_error(app, "error.empty_response", "");
        }
    } else {
        let _ = app.emit(EV_TOKEN, serde_json::json!({ "text": "", "done": true }));
        emit_state(app, mascot_state::TALKING);
        settings::append_history(crate::types::HistoryEntry {
            ts,
            mode: mode_str(cfg.mode).to_string(),
            text: trimmed.to_string(),
            app: if ctx_app.is_empty() {
                None
            } else {
                Some(ctx_app.to_string())
            },
        });
        emit_state(app, mascot_state::IDLE);
    }

    let snapshot = state.stats.lock().clone();
    settings::save_stats(&snapshot);
    Ok(())
}

/// Make the transparent overlay click-through except over the sprite.
///
/// The window spans 360×420 but the visible sprite is small; without this the
/// whole frame would swallow clicks meant for apps beneath/around Peeky. We poll
/// the cursor (~60ms) and only let the window CATCH events when the pointer is
/// inside the centered sprite box — OR when interactive UI (bubble/ask/toolbar)
/// is open (the `overlay_interactive` flag). Everywhere else, clicks pass through.
async fn overlay_click_through_loop<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    use std::sync::atomic::Ordering;
    // Sprite half-extents in LOGICAL points (the sprite is taller than wide due
    // to the hat). Kept close to the actually-drawn sprite so the hit area
    // matches what the user sees.
    const HALF_W: f64 = 34.0;
    const HALF_H: f64 = 42.0;

    let mut ticker = tokio::time::interval(Duration::from_millis(60));
    let mut ignoring: Option<bool> = None; // None = not yet initialized
    loop {
        ticker.tick().await;
        let Some(win) = app.get_webview_window("main") else {
            continue;
        };
        if ignoring.is_none() {
            // Default to click-through until proven the cursor is on the sprite.
            let _ = win.set_ignore_cursor_events(true);
            ignoring = Some(true);
        }

        let interactive = app
            .state::<AppState>()
            .overlay_interactive
            .load(Ordering::Relaxed);

        let want_ignore = if interactive {
            false
        } else {
            match (
                win.cursor_position(),
                win.outer_position(),
                win.outer_size(),
                win.scale_factor(),
            ) {
                (Ok(cur), Ok(pos), Ok(size), Ok(scale)) => {
                    let cx = pos.x as f64 + size.width as f64 / 2.0;
                    let cy = pos.y as f64 + size.height as f64 / 2.0;
                    let inside =
                        (cur.x - cx).abs() <= HALF_W * scale && (cur.y - cy).abs() <= HALF_H * scale;
                    !inside
                }
                // On any read error, err toward interactive so the user is never
                // locked out of clicking the sprite.
                _ => false,
            }
        };

        if ignoring != Some(want_ignore) {
            let _ = win.set_ignore_cursor_events(want_ignore);
            ignoring = Some(want_ignore);
        }
    }
}

/// The 500ms perception loop (PRD §2.1 main loop). Resilient: every error is
/// logged and the loop continues.
async fn main_loop<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    loop {
        ticker.tick().await;

        let state = app.state::<AppState>();
        if state.is_paused() || state.is_speaking() {
            continue;
        }

        // L0 + L1 cheap gating: front-app context + grayscale 128 frame + pHash.
        let decision = match cheap_evaluate(&app) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[peeky] cheap_evaluate failed: {e:#}");
                continue;
            }
        };

        use crate::types::TriggerDecision::*;
        match decision {
            NoChange | Scroll => {
                // Scroll already updated reading progress inside the engine.
                continue;
            }
            Meaningful => {
                // L2 debounce: require ~800ms of stability before committing.
                let now = now_ms();
                state
                    .last_change_ms
                    .store(now, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(800)).await;
                // If something changed again during the debounce window, defer.
                let last = state.last_change_ms.load(std::sync::atomic::Ordering::Relaxed);
                if last != now {
                    continue;
                }

                // Restraint gate (budget / quiet hours / DND / fullscreen, §4).
                let ctx = trigger::front_app_context();
                let allowed = {
                    let cfg = state.config_snapshot();
                    let mut restraint = state.restraint.lock();
                    restraint.allow_speak(&cfg, &ctx)
                };
                if !allowed {
                    // Suppressed: nudge mascot to "has-something" so the user can
                    // pull it up on click/hover (PRD §4.2 / §6.2).
                    {
                        let mut restraint = state.restraint.lock();
                        restraint.record_ignored();
                    }
                    emit_state(&app, mascot_state::HAS_SOMETHING);
                    continue;
                }

                if !state.try_begin_speaking() {
                    continue;
                }
                let res = perceive_and_speak(&app, false).await;
                state.end_speaking();
                if let Err(e) = res {
                    eprintln!("[peeky] perceive_and_speak failed: {e:#}");
                    emit_state(&app, mascot_state::IDLE);
                }
            }
        }
    }
}

/// Cheap L0+L1 evaluation: grab a downsampled grayscale frame and run the
/// trigger engine. Does NOT call the model. Reads/updates engine state.
fn cheap_evaluate<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> anyhow::Result<crate::types::TriggerDecision> {
    let state = app.state::<AppState>();
    // Grab the 128px grayscale fingerprint directly — no full-quality capture,
    // no PNG/base64 roundtrip (PRD §2.2: the cheap gate must stay near-zero
    // cost). The full-quality capture only happens later, in perceive_and_speak.
    let gray128 = capture::capture_gray_128()?;

    let mut trigger = state.trigger.lock();
    Ok(trigger.evaluate(&gray128))
}

/// Full pipeline once we've decided to speak: capture → build messages →
/// stream → emit tokens → update memory & stats. `bypass` is informational
/// (the caller already handled restraint).
async fn perceive_and_speak<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    bypass: bool,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let cfg = state.config_snapshot();

    // Guard: incomplete model config. Don't hammer the API every tick; only tell
    // the user when they explicitly triggered (so the auto loop stays quiet until
    // they configure it in settings — PRD §1.5 / §8.2).
    if cfg.api_base_url.trim().is_empty()
        || cfg.api_key.trim().is_empty()
        || cfg.model.trim().is_empty()
    {
        emit_state(app, mascot_state::IDLE);
        if bypass {
            emit_error(app, "error.config_incomplete", "");
        }
        return Ok(());
    }

    // Screen Recording permission: without it, capture is a black frame. Only
    // nag on a manual trigger (the ambient loop self-silences on a static black
    // frame, so it won't spam).
    if !screen_permission_ok(app, bypass) {
        emit_state(app, mascot_state::IDLE);
        return Ok(());
    }

    // Copilot mode is an AGENT, not a narrator: it can call tools to actually act
    // on the screen (PRD §3.1-C / §5). It runs ON DEMAND (manual trigger /
    // Ctrl+Shift+Space), NOT ambiently on every screen change — autonomously
    // clicking around would be costly and risky.
    if cfg.mode == ModeKind::Copilot {
        if bypass {
            return copilot_loop(app, &cfg).await;
        }
        emit_state(app, mascot_state::IDLE);
        return Ok(());
    }

    emit_state(app, mascot_state::SCANNING);

    // Capture the frontmost window only (not the whole screen) so overlapping
    // apps + Peeky's own overlay don't confuse the model (PRD §2.3).
    let cap_t = std::time::Instant::now();
    let captured = match capture::capture_active_window(cfg.screenshot_quality) {
        Ok(c) => c,
        Err(e) => {
            emit_error(app, "error.capture", &format!("{e:#}"));
            emit_state(app, mascot_state::IDLE);
            return Ok(());
        }
    };
    eprintln!("[peeky] capture: {} ms", cap_t.elapsed().as_millis());

    emit_state(app, mascot_state::THINKING);

    let lang = cfg.language.resolve();
    // Reset carried context when the foreground app changed, so we don't narrate
    // a new app with stale lines from the previous one.
    let ctx = trigger::front_app_context();
    let memory_snapshot = {
        let mut mem = state.memory.lock();
        mem.observe_context(&ctx);
        mem.recent()
    };
    let messages = modes::build_messages(
        cfg.mode,
        lang,
        &captured.png_base64,
        &memory_snapshot,
        None,
    );

    // Signal the frontend that a fresh utterance is starting (clear bubble).
    let _ = app.emit(
        EV_SPEAK,
        serde_json::json!({ "mode": mode_str(cfg.mode) }),
    );

    // Stream tokens to the bubble as they arrive.
    let app_for_tokens = app.clone();
    let on_token = move |chunk: &str| {
        let _ = app_for_tokens.emit(
            EV_TOKEN,
            serde_json::json!({ "text": chunk, "done": false }),
        );
    };

    let model_t = std::time::Instant::now();
    let (full_text, prompt_tokens, completion_tokens) =
        match api::stream_chat(&cfg, messages, on_token).await {
            Ok(v) => v,
            Err(e) => {
                // Surface the failure (auth/network/model) instead of leaving the
                // mascot stuck "thinking" forever.
                emit_error(app, "error.api", &format!("{e:#}"));
                emit_state(app, mascot_state::IDLE);
                return Ok(());
            }
        };
    eprintln!("[peeky] model: {} ms", model_t.elapsed().as_millis());

    let trimmed = full_text.trim();
    // Strip the silent marker even if the model added stray whitespace/quotes.
    let is_silent = trimmed == modes::SILENT_MARKER || trimmed.is_empty();
    if is_silent {
        // No utterance. `<SILENT>` = intentional (PRD §3.1); empty content is
        // usually a problem (e.g. a forced hidden "thinking" mode ate the token
        // budget — PRD §1.5). Only nag on an explicit manual trigger.
        let _ = app.emit(EV_SILENT, ());
        emit_state(app, mascot_state::IDLE);
        if bypass && trimmed.is_empty() {
            emit_error(app, "error.empty_response", "");
        }
        {
            let mut stats = state.stats.lock();
            stats.calls += 1;
            stats.silent += 1;
            stats.prompt_tokens += prompt_tokens;
            stats.completion_tokens += completion_tokens;
        }
        {
            let mut restraint = state.restraint.lock();
            restraint.record_ignored();
        }
    } else {
        // Real utterance: remember it (don't-repeat), finish the stream.
        {
            let mut memory = state.memory.lock();
            memory.push(trimmed);
        }
        let _ = app.emit(
            EV_TOKEN,
            serde_json::json!({ "text": "", "done": true }),
        );
        emit_state(app, mascot_state::TALKING);
        {
            let mut stats = state.stats.lock();
            stats.calls += 1;
            stats.prompt_tokens += prompt_tokens;
            stats.completion_tokens += completion_tokens;
        }
        {
            let mut restraint = state.restraint.lock();
            restraint.record_shown();
        }
        // Persist to the reviewable history (settings → History tab).
        let app_name = trigger::front_app_context().app;
        settings::append_history(crate::types::HistoryEntry {
            ts: (now_ms() / 1000) as i64,
            mode: mode_str(cfg.mode).to_string(),
            text: trimmed.to_string(),
            app: if app_name.is_empty() { None } else { Some(app_name) },
        });
        // Persist stats, then settle back to idle after the bubble shows.
        let stats_snapshot = state.stats.lock().clone();
        settings::save_stats(&stats_snapshot);
        emit_state(app, mascot_state::IDLE);
    }

    Ok(())
}

/// The copilot agent loop (PRD §3.1-C / §5): capture → ask the model WITH tools
/// → execute its tool calls for real → re-capture → repeat, until it returns a
/// final answer, calls `finish`, or hits the step cap. Permission tiers (§3.2)
/// and the hard-forbidden list (§3.3) are enforced on every call.
async fn copilot_loop<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    cfg: &Config,
) -> anyhow::Result<()> {
    use serde_json::json;
    const MAX_STEPS: usize = 8;

    let state = app.state::<AppState>();
    let lang = cfg.language.resolve();
    let tools = agent::tool_defs();

    emit_state(app, mascot_state::SCANNING);
    let mut cap = match capture::capture_active_window(cfg.screenshot_quality) {
        Ok(c) => c,
        Err(e) => {
            emit_error(app, "error.capture", &format!("{e:#}"));
            emit_state(app, mascot_state::IDLE);
            return Ok(());
        }
    };

    let ctx = trigger::front_app_context();
    let memory_snapshot = {
        let mut mem = state.memory.lock();
        mem.observe_context(&ctx);
        mem.recent()
    };
    // Raw-JSON conversation so we can append assistant tool_calls + tool results.
    let mut messages: Vec<serde_json::Value> = modes::build_messages(
        ModeKind::Copilot,
        lang,
        &cap.png_base64,
        &memory_snapshot,
        Some("auto"),
    )
    .into_iter()
    .map(|m| serde_json::to_value(m).unwrap_or_else(|_| json!({})))
    .collect();

    emit_status(app, "status.thinking", "");
    emit_state(app, mascot_state::THINKING);

    for _ in 0..MAX_STEPS {
        let turn = match api::chat_with_tools(cfg, messages.clone(), &tools).await {
            Ok(t) => t,
            Err(e) => {
                emit_error(app, "error.api", &format!("{e:#}"));
                emit_state(app, mascot_state::IDLE);
                return Ok(());
            }
        };
        {
            let mut stats = state.stats.lock();
            stats.calls += 1;
            stats.prompt_tokens += turn.prompt_tokens;
            stats.completion_tokens += turn.completion_tokens;
        }

        // No tool calls → the model is done; show its text answer.
        if turn.tool_calls.is_empty() {
            finish_with_text(app, cfg, turn.content.as_deref().unwrap_or(""));
            return Ok(());
        }

        // Append the assistant message (carrying tool_calls) verbatim.
        messages.push(turn.assistant_message.clone());

        for tc in &turn.tool_calls {
            if tc.name == "finish" {
                let summary = tc.arguments.get("message").and_then(|m| m.as_str()).unwrap_or("");
                messages.push(json!({ "role": "tool", "tool_call_id": tc.id, "content": "ok" }));
                finish_with_text(app, cfg, summary);
                return Ok(());
            }

            emit_status(app, agent::status_key(&tc.name), &tool_detail(tc));
            emit_state(app, mascot_state::WORKING);

            let result = match agent::execute_tool(
                &tc.name,
                &tc.arguments,
                cfg.permission_mode,
                (cap.scale_x, cap.scale_y, cap.origin_x, cap.origin_y),
            ) {
                Ok(outcome) => outcome,
                Err(reason) => {
                    // Blocked (needs confirm) or failed: tell the user AND feed
                    // the reason back so the model can adapt or stop.
                    emit_error(app, "error.tool_blocked", &reason);
                    reason
                }
            };
            messages.push(json!({ "role": "tool", "tool_call_id": tc.id, "content": result }));
        }

        // Re-capture so the model sees the post-action screen.
        cap = match capture::capture_active_window(cfg.screenshot_quality) {
            Ok(c) => c,
            Err(e) => {
                emit_error(app, "error.capture", &format!("{e:#}"));
                emit_state(app, mascot_state::IDLE);
                return Ok(());
            }
        };
        emit_status(app, "status.reading", "");
        messages.push(
            serde_json::to_value(api::vision_user_message(
                "This is the screen after your actions. Continue with tools if needed, or reply with your final answer and NO tool call.",
                &cap.png_base64,
            ))
            .unwrap_or_else(|_| json!({})),
        );
    }

    emit_error(app, "error.agent_limit", "");
    emit_state(app, mascot_state::IDLE);
    Ok(())
}

/// Show a final text answer in the bubble + persist to memory/history/stats.
/// Empty / `<SILENT>` shows nothing. Shared by the copilot loop's terminal cases.
fn finish_with_text<R: tauri::Runtime>(app: &tauri::AppHandle<R>, cfg: &Config, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == modes::SILENT_MARKER {
        let _ = app.emit(EV_SILENT, ());
        emit_state(app, mascot_state::IDLE);
        return;
    }
    let state = app.state::<AppState>();
    let _ = app.emit(EV_SPEAK, serde_json::json!({ "mode": "copilot" }));
    // Whole answer as one chunk; the bubble typewriter animates it.
    let _ = app.emit(EV_TOKEN, serde_json::json!({ "text": trimmed, "done": false }));
    let _ = app.emit(EV_TOKEN, serde_json::json!({ "text": "", "done": true }));
    emit_state(app, mascot_state::TALKING);
    state.memory.lock().push(trimmed);
    let app_name = trigger::front_app_context().app;
    settings::append_history(crate::types::HistoryEntry {
        ts: (now_ms() / 1000) as i64,
        mode: mode_str(cfg.mode).to_string(),
        text: trimmed.to_string(),
        app: if app_name.is_empty() { None } else { Some(app_name) },
    });
    let stats_snapshot = state.stats.lock().clone();
    settings::save_stats(&stats_snapshot);
    emit_state(app, mascot_state::IDLE);
}

/// Compact human detail appended to a tool's status line (e.g. the typed text).
fn tool_detail(tc: &api::ToolCall) -> String {
    match tc.name.as_str() {
        "type_text" => tc
            .arguments
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .chars()
            .take(24)
            .collect(),
        "key" => tc.arguments.get("combo").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        "scroll" => tc.arguments.get("direction").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

// ---- small helpers ----------------------------------------------------------

fn mode_str(mode: ModeKind) -> &'static str {
    match mode {
        ModeKind::Roast => "roast",
        ModeKind::Nerd => "nerd",
        ModeKind::Copilot => "copilot",
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
