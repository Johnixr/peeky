/**
 * Peeky mascot — the transparent floating "探头偷看的魔法小精灵".
 *
 * Renders a self-contained inline-SVG character (a cute hooded peeking sprite
 * wearing a magic hat) animated purely with CSS per state. No real sprite
 * assets exist yet, so everything here is procedural / vector.
 *
 * Owns: src/mascot.ts + src/mascot.css (per FILE OWNERSHIP).
 *
 * Public API (per FRONTEND CONTRACT):
 *   initMascot(root): MascotController
 *   MascotController {
 *     setState(s), startUtterance(mode), appendToken(text),
 *     endUtterance(), showHasSomething(), positionBubble()
 *   }
 *
 * The OS window is transparent / decorationless / always-on-top (configured
 * natively by Foundation in tauri.conf.json). Here we only make the *character*
 * div a drag handle that moves the OS window via @tauri-apps/api/window when
 * available, with a graceful CSS-only fallback for plain-browser dev.
 */

import "./mascot.css";
import { t } from "./i18n/index";
import { renderMarkdown } from "./markdown";

/** Mascot animation states (must match MascotState string constants). */
export type MascotStateName =
  | "idle"
  | "scanning"
  | "thinking"
  | "talking"
  | "has-something"
  | "working"
  | "paused";

const STATES: MascotStateName[] = [
  "idle",
  "scanning",
  "thinking",
  "talking",
  "has-something",
  "working",
  "paused",
];

/** Mode -> accent class. Colors are defined in style.css design tokens. */
type ModeName = "roast" | "nerd" | "copilot";
function normalizeMode(mode: string): ModeName {
  const m = (mode || "").toLowerCase();
  if (m === "nerd" || m === "copilot") return m;
  return "roast";
}

/** Bubble placement relative to the mascot. */
type BubbleSide = "top" | "bottom" | "left" | "right";

/** Callbacks for the "ask about my screen" input box (wired by glue to IPC). */
export interface AskCallbacks {
  /** User pressed Enter: submit the (possibly empty) trimmed question. */
  onSubmit(question: string): void;
  /** User pressed Esc / dismissed: drop the parked screenshot. */
  onCancel(): void;
}

export interface MascotController {
  /** Switch the animation state machine. */
  setState(s: string): void;
  /** A new utterance is starting: clear the bubble, show it, set mode accent. */
  startUtterance(mode: string): void;
  /** Append a streamed token (typewriter effect). */
  appendToken(text: string): void;
  /** Finish the current utterance (keeps bubble visible until dismissed). */
  endUtterance(): void;
  /** Low-priority content suppressed by the restraint engine: sparkle + bounce. */
  showHasSomething(): void;
  /** Show an error/notice in the bubble (e.g. missing API key, request failed). */
  showError(text: string): void;
  /** Show a transient status line while a tool runs (copilot mode). */
  showStatus(text: string): void;
  /** Recompute and apply the freest-side bubble placement. */
  positionBubble(): void;
  /** Toggle bubble visibility (single-click). */
  toggleBubble(): void;
  /** Show the "ask about my screen" input box; calls back on submit/cancel. */
  showAsk(callbacks: AskCallbacks): void;
  /** Hide the ask input box without firing a callback. */
  hideAsk(): void;
  /** Current state, for callers that need to branch (e.g. pause UI). */
  getState(): MascotStateName;
  /** The character element — used by glue to attach hover/click handlers. */
  readonly characterEl: HTMLElement;
  /** The hover toolbar element — glue wires its buttons to commands. */
  readonly toolbarEl: HTMLElement;
}

/**
 * Best-effort import of the Tauri window API. In a plain browser (vite dev
 * without tauri) the import simply fails and we fall back to CSS-positioning.
 */
type TauriWindow = {
  getCurrentWindow: () => {
    startDragging: () => Promise<void>;
    outerPosition: () => Promise<{ x: number; y: number }>;
    setPosition: (p: unknown) => Promise<void>;
  };
};
let tauriWindowApi: TauriWindow | null = null;
async function loadTauriWindow(): Promise<void> {
  try {
    // Dynamic import so a missing module in browser dev doesn't crash the app.
    const mod = (await import("@tauri-apps/api/window")) as unknown as TauriWindow;
    if (mod && typeof mod.getCurrentWindow === "function") {
      tauriWindowApi = mod;
    }
  } catch {
    tauriWindowApi = null;
  }
}

