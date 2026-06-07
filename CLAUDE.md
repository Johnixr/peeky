# CLAUDE.md — working notes for Claude / agents

Guidance for AI agents (and humans) contributing to **peeky**. Read this before
making changes.

## What peeky is

A transparent floating **desktop AI companion** built with **Tauri 2**:
a Rust backend (`src-tauri/`) + a **vanilla TypeScript + Vite** webview frontend
(`src/`). A draggable, decoration-less character that watches the screen via
periodic screenshots and speaks at the right moments, plus on-demand
screenshot-and-ask shortcuts. Runs on **macOS** (Apple Silicon / Intel) and
**Windows 11** — see *Platform support* below.

## Layout

- `src-tauri/src/` — Rust. Entry/wiring + global shortcuts + main loop: `lib.rs`.
  Modules: `capture` (screen capture + crop + JPEG), `trigger` (cheap pHash /
  scroll gate + front-app context), `api` (OpenAI-compatible streaming client),
  `modes` (system-prompt assembly), `agent`/`tools` (copilot tool-calling),
  `restraint`, `memory`, `settings`, `permission` (macOS Screen Recording),
  `commands` (the `#[tauri::command]` surface), `types` (the `Config` + shared
  serde types), `state`, **`platform/`** (per-OS front-app / system / clipboard
  backends — see *Platform support* below).
- `src-tauri/prompts/*.md` — system prompts (roast/nerd/copilot + quick_*).
- `src/` — frontend: `mascot.ts/.css` (the sprite + result bubble + ask box),
  `capture-main.ts` (the fullscreen region-selector overlay), `settings.ts`
  (settings window), `main.ts` (event glue), `markdown.ts`, `i18n/`.
- Three Vite HTML entries: `index.html` (overlay), `settings.html`, `capture.html`.

## Build / run / test

```sh
pnpm install
pnpm tauri dev        # dev (NOTE: debug build — image ops are much slower)
pnpm build            # frontend only: tsc --noEmit (strict) + vite build
cd src-tauri && cargo test     # Rust unit tests
cd src-tauri && cargo check    # keep this at ZERO warnings
```

**Release / signed build** (required for screen capture to actually work — see
below):

```sh
APPLE_SIGNING_IDENTITY="<your Apple Development identity>" pnpm tauri build
```

## Conventions (please keep these green)

- `cargo check`/`cargo test` must stay at **0 warnings, all tests passing**.
- `pnpm build` (strict `tsc`) must pass.
- **i18n parity**: `src/i18n/{en,zh,ja}.json` must have the **identical key set**.
  Add any new UI string to all three. Quick check:
  `node -e 'const e=require("./src/i18n/en.json"),z=require("./src/i18n/zh.json"),j=require("./src/i18n/ja.json");const k=o=>Object.keys(o).sort();const eq=(a,b)=>{a=k(a);b=k(b);return a.length===b.length&&a.every((x,i)=>x===b[i])};console.log(eq(e,z)&&eq(e,j))'`
- Model/system prompts live in `src-tauri/prompts/*.md` (embedded via
  `include_str!`), not inline in Rust.
- Prompt design: keep the **system prompt stable** (cache-friendly) and put
  per-call/variable content (context, user directive, image) at the **end** of
  the user message.

## Platform support

The OS-agnostic core (capture, pHash trigger, restraint, memory, agent loop)
runs on both macOS and Windows 11. Everything that needs OS-specific
information is isolated under `src-tauri/src/platform/`:

| Concern | macOS | Windows |
|---|---|---|
| Frontmost app + window title | `platform::trigger::macos` (AppleScript via `osascript`) | `platform::trigger::windows` (Win32 FFI: `GetForegroundWindow` → `GetWindowThreadProcessId` → `QueryFullProcessImageNameW`) |
| Browser active URL | AppleScript (`return URL of active tab of front window`) | Returns `None` in v1 — model falls back to the window title. (TODO: `UIAutomationCore` + `Address Band Root` control.) |
| Computer name | `platform::system::macos` (AppleScript) | `std::env::var("COMPUTERNAME")` |
| Clipboard paste-and-restore | `pbcopy` / `pbpaste` + `Cmd+V` | `clip.exe` + `powershell.exe Get-Clipboard` + `Ctrl+V` |
| `is_overlay_or_system_window` skip list | macOS system apps | Windows system apps (DWM, explorer, SearchUI, ShellExperienceHost, …) |

Windows FFI uses `windows-sys = 0.59` (compiled in only under
`[target.'cfg(windows)'.dependencies]`) with the smallest set of features
needed: `Win32_Foundation`, `Win32_UI_WindowsAndMessaging`,
`Win32_System_Threading`, `Win32_System_ProcessStatus`, `Win32_Graphics_Gdi`,
`Win32_Security`.

v1 accepts two Windows-specific gaps (tracked as `peeky-windows-1` /
`peeky-windows-2`):

- **Browser current URL** — model can still reason from the window title.
- **Focus Assist state** — `probe_system_dnd` returns `false` on Windows, so
  `follow_system_dnd` is preserved as a setting but does not auto-engage.
  Full impl: read `HKCU\Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\$$windows.data.shell.focusassist\Current\Value`.

## macOS-specific notes

- **Screen Recording permission** is required and is **finicky with ad-hoc
  signatures**: on macOS 15 an ad-hoc-signed build can be granted yet still
  capture an all-black frame. Build with a real **Apple Development** signature
  (stable designated requirement) so the grant sticks across rebuilds. Capture
  goes through in-process `xcap` first, falling back to the `screencapture` CLI.
- Accessibility permission is needed for `osascript` front-app context + copilot
  input tools (`enigo`).

## Windows-specific notes

- There is **no per-app Screen Recording toggle** on Windows. The first call to
  `xcap::Monitor::capture_image` triggers a **system-level** capture consent
  prompt (UAC-style, shown by the shell) — not a per-app switch the user has
  to find in Settings. After the user accepts, `screen_capture_health` reports
  `authorized=true`; if the frame comes back black, surface a "quit and relaunch"
  hint (DWM quirk with some adapters).
- **No Accessibility prerequisite** for `enigo` input on Windows — it uses
  `SendInput`, which works without a separate permission grant.
- The transparent + always-on-top + click-through window is provided by
  WebView2 + the same `transparent / decorations: false / alwaysOnTop /
  skipTaskbar` flags; no special "private API" toggle is needed.
- **WebView2 runtime** is bundled with Windows 11 and is also auto-installed by
  the NSIS/MSI installer; Windows 10 users need it pre-installed.

## Security red-lines (do NOT cross)

- **Never hardcode API keys.** Keys come from settings or the `PEEKY_API_KEY`
  env; an env-supplied key is never written to disk.
- **Never disable TLS verification** (no `danger_accept_invalid_certs`).
- **Never commit secrets or PII** — no keys, no private endpoints/IPs, no signing
  identity (pass it via the `APPLE_SIGNING_IDENTITY` env at build time, not in
  `tauri.conf.json`). The internal product spec is kept out of this repo.
- Copilot/agent actions honor a hard-forbidden list (delete files, quit/close
  apps, shutdown/restart, system settings, sudo, password managers, install/
  uninstall, browser data). These are blocked, never executed.

`AGENTS.md` is a symlink to this file.
