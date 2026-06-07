/*
 * Peeky settings panel (PRD §8.2 + §6.4).
 *
 * mountSettings(root) builds the full panel DOM, wires it to the backend via
 *   get_config / set_config / test_api_connection / get_token_stats /
 *   pause_toggle, and returns { open(), close() } per the frontend CONTRACT.
 *
 * Every label/hint/button goes through t() so the panel is fully localized;
 * it re-renders on language change. The panel is a scrollable card overlay
 * that fits the small companion window.
 *
 * Config shape mirrors src-tauri/src/types.rs::Config exactly — do not rename
 * fields without updating the Rust side.
 */

import { invoke } from "@tauri-apps/api/core";
import { t, getLang, setLanguage, onLanguageChange, type LangSetting } from "./i18n/index";

type Quality = "low" | "med" | "high";
type ModeKind = "roast" | "nerd" | "copilot";
type PermissionMode = "yolo" | "auto" | "cautious";
type Language = "auto" | "en" | "zh" | "ja";
type ReasoningEffort = "off" | "low" | "medium" | "high";

interface QuietHours {
  enabled: boolean;
  start: string;
  end: string;
}

interface Config {
  api_base_url: string;
  api_key: string;
  model: string;
  max_tokens: number;
  temperature: number;
  reasoning_effort: ReasoningEffort;
  mode: ModeKind;
  permission_mode: PermissionMode;
  language: Language;
  sensitivity: Quality;
  speech_budget_per_hour: number;
  screenshot_quality: Quality;
  quiet_hours: QuietHours;
  follow_system_dnd: boolean;
  show_token_stats: boolean;
}

interface TokenStats {
  calls: number;
  prompt_tokens: number;
  completion_tokens: number;
  silent: number;
}

interface HistoryItem {
  ts: number;
  mode: string;
  text: string;
  app: string | null;
}

/** Sensible client-side fallback mirroring Config::default() in types.rs. */
function defaultConfig(): Config {
  return {
    api_base_url: "https://api.stepfun.com/v1",
    api_key: "",
    model: "step-3.7-flash",
    max_tokens: 300,
    temperature: 0.7,
    reasoning_effort: "low",
    mode: "roast",
    permission_mode: "auto",
    language: "auto",
    sensitivity: "med",
    speech_budget_per_hour: 6,
    screenshot_quality: "med",
    quiet_hours: { enabled: false, start: "22:00", end: "09:00" },
    follow_system_dnd: true,
    show_token_stats: true,
  };
}

// ---- small DOM helpers ----------------------------------------------------

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function option(value: string, label: string, selected: boolean): HTMLOptionElement {
  const o = document.createElement("option");
  o.value = value;
  o.textContent = label;
  o.selected = selected;
  return o;
}

/**
 * Inline SVG sidebar icons (no emoji — they render inconsistently and look out
 * of place). 24×24, stroke = currentColor so they pick up the active/idle text
 * color automatically. Sizing is handled in settings.css (.peeky-tab-icon svg).
 */
const SVG = (body: string): string =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

const ICONS: Record<string, string> = {
  // chat bubble with three dots — "what Peeky says"
  mode: SVG('<path d="M20 11.5a7.5 7.5 0 0 1-10.9 6.7L4 19.5l1.3-4.2A7.5 7.5 0 1 1 20 11.5z"/><circle cx="8.5" cy="11.5" r=".6" fill="currentColor" stroke="none"/><circle cx="12" cy="11.5" r=".6" fill="currentColor" stroke="none"/><circle cx="15.5" cy="11.5" r=".6" fill="currentColor" stroke="none"/>'),
  // chip — the model / API
  model: SVG('<rect x="7" y="7" width="10" height="10" rx="1.5"/><path d="M10 3v3M14 3v3M10 18v3M14 18v3M3 10h3M3 14h3M18 10h3M18 14h3"/>'),
  // sliders — behavior tuning
  behavior: SVG('<path d="M4 7h16M4 12h16M4 17h16"/><circle cx="9" cy="7" r="2"/><circle cx="15" cy="12" r="2"/><circle cx="7" cy="17" r="2"/>'),
  // moon — quiet hours / focus
  quiet: SVG('<path d="M20 14.3A8 8 0 0 1 9.7 4 7 7 0 1 0 20 14.3z"/>'),
  // sparkle — the mascot
  mascot: SVG('<path d="M12 3l1.7 5.1L19 10l-5.3 1.9L12 17l-1.7-5.1L5 10l5.3-1.9L12 3z"/>'),
  // bars — token usage
  usage: SVG('<path d="M4 20h16"/><path d="M7 20v-6M12 20V6M17 20v-9"/>'),
  // clock — review history
  history: SVG('<circle cx="12" cy="12" r="8.5"/><path d="M12 7.6V12l3 1.8"/>'),
  // gear — general
  general: SVG('<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M21.5 12h-3M5.5 12h-3M18.7 5.3l-2.1 2.1M7.4 16.6l-2.1 2.1M18.7 18.7l-2.1-2.1M7.4 7.4 5.3 5.3"/>'),
};