/**
 * Inline SVG for the peeking magic sprite.
 *
 * Structure is deliberately broken into named groups so mascot.css can animate
 * individual parts per state:
 *   .peeky-hat   magic hat (tilts on talking, droops on paused)
 *   .peeky-body  hooded body (breathes on idle, bobs on talking)
 *   .peeky-eye   eyes (blink on idle, sparkle on has-something, closed on paused)
 *   .peeky-arm   sleeves/arms (roll up on working)
 *   .peeky-dots  thinking "…" cluster
 *   .peeky-zzz   sleeping z-z marks
 *   .peeky-peek-line  the screen edge it peeks over (scanning)
 */
function svgMarkup(): string {
  return `
<svg class="peeky-svg" viewBox="0 0 120 120" width="100%" height="100%"
     xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <defs>
    <radialGradient id="peeky-glow" cx="50%" cy="40%" r="60%">
      <stop offset="0%" stop-color="var(--peeky-mode-accent)" stop-opacity="0.55"/>
      <stop offset="100%" stop-color="var(--peeky-mode-accent)" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="peeky-hat-grad" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="var(--peeky-mode-accent)"/>
      <stop offset="100%" stop-color="var(--peeky-mode-accent-dark)"/>
    </linearGradient>
  </defs>

  <!-- soft aura that tints to the active mode color -->
  <circle class="peeky-aura" cx="60" cy="56" r="46" fill="url(#peeky-glow)"/>

  <!-- the screen edge the sprite peeks over (only visible while scanning) -->
  <rect class="peeky-peek-line" x="6" y="92" width="108" height="10" rx="3"/>

  <!-- BODY (hooded cloak) -->
  <g class="peeky-body">
    <!-- cloak -->
    <path class="peeky-cloak" d="M30 96
      C28 70 34 50 60 50
      C86 50 92 70 90 96
      C78 90 70 92 60 92
      C50 92 42 90 30 96 Z"/>

    <!-- arms / sleeves -->
    <g class="peeky-arm peeky-arm-l">
      <path d="M34 74 C24 78 22 88 28 94 C33 90 36 84 40 80 Z"/>
    </g>
    <g class="peeky-arm peeky-arm-r">
      <path d="M86 74 C96 78 98 88 92 94 C87 90 84 84 80 80 Z"/>
    </g>

    <!-- face disc -->
    <circle class="peeky-face" cx="60" cy="58" r="22"/>

    <!-- eyes -->
    <g class="peeky-eyes">
      <circle class="peeky-eye peeky-eye-l" cx="52" cy="58" r="4.4"/>
      <circle class="peeky-eye peeky-eye-r" cx="68" cy="58" r="4.4"/>
      <circle class="peeky-spark peeky-spark-l" cx="50.4" cy="56.4" r="1.3"/>
      <circle class="peeky-spark peeky-spark-r" cx="66.4" cy="56.4" r="1.3"/>
    </g>

    <!-- little blush + mouth -->
    <circle class="peeky-cheek peeky-cheek-l" cx="46" cy="66" r="3"/>
    <circle class="peeky-cheek peeky-cheek-r" cx="74" cy="66" r="3"/>
    <path class="peeky-mouth" d="M55 67 Q60 71 65 67"/>
  </g>

  <!-- MAGIC HAT (sits on the hood) -->
  <g class="peeky-hat">
    <path class="peeky-hat-cone" d="M40 40 L60 6 L80 40 Z" fill="url(#peeky-hat-grad)"/>
    <ellipse class="peeky-hat-brim" cx="60" cy="41" rx="26" ry="7"/>
    <circle class="peeky-hat-star" cx="60" cy="22" r="2.6"/>
    <circle class="peeky-hat-star peeky-hat-star-2" cx="66" cy="30" r="1.6"/>
  </g>

  <!-- thinking dots -->
  <g class="peeky-dots">
    <circle cx="86" cy="40" r="3"/>
    <circle cx="95" cy="36" r="3"/>
    <circle cx="104" cy="33" r="3"/>
  </g>

  <!-- sleeping z-z -->
  <g class="peeky-zzz">
    <text x="84" y="40" class="peeky-z peeky-z-1">z</text>
    <text x="94" y="30" class="peeky-z peeky-z-2">z</text>
    <text x="104" y="22" class="peeky-z peeky-z-3">z</text>
  </g>
</svg>`;
}

