/**
 * Peeky reminder overlay window — a full-screen transparent window that shows
 * break-reminder images floating across the entire screen.
 *
 * Uses a PULL model (like the capture window): the backend parks the image and
 * pings `peeky://show-reminder`; we fetch the parked image via the
 * `get_reminder_image` command. We also pull on focus and first mount so a ping
 * that races ahead of this listener is never lost.
 */

function inTauri(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as Record<string, unknown>;
  return (
    typeof w.__TAURI_INTERNALS__ !== "undefined" ||
    typeof w.__TAURI__ !== "undefined"
  );
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  if (!inTauri()) return undefined;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.warn(`[peeky-reminder] invoke "${cmd}" failed:`, err);
    return undefined;
  }
}

let activeImages = 0;
// Guards against two pulls (e.g. event + focus) racing to grab the same image.
let pulling = false;

async function setClickThrough(on: boolean): Promise<void> {
  if (!inTauri()) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setIgnoreCursorEvents(on);
  } catch (err) {
    console.warn("[peeky-reminder] setIgnoreCursorEvents failed:", err);
  }
}

async function hideWindow(): Promise<void> {
  if (!inTauri()) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().hide();
  } catch (err) {
    console.warn("[peeky-reminder] hide failed:", err);
  }
}

function showReminder(dataUrl: string): void {
  activeImages++;

  const img = document.createElement("img");
  img.src = dataUrl;
  img.draggable = false;
  img.className = "reminder-img";
  img.style.animation = "peeky-reminder-float 18s linear forwards";

  const cleanup = () => {
    if (!img.isConnected) return;
    img.remove();
    activeImages = Math.max(0, activeImages - 1);
    // When the last image is gone, go click-through and hide the window.
    if (activeImages === 0) {
      void setClickThrough(true).then(hideWindow);
    }
  };

  img.addEventListener("click", (e) => {
    e.stopPropagation();
    img.style.animationPlayState = "paused";
    cleanup();
  });
  img.addEventListener("animationend", cleanup);
  img.addEventListener("error", (e) => {
    console.error("[peeky-reminder] image failed to load:", e);
    cleanup();
  });

  document.body.appendChild(img);
}

/** Pull the parked image from the backend and display it (no-op if none). */
async function pullAndShow(): Promise<void> {
  if (pulling) return;
  pulling = true;
  try {
    const dataUrl = await invoke<string | null>("get_reminder_image");
    if (dataUrl) {
      await setClickThrough(false);
      showReminder(dataUrl);
    }
  } finally {
    pulling = false;
  }
}

async function boot(): Promise<void> {
  // Inject the keyframe once.
  if (!document.getElementById("reminder-anim-style")) {
    const style = document.createElement("style");
    style.id = "reminder-anim-style";
    style.textContent = `
      @keyframes peeky-reminder-float {
        from { transform: translateX(-110%); }
        to   { transform: translateX(calc(100vw + 110%)); }
      }
    `;
    document.head.appendChild(style);
  }

  if (inTauri()) {
    try {
      const { getCurrentWindow, currentMonitor, LogicalSize, LogicalPosition } =
        await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      await win.setIgnoreCursorEvents(true);

      // Size to the full monitor so images can traverse the whole screen.
      try {
        const monitor = await currentMonitor();
        if (monitor) {
          const scale = monitor.scaleFactor;
          const width = monitor.size.width / scale;
          const height = monitor.size.height / scale;
          await win.setSize(new LogicalSize(width, height));
          await win.setPosition(new LogicalPosition(0, 0));
        }
      } catch (e) {
        console.warn("[peeky-reminder] resize failed:", e);
      }

      // Pull on the backend's "ready" ping...
      const { listen } = await import("@tauri-apps/api/event");
      await listen("peeky://show-reminder", () => void pullAndShow());
      // ...and on focus / first mount, covering a ping that arrived before this
      // listener was live (same belt-and-braces pattern as the capture window).
      window.addEventListener("focus", () => void pullAndShow());
    } catch (e) {
      console.error("[peeky-reminder] init failed:", e);
    }
  }

  // First-mount pull (in case the image was parked before we subscribed).
  await pullAndShow();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void boot());
} else {
  void boot();
}
