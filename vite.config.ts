import { defineConfig } from "vite";

// Vite config for the Peeky webview frontend.
// The Tauri Rust process serves this at devUrl http://localhost:1420 in dev,
// and loads the built ../dist directory in production.
export default defineConfig(async () => ({
  // Prevent Vite from clobbering Rust compiler output in the terminal.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    // Tauri watches the Rust side; ignore src-tauri to avoid double reloads.
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  // Env vars prefixed with these are exposed to the client.
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Target Safari/WebKit used by the macOS WKWebView.
    target: "safari14",
    minify: process.env.TAURI_ENV_DEBUG ? false : "esbuild",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // HTML entry points:
    // - main: the mascot overlay (transparent, small, always-on-top)
    // - settings: opaque settings window
    // - capture: freeze-frame region selector
    // - reminder: full-screen transparent overlay for floating reminder images
    rollupOptions: {
      input: {
        main: "index.html",
        settings: "settings.html",
        capture: "capture.html",
        reminder: "reminder.html",
      },
    },
  },
}));