/**
 * Compute the freest side to open the bubble on, given the mascot's screen
 * position. Near left edge -> open right; near top -> open down; etc. The goal
 * is that the bubble never goes off-screen (PRD §6.2/§6.3).
 */
function chooseSide(rect: DOMRect, vw: number, vh: number): BubbleSide {
  const cx = rect.left + rect.width / 2;
  const cy = rect.top + rect.height / 2;
  const spaceLeft = rect.left;
  const spaceRight = vw - rect.right;
  const spaceTop = rect.top;
  const spaceBottom = vh - rect.bottom;

  // Build candidate list ordered by available space, but bias toward bottom/top
  // (a speech bubble reads most naturally below/above the speaker).
  const candidates: Array<{ side: BubbleSide; space: number; bias: number }> = [
    { side: "bottom", space: spaceBottom, bias: 1.15 },
    { side: "top", space: spaceTop, bias: 1.0 },
    { side: "right", space: spaceRight, bias: 0.9 },
    { side: "left", space: spaceLeft, bias: 0.9 },
  ];
  candidates.sort((a, b) => b.space * b.bias - a.space * a.bias);

  // If the mascot hugs a horizontal edge, force horizontal opening so the
  // bubble grows toward center (keeps it on-screen).
  if (cx < vw * 0.18) return "right";
  if (cx > vw * 0.82) return "left";
  if (cy < vh * 0.18) return "bottom";
  if (cy > vh * 0.82) return "top";

  return candidates[0].side;
}

