# peeky

English · [简体中文](./README.zh.md)

> A transparent floating **macOS desktop AI companion** — a little magic sprite
> that peeks at your screen and chimes in at the right moment.

peeky is built with [Tauri 2](https://tauri.app): a Rust backend (`src-tauri/`)
plus a vanilla TypeScript + Vite webview frontend (`src/`). It's a draggable,
decoration-less character that lives anywhere on your desktop, periodically
perceives what's on your screen, and speaks **at the right moment — and stays
quiet the rest of the time.** It also gives you instant **screenshot → ask /
explain / translate** shortcuts.

The idea: AI shouldn't only show up when you open a chat box. It should sit
beside you like a friend, notice what you're doing, and help when it actually
helps.

> Platform: **macOS** (Apple Silicon / Intel) and **Windows 11**.

## Features

- **Transparent floating mascot** — a procedural inline-SVG character (no sprite
  assets), draggable anywhere, always-on-top, click-through except over the
  sprite. The result card renders streamed **Markdown**.
- **Perceive → maybe speak loop** — a 500 ms background loop captures the screen,
  runs a cheap pHash + scroll change-detector (no model, no OCR), debounces, then
  calls the model only when something meaningful changed.
- **Personality modes** — `roast` / `nerd` / `copilot` (`Ctrl+Shift+M` to cycle).
  Prompts live in `src-tauri/prompts/*.md`.
- **Quick screenshot shortcuts** — freeze the screen and drag a precise region
  with a magnifier loupe, then:
  - `Ctrl+Shift+E` — **Explain** the selection
  - `Ctrl+Shift+B` — **Ask** a typed question about it
  - `Ctrl+Shift+T` — **Translate** it + a short vocabulary note
- **Restraint engine** — per-hour speech budget, quiet hours, follow system
  Focus / Do-Not-Disturb, fullscreen auto-pause — so peeky never becomes Clippy.
- **OpenAI-compatible streaming** — any `/chat/completions` endpoint (cloud,
  private, or local). Vision messages, SSE streaming, token accounting.
  **TLS is never disabled; keys are never hardcoded.**
- **Reasoning effort control** — Off / Low / Medium / High, to trade speed for
  quality on reasoning models (StepFun, GPT-5/o-series, Qwen, DeepSeek …).
- **Multi-language** — full **zh / ja / en** UI + replies; default from system
  locale, switchable in settings.

## Run

Prerequisites:

- **macOS**: macOS 11+, [Rust](https://rustup.rs) (stable), [pnpm](https://pnpm.io),
  Xcode command-line tools.
- **Windows 11**: [Rust](https://rustup.rs) (stable, ≥ 1.85), [pnpm](https://pnpm.io),
  Visual Studio Build Tools (C++ workload), WebView2 runtime (preinstalled on
  Windows 11).

```sh
pnpm install
pnpm tauri dev      # dev build (image ops are slower in debug)
pnpm tauri build    # production .app / .dmg  (macOS)  ·  .exe / NSIS / .msi  (Windows)
```

### macOS permissions

Grant peeky **Screen Recording** (required — the whole point) and
**Accessibility** (for window context + copilot input) in **System Settings →
Privacy & Security**, then quit and reopen it.

> Note: macOS is finicky about Screen Recording for ad-hoc-signed builds — on
> macOS 15 an ad-hoc build may be "granted" yet still capture a black frame.
> Build with a real Apple Development signature so the grant sticks:
> `APPLE_SIGNING_IDENTITY="<your identity>" pnpm tauri build`.

### Windows permissions

Windows has **no per-app Screen Recording toggle** — the first screen capture
inside peeky triggers a **system-level** capture consent prompt. Accept it
once; peeky then captures normally. If a frame ever comes back blank, fully
quit and relaunch peeky (DWM quirk on some adapters).

### Configure the model

Open settings (gear on hover, or `Ctrl+Shift+S`) and set **Base URL**, **API
Key**, **Model** (e.g. `https://api.stepfun.com/v1` / `step-3.7-flash`).
The key can instead come from the `PEEKY_API_KEY` env var (see `.env.example`) —
**keys are never committed.** Use **Test connection** to verify.

### Shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+Space` | Manual trigger (capture + speak) |
| `Ctrl+Shift+E` / `B` / `T` | Region select → explain / ask / translate |
| `Ctrl+Shift+M` | Cycle personality mode |
| `Ctrl+Shift+P` | Pause / resume |
| `Ctrl+Shift+S` | Open settings |

Single-click the mascot toggles its card; double-click pauses; right-click fires
a manual trigger.

## Project layout

- `src-tauri/` — Rust backend (capture, trigger, api, modes, restraint, memory,
  tools, permission, commands; main loop in `lib.rs`).
- `src/` — TypeScript frontend (mascot, region selector, settings, i18n, glue).
- `src-tauri/prompts/` — per-mode + quick-shortcut system prompts.
- `ARCHITECTURE.md` — deeper design notes · `CLAUDE.md` — contributor/agent guide.

## License

[MIT](./LICENSE).