export interface SettingsController {
  open(): void;
  close(): void;
}

export interface MountOptions {
  /**
   * Render as a full-window opaque page (the dedicated settings OS window)
   * instead of a dimmed modal overlay. In standalone mode `close()` hides the
   * OS window rather than just toggling a div.
   */
  standalone?: boolean;
}

export function mountSettings(root: HTMLElement, opts: MountOptions = {}): SettingsController {
  const standalone = opts.standalone ?? false;
  let config: Config = defaultConfig();
  let visible = false;
  let paused = false;
  // Which category tab is showing; persists across re-renders (language change).
  let activeTab = "model";

  // Shell --------------------------------------------------------------------
  // Standalone: the surface IS the window page (opaque, always shown).
  // Overlay: a dimmed modal that floats inside the (transparent) mascot window.
  const overlay = el("div", standalone ? "peeky-settings-page" : "peeky-settings-overlay");
  overlay.setAttribute("role", "dialog");
  overlay.setAttribute("aria-modal", "true");
  overlay.hidden = !standalone;

  const card = el("div", "peeky-settings-card");
  overlay.appendChild(card);
  root.appendChild(overlay);

  // Only the overlay has a dismissable backdrop.
  if (!standalone) {
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });
  }

  // Field references we need to read on save / update on render. ------------
  let modeSelect!: HTMLSelectElement;
  let baseUrlInput!: HTMLInputElement;
  let apiKeyInput!: HTMLInputElement;
  let modelInput!: HTMLInputElement;
  let maxTokensInput!: HTMLInputElement;
  let temperatureInput!: HTMLInputElement;
  let reasoningSelect!: HTMLSelectElement;
  let sensitivitySelect!: HTMLSelectElement;
  let budgetInput!: HTMLInputElement;
  let quietEnableInput!: HTMLInputElement;
  let quietStartInput!: HTMLInputElement;
  let quietEndInput!: HTMLInputElement;
  let followDndInput!: HTMLInputElement;
  let screenshotQualitySelect!: HTMLSelectElement;
  let permissionSelect!: HTMLSelectElement;
  let languageSelect!: HTMLSelectElement;
  let showStatsInput!: HTMLInputElement;

  // Render the panel: a macOS-System-Settings-style layout — a left sidebar of
  // category tabs + a content pane showing one category at a time (so settings
  // are grouped instead of one endless scroll), with a sticky Save footer.
  // Called on open and on language change so all text stays localized.
  function render(): void {
    card.replaceChildren();

    const layout = el("div", "peeky-settings-layout");
    const sidebar = el("nav", "peeky-settings-sidebar");
    sidebar.setAttribute("role", "tablist");
    sidebar.setAttribute("aria-label", t("settings.title"));
    const content = el("div", "peeky-settings-content");
    layout.appendChild(sidebar);
    layout.appendChild(content);
    card.appendChild(layout);

    const panels: Record<string, HTMLElement> = {};

    const tabs: Array<{ id: string; icon: string; label: string; build: (root: HTMLElement) => void }> = [
      { id: "mode", icon: ICONS.mode, label: t("settings.tab.mode"), build: buildModeTab },
      { id: "model", icon: ICONS.model, label: t("settings.tab.model"), build: buildModelTab },
      { id: "behavior", icon: ICONS.behavior, label: t("settings.tab.behavior"), build: buildBehaviorTab },
      { id: "quiet", icon: ICONS.quiet, label: t("settings.tab.quiet"), build: buildQuietTab },
      { id: "mascot", icon: ICONS.mascot, label: t("settings.tab.mascot"), build: buildMascotTab },
      { id: "usage", icon: ICONS.usage, label: t("settings.tab.usage"), build: buildUsageTab },
      { id: "history", icon: ICONS.history, label: t("settings.tab.history"), build: buildHistoryTab },
      { id: "general", icon: ICONS.general, label: t("settings.tab.general"), build: buildGeneralTab },
    ];

    function setActiveTab(id: string): void {
      activeTab = id;
      sidebar.querySelectorAll<HTMLElement>(".peeky-tab-btn").forEach((b) => {
        const on = b.dataset.tab === id;
        b.classList.toggle("active", on);
        b.setAttribute("aria-selected", on ? "true" : "false");
      });
      for (const pid of Object.keys(panels)) panels[pid].hidden = pid !== id;
      content.scrollTop = 0;
    }

    // Build EVERY panel up front (not lazily) so all field references exist and
    // edits survive switching tabs; only the active one is visible.
    for (const tab of tabs) {
      const btn = el("button", "peeky-tab-btn");
      btn.type = "button";
      btn.dataset.tab = tab.id;
      btn.setAttribute("role", "tab");
      const iconSpan = el("span", "peeky-tab-icon");
      iconSpan.innerHTML = tab.icon; // trusted static SVG markup
      btn.appendChild(iconSpan);
      btn.appendChild(el("span", "peeky-tab-label", tab.label));
      btn.addEventListener("click", () => setActiveTab(tab.id));
      sidebar.appendChild(btn);

      const panel = el("section", "peeky-tab-panel");
      panel.dataset.tab = tab.id;
      panel.appendChild(el("h1", "peeky-tab-title", tab.label));
      tab.build(panel);
      panels[tab.id] = panel;
      content.appendChild(panel);
    }

    // Footer: Save (always visible, full width across sidebar + content).
    const footer = el("div", "peeky-settings-footer");
    const saveBtn = el("button", "peeky-btn peeky-btn-primary", t("settings.save"));
    saveBtn.type = "button";
    const saveResult = el("span", "peeky-save-result");
    saveBtn.addEventListener("click", async () => {
      if (!validate(saveResult)) {
        setActiveTab("model"); // required fields (Base URL/Key/model) live there
        return;
      }
      readInto(config);
      saveBtn.disabled = true;
      saveBtn.textContent = t("settings.saving");
      try {
        await invoke("set_config", { config });
        saveResult.className = "peeky-save-result ok";
        saveResult.textContent = t("settings.saved");
      } catch (err) {
        saveResult.className = "peeky-save-result err";
        saveResult.textContent = formatErr(err);
      } finally {
        saveBtn.disabled = false;
        saveBtn.textContent = t("settings.save");
      }
    });
    footer.appendChild(saveResult);
    footer.appendChild(saveBtn);
    card.appendChild(footer);

    setActiveTab(panels[activeTab] ? activeTab : "model");
  }

  // ---- per-tab builders (assign the shared field refs) --------------------

  function buildModeTab(root: HTMLElement): void {
    const sec = section(root);
    modeSelect = selectField(sec, {
      label: t("settings.mode.label"),
      hint: t("settings.mode.hint"),
      value: config.mode,
      options: [
        { value: "roast", label: `${t("mode.roast")} — ${t("mode.roast.desc")}` },
        { value: "nerd", label: `${t("mode.nerd")} — ${t("mode.nerd.desc")}` },
        { value: "copilot", label: `${t("mode.copilot")} — ${t("mode.copilot.desc")}` },
      ],
    });
    // Apply immediately (and persist) so switching mode takes effect without a
    // full Save — matches the Ctrl+Shift+M shortcut behavior.
    modeSelect.addEventListener("change", () => {
      const m = modeSelect.value as ModeKind;
      config.mode = m;
      void invoke("set_mode", { mode: m });
    });
  }

  function buildModelTab(root: HTMLElement): void {
    const sec = section(root);

    baseUrlInput = textField(sec, {
      label: t("settings.baseUrl.label"),
      hint: t("settings.baseUrl.hint"),
      placeholder: t("settings.baseUrl.placeholder"),
      value: config.api_base_url,
      required: true,
    });

    // API key with show/hide toggle.
    const keyRow = fieldRow(sec, t("settings.apiKey.label"), t("settings.apiKey.hint"), true);
    const keyWrap = el("div", "peeky-input-wrap");
    apiKeyInput = el("input", "peeky-input");
    apiKeyInput.type = "password";
    apiKeyInput.placeholder = t("settings.apiKey.placeholder");
    apiKeyInput.value = config.api_key;
    apiKeyInput.autocomplete = "off";
    apiKeyInput.spellcheck = false;
    const reveal = el("button", "peeky-reveal-btn");
    reveal.type = "button";
    reveal.title = t("settings.apiKey.show");
    reveal.textContent = "👁";
    reveal.addEventListener("click", () => {
      const show = apiKeyInput.type === "password";
      apiKeyInput.type = show ? "text" : "password";
      reveal.title = show ? t("settings.apiKey.hide") : t("settings.apiKey.show");
    });
    keyWrap.appendChild(apiKeyInput);
    keyWrap.appendChild(reveal);
    keyRow.appendChild(keyWrap);

    modelInput = textField(sec, {
      label: t("settings.model.label"),
      hint: t("settings.model.hint"),
      placeholder: t("settings.model.placeholder"),
      value: config.model,
      required: true,
    });

    const grid = el("div", "peeky-grid-2");
    sec.appendChild(grid);
    maxTokensInput = numField(grid, {
      label: t("settings.maxTokens.label"),
      hint: t("settings.maxTokens.hint"),
      value: String(config.max_tokens),
      min: 1,
      max: 4096,
      step: 1,
    });
    temperatureInput = numField(grid, {
      label: t("settings.temperature.label"),
      hint: t("settings.temperature.hint"),
      value: String(config.temperature),
      min: 0,
      max: 2,
      step: 0.1,
    });

    // Reasoning / thinking effort — trades speed for quality.
    reasoningSelect = selectField(sec, {
      label: t("settings.reasoning.label"),
      hint: t("settings.reasoning.hint"),
      value: config.reasoning_effort ?? "low",
      options: [
        { value: "off", label: t("reasoning.off") },
        { value: "low", label: t("reasoning.low") },
        { value: "medium", label: t("reasoning.medium") },
        { value: "high", label: t("reasoning.high") },
      ],
    });

    // Test connection.
    const testRow = el("div", "peeky-test-row");
    const testBtn = el("button", "peeky-btn peeky-btn-secondary", t("settings.test.button"));
    testBtn.type = "button";
    const testResult = el("span", "peeky-test-result");
    testBtn.addEventListener("click", async () => {
      readInto(config); // test what the user typed
      testBtn.disabled = true;
      testResult.className = "peeky-test-result";
      testResult.textContent = t("settings.test.testing");
      try {
        await invoke("set_config", { config });
        const msg = await invoke<string>("test_api_connection");
        testResult.classList.add("ok");
        testResult.textContent = `${t("settings.test.success")}${msg ? ` — ${msg}` : ""}`;
      } catch (err) {
        testResult.classList.add("err");
        testResult.textContent = `${t("settings.test.failed")}: ${formatErr(err)}`;
      } finally {
        testBtn.disabled = false;
      }
    });
    testRow.appendChild(testBtn);
    testRow.appendChild(testResult);
    sec.appendChild(testRow);
  }

  function buildBehaviorTab(root: HTMLElement): void {
    const sec = section(root);
    sensitivitySelect = selectField(sec, {
      label: t("settings.sensitivity.label"),
      hint: t("settings.sensitivity.hint"),
      value: config.sensitivity,
      options: qualityOptions(),
    });
    budgetInput = numField(sec, {
      label: t("settings.budget.label"),
      hint: t("settings.budget.hint"),
      value: String(config.speech_budget_per_hour),
      min: 0,
      max: 120,
      step: 1,
    });
    screenshotQualitySelect = selectField(sec, {
      label: t("settings.screenshotQuality.label"),
      hint: t("settings.screenshotQuality.hint"),
      value: config.screenshot_quality,
      options: qualityOptions(),
    });
    permissionSelect = selectField(sec, {
      label: t("settings.permission.label"),
      hint: t("settings.permission.hint"),
      value: config.permission_mode,
      options: [
        { value: "yolo", label: `${t("permission.yolo")} — ${t("permission.yolo.desc")}` },
        { value: "auto", label: `${t("permission.auto")} — ${t("permission.auto.desc")}` },
        { value: "cautious", label: `${t("permission.cautious")} — ${t("permission.cautious.desc")}` },
      ],
    });
  }

  function buildQuietTab(root: HTMLElement): void {
    const sec = section(root);
    quietEnableInput = toggleField(sec, {
      label: t("settings.quietHours.enable"),
      hint: t("settings.quietHours.hint"),
      checked: config.quiet_hours.enabled,
    });
    const grid = el("div", "peeky-grid-2");
    sec.appendChild(grid);
    quietStartInput = timeField(grid, t("settings.quietHours.start"), config.quiet_hours.start);
    quietEndInput = timeField(grid, t("settings.quietHours.end"), config.quiet_hours.end);
    followDndInput = toggleField(sec, {
      label: t("settings.followDnd.label"),
      hint: t("settings.followDnd.hint"),
      checked: config.follow_system_dnd,
    });
  }

  function buildMascotTab(root: HTMLElement): void {
    const sec = section(root);
    sec.appendChild(el("h3", "peeky-mascot-heading", t("settings.mascot.heading")));
    sec.appendChild(el("p", "peeky-hint", t("settings.mascot.intro")));
    const ta = el("textarea", "peeky-mascot-template");
    ta.value = t("settings.mascot.template");
    ta.readOnly = true;
    ta.rows = 12;
    ta.spellcheck = false;
    sec.appendChild(ta);
    const copyBtn = el("button", "peeky-btn peeky-btn-secondary", t("settings.mascot.copy"));
    copyBtn.type = "button";
    copyBtn.addEventListener("click", async () => {
      const ok = await copyText(ta.value, ta);
      if (ok) {
        const orig = t("settings.mascot.copy");
        copyBtn.textContent = t("settings.mascot.copied");
        copyBtn.classList.add("ok");
        window.setTimeout(() => {
          copyBtn.textContent = orig;
          copyBtn.classList.remove("ok");
        }, 1500);
      }
    });
    sec.appendChild(copyBtn);
  }

  function buildUsageTab(root: HTMLElement): void {
    const sec = section(root);
    showStatsInput = toggleField(sec, {
      label: t("settings.showStats.label"),
      hint: t("settings.showStats.hint"),
      checked: config.show_token_stats,
    });
    const statsBox = el("div", "peeky-stats");
    sec.appendChild(statsBox);
    const refreshStats = async () => {
      statsBox.replaceChildren();
      if (!showStatsInput.checked) {
        statsBox.appendChild(el("p", "peeky-hint", t("settings.stats.hidden")));
        return;
      }
      let stats: TokenStats = { calls: 0, prompt_tokens: 0, completion_tokens: 0, silent: 0 };
      try {
        stats = await invoke<TokenStats>("get_token_stats");
      } catch {
        // leave zeros
      }
      statsBox.appendChild(statLine(t("settings.stats.calls"), stats.calls));
      statsBox.appendChild(statLine(t("settings.stats.prompt"), stats.prompt_tokens));
      statsBox.appendChild(statLine(t("settings.stats.completion"), stats.completion_tokens));
      statsBox.appendChild(statLine(t("settings.stats.silent"), stats.silent));
      const refresh = el("button", "peeky-btn peeky-btn-ghost", t("settings.stats.refresh"));
      refresh.type = "button";
      refresh.addEventListener("click", refreshStats);
      statsBox.appendChild(refresh);
    };
    showStatsInput.addEventListener("change", refreshStats);
    void refreshStats();
  }

  function buildHistoryTab(root: HTMLElement): void {
    const sec = section(root);
    const toolbar = el("div", "peeky-history-toolbar");
    const clearBtn = el("button", "peeky-btn peeky-btn-ghost", t("settings.history.clear"));
    clearBtn.type = "button";
    toolbar.appendChild(clearBtn);
    sec.appendChild(toolbar);
    const list = el("div", "peeky-history-list");
    sec.appendChild(list);

    const locale = (): string | undefined => {
      const lg = getLang();
      return lg === "zh" ? "zh-CN" : lg === "ja" ? "ja" : lg === "en" ? "en" : undefined;
    };
    const fmtTime = (ts: number): string => {
      try {
        return new Date(ts * 1000).toLocaleString(locale(), {
          month: "short",
          day: "numeric",
          hour: "2-digit",
          minute: "2-digit",
        });
      } catch {
        return "";
      }
    };

    const refresh = async () => {
      list.replaceChildren();
      let items: HistoryItem[] = [];
      try {
        items = await invoke<HistoryItem[]>("get_history");
      } catch {
        // leave empty
      }
      if (!items.length) {
        list.appendChild(el("p", "peeky-hint", t("settings.history.empty")));
        return;
      }
      for (const it of items) {
        const row = el("div", "peeky-history-row");
        const meta = el("div", "peeky-history-meta");
        meta.appendChild(el("span", `peeky-history-badge peeky-badge-${it.mode}`, t(`mode.${it.mode}`)));
        meta.appendChild(el("span", "peeky-history-time", fmtTime(it.ts)));
        if (it.app) meta.appendChild(el("span", "peeky-history-app", it.app));
        row.appendChild(meta);
        row.appendChild(el("p", "peeky-history-text", it.text));
        list.appendChild(row);
      }
    };

    clearBtn.addEventListener("click", async () => {
      try {
        await invoke("clear_history");
      } catch {
        // ignore
      }
      clearBtn.textContent = t("settings.history.cleared");
      window.setTimeout(() => {
        clearBtn.textContent = t("settings.history.clear");
      }, 1200);
      void refresh();
    });

    void refresh();
  }

  function buildGeneralTab(root: HTMLElement): void {
    const sec = section(root);
    languageSelect = selectField(sec, {
      label: t("settings.language.label"),
      hint: t("settings.language.hint"),
      value: config.language,
      options: [
        { value: "auto", label: t("language.auto") },
        { value: "en", label: t("language.en") },
        { value: "zh", label: t("language.zh") },
        { value: "ja", label: t("language.ja") },
      ],
    });
    // Live language switch so the whole panel re-localizes immediately.
    languageSelect.addEventListener("change", async () => {
      const lang = languageSelect.value as LangSetting;
      config.language = lang as Language;
      readInto(config); // keep other edits when persisting inside setLanguage
      await setLanguage(lang); // persists Config + triggers onLanguageChange -> render()
    });

    // Screen Recording permission: status + guidance. Without it, capture is a
    // black frame, so the user needs a clear place to see & grant it.
    const permSec = section(root);
    fieldRow(permSec, t("settings.screenperm.label"), t("settings.screenperm.hint"), false);
    const permRow = el("div", "peeky-pause-row");
    const permStatus = el("span", "peeky-pause-status peeky-perm-status", "…");
    const permRecheck = el("button", "peeky-btn peeky-btn-secondary", t("settings.screenperm.recheck"));
    permRecheck.type = "button";
    const permGrant = el("button", "peeky-btn peeky-btn-secondary", t("settings.screenperm.grant"));
    permGrant.type = "button";

    async function refreshPerm(): Promise<void> {
      let authorized = false;
      let working = false;
      let settingsOpenSupported = false;
      try {
        const st = await invoke<{
          authorized?: boolean;
          working?: boolean;
          settings_open_supported?: boolean;
        }>("screen_capture_health");
        authorized = !!st?.authorized;
        working = !!st?.working;
        settingsOpenSupported = !!st?.settings_open_supported;
      } catch {
        /* outside Tauri: leave as denied-unknown */
      }
      // Three states: working / granted-but-black (needs restart) / not granted.
      if (working) {
        permStatus.textContent = t("settings.screenperm.granted");
      } else if (authorized) {
        permStatus.textContent = t("settings.screenperm.black");
      } else {
        permStatus.textContent = t("settings.screenperm.denied");
      }
      permStatus.classList.toggle("peeky-perm-ok", working);
      permStatus.classList.toggle("peeky-perm-bad", !working);
      // Only offer "Grant…" when not even authorized AND the host has a
      // system-settings pane we can route to. On Windows there is no
      // per-app screen-capture toggle (the grant is a one-shot modal), so
      // we hide the button entirely — clicking it would lead nowhere.
      permGrant.style.display =
        authorized || !settingsOpenSupported ? "none" : "";
    }
    permGrant.addEventListener("click", async () => {
      // Trigger the one-time system prompt, then open the pane for the toggle.
      try {
        await invoke("request_screen_capture");
      } catch {
        /* ignore */
      }
      try {
        await invoke("open_screen_settings");
      } catch {
        /* ignore */
      }
      window.setTimeout(() => void refreshPerm(), 800);
    });
    permRecheck.addEventListener("click", () => void refreshPerm());
    permRow.appendChild(permStatus);
    permRow.appendChild(permRecheck);
    permRow.appendChild(permGrant);
    permSec.appendChild(permRow);
    void refreshPerm();

    const powerSec = section(root);
    const pauseRow = el("div", "peeky-pause-row");
    const statusLabel = el(
      "span",
      "peeky-pause-status",
      paused ? t("settings.pause.paused") : t("settings.pause.active"),
    );
    const pauseBtn = el(
      "button",
      "peeky-btn peeky-btn-secondary",
      paused ? t("settings.pause.button.resume") : t("settings.pause.button.pause"),
    );
    pauseBtn.type = "button";
    pauseBtn.addEventListener("click", async () => {
      try {
        paused = await invoke<boolean>("pause_toggle");
      } catch {
        paused = !paused;
      }
      statusLabel.textContent = paused ? t("settings.pause.paused") : t("settings.pause.active");
      pauseBtn.textContent = paused
        ? t("settings.pause.button.resume")
        : t("settings.pause.button.pause");
    });
    pauseRow.appendChild(statusLabel);
    pauseRow.appendChild(pauseBtn);
    powerSec.appendChild(pauseRow);
  }

  // Read all editable fields back into the given config object.
  function readInto(cfg: Config): void {
    cfg.mode = modeSelect.value as ModeKind;
    cfg.api_base_url = baseUrlInput.value.trim();
    cfg.api_key = apiKeyInput.value;
    cfg.model = modelInput.value.trim();
    cfg.max_tokens = clampInt(maxTokensInput.value, 1, 4096, cfg.max_tokens);
    cfg.temperature = clampFloat(temperatureInput.value, 0, 2, cfg.temperature);
    cfg.reasoning_effort = reasoningSelect.value as ReasoningEffort;
    cfg.sensitivity = sensitivitySelect.value as Quality;
    cfg.speech_budget_per_hour = clampInt(budgetInput.value, 0, 120, cfg.speech_budget_per_hour);
    cfg.quiet_hours = {
      enabled: quietEnableInput.checked,
      start: quietStartInput.value || "22:00",
      end: quietEndInput.value || "09:00",
    };
    cfg.follow_system_dnd = followDndInput.checked;
    cfg.screenshot_quality = screenshotQualitySelect.value as Quality;
    cfg.permission_mode = permissionSelect.value as PermissionMode;
    cfg.language = languageSelect.value as Language;
    cfg.show_token_stats = showStatsInput.checked;
  }

  // Required-field validation (Base URL, API Key, model — PRD §8.2).
  function validate(result: HTMLElement): boolean {
    const problems: string[] = [];
    if (!baseUrlInput.value.trim()) problems.push(t("settings.baseUrl.required"));
    if (!apiKeyInput.value.trim()) problems.push(t("settings.apiKey.required"));
    if (!modelInput.value.trim()) problems.push(t("settings.model.required"));
    if (problems.length > 0) {
      result.className = "peeky-save-result err";
      result.textContent = problems.join(" ");
      return false;
    }
    return true;
  }

  // Re-render labels when the language changes (e.g. external trigger).
  const unsubscribe = onLanguageChange(() => {
    if (visible) render();
  });
  void unsubscribe; // kept alive for the panel lifetime

  // ---- public API --------------------------------------------------------
  async function open(): Promise<void> {
    // Always reload config so the panel reflects external changes.
    try {
      config = await invoke<Config>("get_config");
    } catch {
      config = defaultConfig();
    }
    visible = true;
    overlay.hidden = false;
    render();
    // Focus the first input for keyboard users.
    baseUrlInput?.focus();
  }

  function close(): void {
    if (standalone) {
      // Hide the OS window; the page stays mounted for the next open().
      void hideOwnWindow();
      return;
    }
    visible = false;
    overlay.hidden = true;
  }

  // Esc closes the panel.
  overlay.addEventListener("keydown", (e) => {
    if (e.key === "Escape") close();
  });

  return {
    open: () => void open(),
    close,
  };

  // ---- localized field builders (closures over nothing; pure helpers) ----

  function section(parent: HTMLElement, title?: string): HTMLElement {
    const sec = el("section", "peeky-section");
    if (title) sec.appendChild(el("h2", "peeky-section-title", title));
    parent.appendChild(sec);
    return sec;
  }

  function fieldRow(
    parent: HTMLElement,
    label: string,
    hint: string | undefined,
    required: boolean,
  ): HTMLElement {
    const row = el("div", "peeky-field");
    const lab = el("label", "peeky-label", label);
    if (required) {
      const star = el("span", "peeky-required", " *");
      star.title = t("common.required");
      lab.appendChild(star);
    }
    row.appendChild(lab);
    if (hint) row.appendChild(el("p", "peeky-hint", hint));
    parent.appendChild(row);
    return row;
  }

  function textField(
    parent: HTMLElement,
    opts: { label: string; hint?: string; placeholder?: string; value: string; required?: boolean },
  ): HTMLInputElement {
    const row = fieldRow(parent, opts.label, opts.hint, !!opts.required);
    const input = el("input", "peeky-input");
    input.type = "text";
    if (opts.placeholder) input.placeholder = opts.placeholder;
    input.value = opts.value;
    input.spellcheck = false;
    input.autocomplete = "off";
    row.appendChild(input);
    return input;
  }

  function numField(
    parent: HTMLElement,
    opts: { label: string; hint?: string; value: string; min: number; max: number; step: number },
  ): HTMLInputElement {
    const row = fieldRow(parent, opts.label, opts.hint, false);
    const input = el("input", "peeky-input");
    input.type = "number";
    input.value = opts.value;
    input.min = String(opts.min);
    input.max = String(opts.max);
    input.step = String(opts.step);
    row.appendChild(input);
    return input;
  }

  function timeField(parent: HTMLElement, label: string, value: string): HTMLInputElement {
    const row = fieldRow(parent, label, undefined, false);
    const input = el("input", "peeky-input");
    input.type = "time";
    input.value = value;
    row.appendChild(input);
    return input;
  }

  function selectField(
    parent: HTMLElement,
    opts: { label: string; hint?: string; value: string; options: { value: string; label: string }[] },
  ): HTMLSelectElement {
    const row = fieldRow(parent, opts.label, opts.hint, false);
    const sel = el("select", "peeky-select");
    for (const o of opts.options) sel.appendChild(option(o.value, o.label, o.value === opts.value));
    row.appendChild(sel);
    return sel;
  }

  function toggleField(
    parent: HTMLElement,
    opts: { label: string; hint?: string; checked: boolean },
  ): HTMLInputElement {
    const row = el("div", "peeky-field peeky-toggle-field");
    const wrap = el("label", "peeky-toggle");
    const input = el("input", "peeky-toggle-input");
    input.type = "checkbox";
    input.checked = opts.checked;
    const track = el("span", "peeky-toggle-track");
    const text = el("span", "peeky-toggle-label", opts.label);
    wrap.appendChild(input);
    wrap.appendChild(track);
    wrap.appendChild(text);
    row.appendChild(wrap);
    if (opts.hint) row.appendChild(el("p", "peeky-hint", opts.hint));
    parent.appendChild(row);
    return input;
  }

  function statLine(label: string, value: number): HTMLElement {
    const line = el("div", "peeky-stat-line");
    line.appendChild(el("span", "peeky-stat-label", label));
    line.appendChild(el("span", "peeky-stat-value", String(value)));
    return line;
  }

  function qualityOptions(): { value: string; label: string }[] {
    return [
      { value: "low", label: t("quality.low") },
      { value: "med", label: t("quality.med") },
      { value: "high", label: t("quality.high") },
    ];
  }
}

// ---- module-level utilities ----------------------------------------------

/** Hide the current OS window (used to "close" the standalone settings window). */
async function hideOwnWindow(): Promise<void> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().hide();
  } catch {
    // Not running inside Tauri (plain vite preview) — nothing to hide.
  }
}

function clampInt(raw: string, min: number, max: number, fallback: number): number {
  const n = parseInt(raw, 10);
  if (Number.isNaN(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function clampFloat(raw: string, min: number, max: number, fallback: number): number {
  const n = parseFloat(raw);
  if (Number.isNaN(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function formatErr(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

/** Copy text to the clipboard, with a textarea-selection fallback. */
async function copyText(text: string, source: HTMLTextAreaElement): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through to legacy path
  }
  try {
    source.removeAttribute("readonly");
    source.focus();
    source.select();
    const ok = document.execCommand("copy");
    source.setAttribute("readonly", "true");
    source.setSelectionRange(0, 0);
    source.blur();
    return ok;
  } catch {
    return false;
  }
}

// Referenced so `getLang` import stays meaningful for future use (avoids
// unused-import friction if a consumer needs the active language).
export const __i18nProbe = getLang;