export function initMascot(root: HTMLElement): MascotController {
  // ---- DOM scaffold ---------------------------------------------------------
  const container = document.createElement("div");
  container.className = "peeky-mascot peeky-mode-roast";
  container.dataset.state = "idle";

  // The draggable character (whole sprite area is the OS-window drag handle).
  const character = document.createElement("div");
  character.className = "peeky-character";
  character.innerHTML = svgMarkup();
  // Hint native drag-region behaviour for tauri builds that honor it.
  character.setAttribute("data-tauri-drag-region", "");

  // Hover toolbar (settings / tools entry). Hidden until you point at the
  // mascot — visibility is driven by JS below, NOT CSS :hover (which sticks
  // "on" in borderless overlay webviews and made the buttons look permanent).
  const gearSvg =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M21.5 12h-3M5.5 12h-3M18.7 5.3l-2.1 2.1M7.4 16.6l-2.1 2.1M18.7 18.7l-2.1-2.1M7.4 7.4 5.3 5.3"/></svg>';
  const pauseSvg =
    '<svg viewBox="0 0 24 24" fill="currentColor" stroke="none"><rect x="7" y="6" width="3.4" height="12" rx="1"/><rect x="13.6" y="6" width="3.4" height="12" rx="1"/></svg>';
  const toolbar = document.createElement("div");
  toolbar.className = "peeky-toolbar";
  toolbar.innerHTML = `
    <button class="peeky-tool-btn" data-action="settings" type="button" title="Settings" aria-label="Settings">${gearSvg}</button>
    <button class="peeky-tool-btn" data-action="pause" type="button" title="Pause" aria-label="Pause">${pauseSvg}</button>
  `;

  // Speech caption (no background box — outlined text, see mascot.css).
  const bubble = document.createElement("div");
  bubble.className = "peeky-bubble peeky-bubble-hidden";
  bubble.dataset.side = "bottom";
  const bubbleText = document.createElement("div");
  bubbleText.className = "peeky-bubble-text";
  bubble.appendChild(bubbleText);

  // "Ask about my screen" input box (Ctrl+Shift+B). Hidden until the backend
  // (which captured the screen on the shortcut) asks us to show it.
  const ask = document.createElement("div");
  ask.className = "peeky-ask peeky-ask-hidden";
  ask.dataset.side = "bottom";
  const askInput = document.createElement("textarea");
  askInput.className = "peeky-ask-input";
  askInput.rows = 1;
  askInput.setAttribute("spellcheck", "false");
  const askHint = document.createElement("div");
  askHint.className = "peeky-ask-hint";
  ask.appendChild(askInput);
  ask.appendChild(askHint);

  container.appendChild(bubble);
  container.appendChild(ask);
  container.appendChild(character);
  container.appendChild(toolbar);
  root.appendChild(container);

  // ---- state ----------------------------------------------------------------
  let state: MascotStateName = "idle";
  let bubbleVisible = false;
  let bubbleHasContent = false;
  // "armed" = an utterance started but no token has arrived yet. We delay showing
  // the bubble until the first real token so an empty/silent reply never leaves a
  // blank bubble on screen.
  let armed = false;
  // "expanded" = user clicked the caption to read the full message (vs the
  // default compact 2-line rolling view).
  let expanded = false;
  // "stickBottom" = auto-follow the newest streamed text. Turns OFF the moment
  // the user scrolls up to read, so the typewriter never yanks them back down;
  // turns back ON if they scroll to the bottom again.
  let stickBottom = true;
  // True while the pointer is over the caption (reading) — pins it open + lets
  // Esc target it. Declared here so the typewriter can check it on stream-end.
  let pointerOverBubble = false;
  function isHovered(): boolean {
    return pointerOverBubble;
  }

  function atBottom(): boolean {
    return bubbleText.scrollHeight - bubbleText.scrollTop - bubbleText.clientHeight < 6;
  }
  function scrollToBottom(): void {
    bubbleText.scrollTop = bubbleText.scrollHeight;
  }

  // Click-through coupling: tell the backend when interactive UI (bubble / ask /
  // hover toolbar) is open so it lets the WHOLE window catch mouse events.
  // Otherwise only the sprite area is clickable and the rest passes through to
  // the apps underneath (so Peeky stops stealing nearby clicks).
  let lastInteractive = false;
  async function setInteractive(on: boolean): Promise<void> {
    if (on === lastInteractive) return;
    lastInteractive = on;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("set_overlay_interactive", { on });
    } catch {
      /* browser dev / no Tauri — ignore */
    }
  }
  function updateInteractive(): void {
    void setInteractive(bubbleVisible || askVisible || toolbar.classList.contains("show"));
  }

  // Typewriter queue: tokens may arrive faster than we reveal them.
  let pending = "";
  let revealed = "";
  let typer: number | null = null;
  // "streamDone" = the backend signalled end-of-utterance. The typewriter keeps
  // running until it has revealed everything, THEN settles to idle — so the
  // typing animation always plays out even when the whole reply arrives at once
  // (fast / non-streaming models). endUtterance must NOT dump the full text.
  let streamDone = false;

  function stopTyper(): void {
    if (typer !== null) {
      window.clearInterval(typer);
      typer = null;
    }
  }

  function startTyper(): void {
    if (typer !== null) return;
    typer = window.setInterval(() => {
      if (revealed.length >= pending.length) {
        // Caught up. If the stream is finished, settle to idle. For a finished
        // answer, jump back to the TOP so the user reads it from the beginning
        // at their own pace (and the last line is never stuck half-off the
        // bottom). The auto-dismiss countdown is generous and pauses on hover.
        stopTyper();
        if (streamDone) {
          setState("idle");
          requestAnimationFrame(() => {
            if (!isHovered()) {
              bubbleText.scrollTop = 0;
              stickBottom = false;
            }
          });
          scheduleHide(14000);
        }
        return;
      }
      // Reveal a small chunk per tick for a smooth, visible typewriter feel.
      const step = Math.max(1, Math.round((pending.length - revealed.length) / 14));
      revealed = pending.slice(0, revealed.length + step);
      // Render the streamed text as (safe) Markdown. The renderer tolerates the
      // half-finished markup that partial streaming produces.
      bubbleText.innerHTML = renderMarkdown(revealed);
      // Follow the newest text ONLY while the user hasn't scrolled up to read.
      if (!expanded && stickBottom) scrollToBottom();
      positionBubble();
    }, 22);
  }

  // ---- bubble positioning ---------------------------------------------------
  function positionBubble(): void {
    if (!bubbleVisible) return;
    const rect = character.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const side = chooseSide(rect, vw, vh);
    bubble.dataset.side = side;
    // Clear any inline offsets; CSS handles base placement via [data-side].
    // We additionally clamp horizontally so long bubbles never clip the viewport.
    bubble.style.removeProperty("--peeky-bubble-shift");
    requestAnimationFrame(() => {
      const br = bubble.getBoundingClientRect();
      let shift = 0;
      if (br.left < 6) shift = 6 - br.left;
      else if (br.right > vw - 6) shift = vw - 6 - br.right;
      if (shift !== 0) {
        bubble.style.setProperty("--peeky-bubble-shift", `${shift}px`);
      }
    });
  }

  // Auto-dismiss: a finished caption fades on its own after a while so the
  // screen doesn't keep a stale line forever. A new utterance cancels it; an
  // active tool status or an expanded read keeps it pinned.
  let hideTimer: number | null = null;
  function clearHide(): void {
    if (hideTimer !== null) {
      window.clearTimeout(hideTimer);
      hideTimer = null;
    }
  }
  function scheduleHide(ms: number): void {
    clearHide();
    hideTimer = window.setTimeout(() => hideBubble(), ms);
  }

  function showBubble(): void {
    bubbleVisible = true;
    bubble.classList.remove("peeky-bubble-hidden");
    positionBubble();
    updateInteractive();
  }
  function hideBubble(): void {
    bubbleVisible = false;
    bubble.classList.add("peeky-bubble-hidden");
    // Once a card is dismissed it's done — clicking the sprite must NOT bring
    // the same reply back. Only a fresh utterance shows a new card.
    bubbleHasContent = false;
    expanded = false;
    bubble.classList.remove("expanded");
    clearHide();
    updateInteractive();
  }

  // ---- public API impl ------------------------------------------------------
  function setState(s: string): void {
    const next = (STATES as string[]).includes(s) ? (s as MascotStateName) : "idle";
    state = next;
    container.dataset.state = next;
    for (const st of STATES) container.classList.remove(`peeky-state-${st}`);
    container.classList.add(`peeky-state-${next}`);
    // has-something is a one-shot attention cue; restore idle after it plays.
    if (next === "has-something") {
      window.setTimeout(() => {
        if (state === "has-something") setState("idle");
      }, 2600);
    }
  }

  function startUtterance(mode: string): void {
    const m = normalizeMode(mode);
    for (const mm of ["roast", "nerd", "copilot"]) {
      container.classList.remove(`peeky-mode-${mm}`);
    }
    container.classList.add(`peeky-mode-${m}`);
    // Arm but don't reveal the bubble yet — wait for the first real token so a
    // silent/empty reply never leaves a blank bubble (this was the empty-bubble
    // bug). The existing bubble (if any) stays untouched until a token lands.
    stopTyper();
    clearHide();
    bubble.classList.remove("peeky-bubble-error", "peeky-bubble-status", "expanded");
    bubbleText.textContent = "";
    expanded = false;
    streamDone = false;
    stickBottom = true; // follow the newest text as it streams in
    pending = "";
    revealed = "";
    armed = true;
  }

  function appendToken(text: string): void {
    if (!text) return;
    if (armed) {
      // First token of a new utterance: now reveal a fresh, empty caption.
      armed = false;
      bubble.classList.remove("peeky-bubble-status");
      bubbleText.textContent = "";
      bubbleHasContent = true;
      showBubble();
      setState("talking");
    } else if (!bubbleVisible) {
      showBubble();
    }
    pending += text;
    startTyper();
  }

  function endUtterance(): void {
    if (armed) {
      // Stream ended without a single token — never show a blank bubble.
      armed = false;
      setState("idle");
      return;
    }
    // Don't dump the full text — just mark the stream finished and let the
    // typewriter reveal the remainder, then settle to idle on catch-up. This is
    // what keeps the typing animation visible even for a one-shot reply.
    streamDone = true;
    startTyper();
  }

  function showHasSomething(): void {
    setState("has-something");
  }

  // Transient status while the agent runs a tool (scrolling, typing, …) — the
  // "I'm doing, not talking" cue for copilot mode. One short line.
  function showStatus(text: string): void {
    if (!text) return;
    stopTyper();
    clearHide(); // a tool is running; keep the status pinned until it changes
    armed = false;
    expanded = false;
    bubble.classList.remove("peeky-bubble-error", "expanded");
    bubble.classList.add("peeky-bubble-status");
    pending = text;
    revealed = text;
    bubbleText.textContent = text;
    bubbleHasContent = true;
    showBubble();
    setState("working");
  }

  function showError(text: string): void {
    if (!text) return;
    stopTyper();
    armed = false;
    expanded = false;
    bubble.classList.remove("peeky-bubble-status", "expanded");
    pending = text;
    revealed = text;
    bubbleText.textContent = text;
    bubble.classList.add("peeky-bubble-error");
    bubbleHasContent = true;
    showBubble();
    setState("idle");
    scheduleHide(10000);
  }

  // Toolbar visibility — explicit, so it can't get stuck visible. Show while the
  // pointer is over the mascot or the toolbar; hide shortly after it leaves.
  let toolbarTimer: number | null = null;
  function showToolbar(): void {
    if (toolbarTimer !== null) {
      window.clearTimeout(toolbarTimer);
      toolbarTimer = null;
    }
    toolbar.classList.add("show");
    updateInteractive();
  }
  function hideToolbar(): void {
    toolbar.classList.remove("show");
    updateInteractive();
  }
  function hideToolbarSoon(): void {
    if (toolbarTimer !== null) window.clearTimeout(toolbarTimer);
    toolbarTimer = window.setTimeout(hideToolbar, 400);
  }
  character.addEventListener("pointerenter", showToolbar);
  character.addEventListener("pointerleave", hideToolbarSoon);
  toolbar.addEventListener("pointerenter", showToolbar);
  toolbar.addEventListener("pointerleave", hideToolbarSoon);
  // Safety nets: never leave it pinned if the pointer/focus leaves the window.
  window.addEventListener("blur", hideToolbar);
  document.addEventListener("pointerleave", hideToolbar);

  // Click the caption to expand to the full message / collapse back to compact.
  bubble.addEventListener("click", (e) => {
    e.stopPropagation();
    if (!bubbleHasContent) return;
    expanded = !expanded;
    bubble.classList.toggle("expanded", expanded);
    bubbleText.scrollTop = expanded ? 0 : bubbleText.scrollHeight;
    // Reading: keep it pinned while expanded; resume countdown when collapsed.
    if (expanded) clearHide();
    else scheduleHide(8000);
    positionBubble();
  });

  // Make the panel focusable so it can receive Esc while the pointer is on it.
  bubble.tabIndex = -1;

  // Pin the caption while the pointer is over it (reading/scrolling) so it can
  // never vanish mid-read; once the pointer leaves, restart a generous countdown.
  bubble.addEventListener("pointerenter", () => {
    pointerOverBubble = true;
    clearHide();
    // Best-effort focus so an Esc keypress lands on us (needs window key focus).
    try {
      bubble.focus({ preventScroll: true });
    } catch {
      /* ignore */
    }
  });
  bubble.addEventListener("pointerleave", () => {
    pointerOverBubble = false;
    if (!expanded) scheduleHide(6000);
  });
  // Track the user's scroll intent: scrolling up stops the auto-follow so the
  // typewriter can't yank them back to the bottom; scrolling to the bottom
  // resumes it. Either way it's active reading, so keep the caption open.
  bubbleText.addEventListener("scroll", () => {
    stickBottom = atBottom();
    clearHide();
  });

  // Esc dismisses the panel when the pointer is over it (or it has focus). This
  // is scoped (not a global Esc hijack) so it only fires when the user is
  // actually pointing at / focused on the bubble.
  window.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    if (!bubbleVisible) return;
    if (pointerOverBubble || document.activeElement === bubble) {
      e.preventDefault();
      hideBubble();
    }
  });

  function toggleBubble(): void {
    if (bubbleVisible) {
      hideBubble();
    } else if (bubbleHasContent) {
      showBubble();
    }
  }

  // ---- "ask about my screen" input box --------------------------------------
  let askVisible = false;
  let askCallbacks: AskCallbacks | null = null;

  function positionAsk(): void {
    if (!askVisible) return;
    const rect = character.getBoundingClientRect();
    ask.dataset.side = chooseSide(rect, window.innerWidth, window.innerHeight);
  }

  function showAsk(callbacks: AskCallbacks): void {
    askCallbacks = callbacks;
    // A fresh prompt: clear any prior text, hide the bubble so they don't stack.
    hideBubble();
    askInput.value = "";
    askInput.style.height = "auto";
    askInput.placeholder = t("ask.placeholder");
    askHint.textContent = t("ask.hint");
    askVisible = true;
    ask.classList.remove("peeky-ask-hidden");
    positionAsk();
    updateInteractive();
    // Focus on the next frame so the element is laid out + visible first.
    requestAnimationFrame(() => askInput.focus());
  }

  function hideAsk(): void {
    askVisible = false;
    askCallbacks = null;
    ask.classList.add("peeky-ask-hidden");
    updateInteractive();
  }

  // Auto-grow the textarea (1 → a few lines) as the question is typed.
  askInput.addEventListener("input", () => {
    askInput.style.height = "auto";
    askInput.style.height = `${Math.min(askInput.scrollHeight, 110)}px`;
    positionAsk();
  });

  // Enter submits; Shift+Enter inserts a newline; Esc cancels.
  askInput.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const q = askInput.value.trim();
      const cb = askCallbacks;
      hideAsk();
      cb?.onSubmit(q);
    } else if (e.key === "Escape") {
      e.preventDefault();
      const cb = askCallbacks;
      hideAsk();
      cb?.onCancel();
    }
    // Shift+Enter: let the textarea insert a newline (default behavior).
  });

  // ---- dragging: move the OS window via Tauri, CSS fallback otherwise -------
  let dragging = false;
  let startPointer = { x: 0, y: 0 };
  let startBox = { x: 0, y: 0 };
  let movedDuringDrag = false;

  character.addEventListener("pointerdown", (e: PointerEvent) => {
    if (e.button !== 0) return;
    movedDuringDrag = false;
    // Prefer native window drag when Tauri is present — smoothest + multi-monitor safe.
    if (tauriWindowApi) {
      try {
        // startDragging consumes the gesture at the OS level.
        void tauriWindowApi.getCurrentWindow().startDragging();
        return;
      } catch {
        /* fall through to CSS drag */
      }
    }
    // CSS fallback (browser dev): reposition the container within the viewport.
    dragging = true;
    startPointer = { x: e.clientX, y: e.clientY };
    const cs = getComputedStyle(container);
    startBox = {
      x: parseFloat(cs.left) || container.offsetLeft || 0,
      y: parseFloat(cs.top) || container.offsetTop || 0,
    };
    character.setPointerCapture(e.pointerId);
  });

  character.addEventListener("pointermove", (e: PointerEvent) => {
    if (!dragging) return;
    const dx = e.clientX - startPointer.x;
    const dy = e.clientY - startPointer.y;
    if (Math.abs(dx) > 3 || Math.abs(dy) > 3) movedDuringDrag = true;
    container.style.left = `${startBox.x + dx}px`;
    container.style.top = `${startBox.y + dy}px`;
    container.style.right = "auto";
    container.style.bottom = "auto";
    positionBubble();
  });

  function endDrag(e: PointerEvent): void {
    if (!dragging) return;
    dragging = false;
    try {
      character.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    positionBubble();
  }
  character.addEventListener("pointerup", endDrag);
  character.addEventListener("pointercancel", endDrag);

  // Suppress the click that follows a real drag (avoid accidental bubble toggle).
  character.addEventListener(
    "click",
    (e) => {
      if (movedDuringDrag) {
        e.stopImmediatePropagation();
        movedDuringDrag = false;
      }
    },
    true
  );

  // Reposition bubble + ask box on viewport resize (monitor change / rotation).
  window.addEventListener("resize", () => {
    positionBubble();
    positionAsk();
  });

  // Kick off async Tauri window detection (non-blocking).
  void loadTauriWindow();

  // Initialize state classes.
  setState("idle");

  const controller: MascotController = {
    setState,
    startUtterance,
    appendToken,
    endUtterance,
    showHasSomething,
    showError,
    showStatus,
    positionBubble,
    toggleBubble,
    showAsk,
    hideAsk,
    getState: () => state,
    characterEl: character,
    toolbarEl: toolbar,
  };
  return controller;
}
