// Shared preset editor — extracted from the firmware's preset editor.
// Self-contained: injects its own CSS, creates its own overlay DOM.
// Uses CSS variables for theming (define --bg, --surface, --surface2, --text,
// --text2, --accent, --ok, --warn, --danger on :root or a parent element).

// ── Types (structurally compatible with both projects' protocol types) ──

export interface EditorCollar {
  name: string;
  collar_id: number;
  channel: number;
}

export interface EditorPresetStep {
  mode: "shock" | "vibrate" | "beep" | "pause";
  intensity: number;
  intensity_max?: number;
  duration_ms: number;
  duration_max_ms?: number;
  intensity_distribution?: "uniform" | "gaussian";
  duration_distribution?: "uniform" | "gaussian";
}

export interface EditorPresetTrack {
  collar_name: string;
  steps: EditorPresetStep[];
}

export interface EditorPreset {
  name: string;
  tracks: EditorPresetTrack[];
}

export interface EditorPresetPreviewEvent {
  requested_time_us: number;
  actual_time_us: number;
  track_index: number;
  step_index: number;
  transmit_duration_us: number;
  collar_name: string;
  collar_id: number;
  channel: number;
  mode: string;
  mode_byte: number;
  intensity: number;
  raw_hex: string;
}

export interface EditorPresetPreview {
  total_duration_us: number;
  events: EditorPresetPreviewEvent[];
}

export interface EditorModeLimit {
  maxIntensity: number;
  maxDurationMs: number;
}

export interface EditorCollarPermission {
  collarName: string;
  shock: EditorModeLimit | null;
  vibrate: EditorModeLimit | null;
  beep: { maxDurationMs: number } | null;
}

// ── Configuration ──

export interface PresetEditorConfig {
  collars: EditorCollar[];
  permissions?: EditorCollarPermission[];
  onSave: (originalName: string | null, preset: EditorPreset) => Promise<void>;
  onPreview?: (nonce: number, preset: EditorPreset) => void;
}

// ── Constants ──

const DURATION_MIN_MS = 500;
const DURATION_MAX_MS = 10000;
const DURATION_STEP_MS = 500;
const PREVIEW_DEBOUNCE_MS = 150;
const TRACK_COLORS = ["#4ecca3", "#ffc947", "#e94560", "#6fa8ff", "#ff8fab", "#b8de6f"];
const MODE_EMOJI: Record<string, string> = { shock: "\u26A1", vibrate: "\u3030\uFE0F", beep: "\uD83D\uDD0A", pause: "\u23F8\uFE0F" };
const MODE_LABEL: Record<string, string> = { shock: "Shock", vibrate: "Vibrate", beep: "Beep", pause: "Pause" };

// ── State ──

let config: PresetEditorConfig | null = null;
let editorData: EditorPreset | null = null;
let editorOriginalName: string | null = null;
let editorOpenTrack = -1;
let overlayEl: HTMLDivElement | null = null;

let previewNonce = 0;
let previewTimer: ReturnType<typeof setTimeout> | null = null;
let previewState: { loading: boolean; error: string | null; data: EditorPresetPreview | null } = {
  loading: false, error: null, data: null,
};

// ── Duration helpers ──

function normalizeDuration(ms: number, minMs: number = DURATION_MIN_MS): number {
  const v = Number.isFinite(ms) ? ms : minMs;
  return Math.round(Math.min(DURATION_MAX_MS, Math.max(minMs, v)) / DURATION_STEP_MS) * DURATION_STEP_MS;
}

function formatEditorDuration(ms: number, minMs: number = DURATION_MIN_MS): string {
  const s = normalizeDuration(ms, minMs) / 1000;
  return Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`;
}

function formatIntensityVal(step: EditorPresetStep): string {
  if (step.intensity_max !== undefined) return `${step.intensity}-${step.intensity_max}`;
  return String(step.intensity);
}

function formatDurationVal(step: EditorPresetStep): string {
  const minDur = step.mode === "pause" ? 0 : DURATION_MIN_MS;
  if (step.duration_max_ms !== undefined) return `${formatEditorDuration(step.duration_ms, minDur)}-${formatEditorDuration(step.duration_max_ms, minDur)}`;
  return formatEditorDuration(step.duration_ms, minDur);
}

function normalizeEditorDurations(preset: EditorPreset): void {
  for (const track of preset.tracks) {
    for (const step of track.steps) {
      const minDur = step.mode === "pause" ? 0 : DURATION_MIN_MS;
      step.duration_ms = normalizeDuration(step.duration_ms, minDur);
      if (step.duration_max_ms !== undefined) {
        step.duration_max_ms = normalizeDuration(step.duration_max_ms, minDur);
        if (step.duration_max_ms <= step.duration_ms) {
          delete step.duration_max_ms;
          delete step.duration_distribution;
        }
      }
      if (step.intensity_max !== undefined) {
        if (step.intensity_max <= step.intensity) {
          delete step.intensity_max;
          delete step.intensity_distribution;
        }
      }
    }
  }
}

function fmtUs(us: number): string {
  const ms = Math.round(us / 1000);
  if (ms === 0) return "0ms";
  if (ms % 1000 === 0) return `${ms / 1000}s`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(3)}s`;
  return `${ms}ms`;
}

function fmtMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

function describeMode(mode: string): string {
  return MODE_LABEL[mode] ?? mode;
}

function segTitle(ev: EditorPresetPreviewEvent): string {
  const delayUs = ev.actual_time_us - ev.requested_time_us;
  return `Track ${ev.track_index + 1}, step ${ev.step_index + 1}, ${ev.collar_name}, ${describeMode(ev.mode)}, level ${ev.intensity}, requested ${fmtUs(ev.requested_time_us)}, actual ${fmtUs(ev.actual_time_us)}, delay ${fmtUs(delayUs)}, TX ${fmtUs(ev.transmit_duration_us)}`;
}

function esc(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

// ── Permission helpers ──

function getCollarPerm(collarName: string): EditorCollarPermission | undefined {
  return config?.permissions?.find((c) => c.collarName === collarName);
}

function getMaxIntensity(collarName: string, mode: string): number {
  if (!config?.permissions) return 99;
  const cp = getCollarPerm(collarName);
  if (!cp) return 99;
  if (mode === "shock" && cp.shock) return cp.shock.maxIntensity;
  if (mode === "vibrate" && cp.vibrate) return cp.vibrate.maxIntensity;
  return 99;
}

function getMaxDuration(collarName: string, mode: string): number {
  if (!config?.permissions) return DURATION_MAX_MS;
  const cp = getCollarPerm(collarName);
  if (!cp) return DURATION_MAX_MS;
  if (mode === "shock" && cp.shock) return Math.min(cp.shock.maxDurationMs, DURATION_MAX_MS);
  if (mode === "vibrate" && cp.vibrate) return Math.min(cp.vibrate.maxDurationMs, DURATION_MAX_MS);
  if (mode === "beep" && cp.beep) return Math.min(cp.beep.maxDurationMs, DURATION_MAX_MS);
  return DURATION_MAX_MS;
}

// ── CSS injection ──
// Uses CSS variables from the host page with fallback defaults (firmware theme).

let stylesInjected = false;

function injectStyles(): void {
  if (stylesInjected) return;
  stylesInjected = true;
  const style = document.createElement("style");
  style.textContent = `
    .pe-overlay {
      display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.7);
      z-index: 100; overflow-y: auto;
      --pe-bg: var(--bg, #1a1a2e);
      --pe-surface: var(--surface, var(--panel, #16213e));
      --pe-surface2: var(--surface2, var(--panel-strong, #0f3460));
      --pe-text: var(--text, #eee);
      --pe-text2: var(--text2, var(--muted, #aaa));
      --pe-accent: var(--accent, #e94560);
      --pe-ok: var(--ok, #4ecca3);
      --pe-warn: var(--warn, #ffc947);
      --pe-danger: var(--danger, #e94560);
    }
    .pe-overlay.active { display: flex; justify-content: center; padding: 10px; }
    .pe-box { background: var(--pe-bg); border-radius: 8px; padding: 12px; width: 100%; max-width: 600px; max-height: 90vh; overflow-y: auto; color: var(--pe-text); font-family: system-ui, sans-serif; }
    .pe-box h2 { margin-bottom: 14px; font-size: 1.1em; }
    .pe-box label span { color: var(--pe-text2); font-size: 0.85em; }
    .pe-box input, .pe-box select { background: var(--pe-surface2); color: var(--pe-text); border: 1px solid #333; border-radius: 4px; padding: 6px 8px; font: inherit; width: 100%; box-sizing: border-box; font-size: 0.9em; }
    .pe-box button { font: inherit; border: none; border-radius: 4px; padding: 8px 14px; font-size: 0.9em; color: #fff; background: var(--pe-surface2); cursor: pointer; }
    .pe-box button:active { opacity: 0.8; }
    .pe-box button.accent { background: var(--pe-ok); color: #000; font-weight: bold; }
    .pe-box button.danger { background: var(--pe-danger); }
    .pe-track { background: var(--pe-surface); border-radius: 6px; padding: 8px; margin-bottom: 10px; }
    .pe-track-header { cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px; }
    .pe-track-header .fold-arrow { transition: transform 0.15s; display: inline-block; }
    .pe-track-header .fold-arrow.open { transform: rotate(90deg); }
    .pe-track-body { display: none; margin-top: 8px; }
    .pe-track-body.open { display: block; }
    .pe-step { display: flex; flex-direction: column; gap: 6px; margin-bottom: 8px; padding: 6px; border-radius: 6px; background: rgba(255,255,255,0.03); }
    .pe-step select, .pe-step input { font-size: 0.85em; padding: 4px 6px; }
    .pe-step-header { display: flex; gap: 4px; align-items: center; }
    .pe-step-header select { min-width: 0; width: auto; }
    .pe-step-header select[data-step-mode] { flex: 1; }
    .pe-drag-handle { cursor: grab; color: var(--pe-text2); font-size: 1rem; line-height: 1; padding: 2px 2px; user-select: none; flex-shrink: 0; }
    .pe-drag-handle:active { cursor: grabbing; }
    .pe-step-controls { display: flex; flex-direction: column; gap: 4px; }
    .pe-slider { display: flex; align-items: center; gap: 4px; min-width: 0; width: 100%; }
    .pe-slider .slider-label { width: 52px; flex-shrink: 0; font-size: 0.8em; color: var(--pe-text2); }
    .pe-slider input[type=range] { flex: 1; accent-color: var(--pe-accent); }
    .pe-slider .slider-val { min-width: 28px; text-align: right; font-size: 0.85em; color: var(--pe-text2); }
    .pe-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 14px; }
    .pe-header h2 { margin: 0; font-size: 1.1em; }
    .pe-header-actions { display: flex; gap: 8px; }
    .pe-limits { margin-top: 0.75rem; padding: 0.6rem; border-radius: 6px; background: var(--pe-surface); font-size: 0.8rem; color: var(--pe-text2); }
    .pe-preview { margin-top: 12px; background: var(--pe-surface); border-radius: 6px; padding: 10px; }
    .pe-preview summary { cursor: pointer; font-weight: bold; }
    .pe-preview .ep-status { font-size: 0.85em; color: var(--pe-text2); margin-top: 8px; }
    .pe-preview .ep-status.ep-error { color: var(--pe-warn); }
    .pe-preview .ep-summary { font-size: 0.8em; color: var(--pe-text2); margin-top: 8px; }
    .pe-preview-legend { margin-top: 10px; font-size: 0.75em; line-height: 1.4; color: var(--pe-text2); padding: 8px 10px; border-radius: 6px; background: rgba(255,255,255,0.03); border: 1px solid rgba(255,255,255,0.06); }
    .pe-preview-legend-row + .pe-preview-legend-row { margin-top: 4px; }
    .pe-preview-legend strong { color: var(--pe-text); font-weight: 600; }
    .ep-timeline { margin-top: 10px; }
    .ep-timeline-scale { display: flex; justify-content: space-between; gap: 8px; margin-bottom: 4px; font-size: 0.72em; color: var(--pe-text2); font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
    .ep-timeline-bar { display: flex; border-radius: 6px; overflow: hidden; background: linear-gradient(180deg, rgba(255,255,255,0.05), rgba(255,255,255,0.02)); border: 1px solid rgba(255,255,255,0.1); height: 20px; }
    .pe-box .ep-seg { flex: none; height: 100%; border: none; border-radius: 0; cursor: pointer; opacity: 0.85; min-width: 1px; padding: 0; background: none; }
    .pe-box .ep-seg:hover, .pe-box .ep-seg:focus-visible, .pe-box .ep-seg.active { opacity: 1; outline: none; }
    .ep-table-wrap { margin-top: 10px; overflow-x: auto; }
    .ep-table { width: 100%; border-collapse: collapse; font-size: 0.72em; }
    .ep-table th, .ep-table td { text-align: left; padding: 4px 6px; border-bottom: 1px solid rgba(255,255,255,0.08); vertical-align: top; }
    .ep-table th { color: var(--pe-text2); font-weight: 600; white-space: nowrap; }
    .ep-table td { white-space: nowrap; }
    .ep-table .frame-hex { white-space: normal; word-break: break-all; }
    .ep-row { transition: background-color 0.12s; }
    .ep-row.active { background: rgba(255,255,255,0.08); }
    .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
    .pe-mode-select { width: auto; min-width: 0; flex-shrink: 0; font-size: 0.8em; padding: 2px 4px; }
    .pe-range-slider { position: relative; height: 20px; flex: 1; }
    .pe-range-slider input[type=range] { position: absolute; width: 100%; top: 0; height: 20px; pointer-events: none; -webkit-appearance: none; appearance: none; background: transparent; margin: 0; }
    .pe-range-slider input[type=range]::-webkit-slider-thumb { pointer-events: all; -webkit-appearance: none; appearance: none; width: 14px; height: 14px; border-radius: 50%; background: var(--pe-accent); cursor: pointer; border: none; }
    .pe-range-slider input[type=range]::-moz-range-thumb { pointer-events: all; width: 14px; height: 14px; border-radius: 50%; background: var(--pe-accent); cursor: pointer; border: none; }
    .pe-range-slider input[type=range].range-min { z-index: 1; }
    .pe-range-slider input[type=range].range-max { z-index: 2; }
    .pe-range-slider input[type=range]::-webkit-slider-runnable-track { height: 4px; background: rgba(255,255,255,0.15); border-radius: 2px; }
    .pe-range-slider input[type=range]::-moz-range-track { height: 4px; background: rgba(255,255,255,0.15); border-radius: 2px; }
  `;
  document.head.appendChild(style);
}

// ── Public API ──

export function openEditor(cfg: PresetEditorConfig, preset: EditorPreset | null, originalName: string | null): void {
  injectStyles();
  config = cfg;
  editorOpenTrack = -1;
  previewState = { loading: false, error: null, data: null };
  previewNonce = 0;

  if (preset) {
    editorData = JSON.parse(JSON.stringify(preset));
    editorOriginalName = originalName;
  } else {
    editorData = { name: "", tracks: [] };
    editorOriginalName = null;
  }
  normalizeEditorDurations(editorData!);

  if (!overlayEl) {
    overlayEl = document.createElement("div");
    overlayEl.className = "pe-overlay";
    document.body.appendChild(overlayEl);
  }
  overlayEl.classList.add("active");
  renderEditor();
}

export function closeEditor(): void {
  overlayEl?.classList.remove("active");
  if (previewTimer !== null) clearTimeout(previewTimer);
  previewTimer = null;
  editorData = null;
  editorOriginalName = null;
  config = null;
}

export function handlePreviewResult(nonce: number, preview: EditorPresetPreview | null, error: string | null): void {
  if (!editorData || nonce !== previewNonce) return;
  previewState = { loading: false, error, data: preview };
  renderEditorPreview();
}

// ── Rendering ──

function renderEditor(): void {
  if (!overlayEl || !editorData || !config) return;

  const limitsHtml = config.permissions ? `
    <div class="pe-limits">
      <div style="font-weight:600;margin-bottom:0.3rem">Your Limits</div>
      ${config.permissions.map((cp) => {
        const modes: string[] = [];
        if (cp.shock) modes.push(`shock(max ${cp.shock.maxIntensity}, ${fmtMs(cp.shock.maxDurationMs)})`);
        if (cp.vibrate) modes.push(`vibrate(max ${cp.vibrate.maxIntensity}, ${fmtMs(cp.vibrate.maxDurationMs)})`);
        if (cp.beep) modes.push(`beep(${fmtMs(cp.beep.maxDurationMs)})`);
        return `<div>${esc(cp.collarName)}: ${modes.join(", ")}</div>`;
      }).join("")}
    </div>` : "";

  overlayEl.innerHTML = `<div class="pe-box">
    <div class="pe-header">
      <h2>${editorOriginalName ? "Edit Preset" : "New Preset"}</h2>
      <div class="pe-header-actions">
        <button id="pe-cancel">Cancel</button>
        <button class="accent" id="pe-save">Save</button>
      </div>
    </div>
    <div style="margin-bottom:0.75rem"><label><span>Name</span><input type="text" id="pe-name" value="${esc(editorData.name)}" placeholder="Preset name"></label></div>
    <div id="pe-tracks"></div>
    <button id="pe-add-track" style="margin-top:8px">+ Add Track</button>
    ${limitsHtml}
    <details class="pe-preview" id="pe-preview-panel" ${config.onPreview ? "" : "hidden"}>
      <summary>Preview</summary>
      <div class="ep-status" id="ep-status">No preview yet.</div>
      <div class="ep-summary" id="ep-summary"></div>
      <div class="ep-timeline" id="ep-timeline"></div>
      <div class="ep-table-wrap" id="ep-events"></div>
      <div class="pe-preview-legend" id="ep-legend">
        <div class="pe-preview-legend-row"><strong>At</strong>: actual RF transmit start after single-transmitter serialization.</div>
        <div class="pe-preview-legend-row"><strong>\uD83C\uDFAF Requested</strong>: ideal transmit start before any collision shifting.</div>
        <div class="pe-preview-legend-row"><strong>Delay</strong>: serialization slip, equal to <span class="mono">At \u2212 Requested</span>.</div>
        <div class="pe-preview-legend-row"><strong>\u23F1\uFE0F TX</strong>: on-air transmit duration of one RF message, derived from the encoder waveform.</div>
      </div>
    </details>
  </div>`;

  const nameEl = overlayEl.querySelector("#pe-name") as HTMLInputElement;
  nameEl.addEventListener("input", () => { if (editorData) { editorData.name = nameEl.value; schedulePreviewRefresh(); } });
  overlayEl.querySelector("#pe-add-track")!.addEventListener("click", editorAddTrack);
  overlayEl.querySelector("#pe-cancel")!.addEventListener("click", closeEditor);
  overlayEl.querySelector("#pe-save")!.addEventListener("click", editorSave);

  renderEditorTracks();
}

function editorSave(): void {
  if (!editorData || !config) return;
  editorData.name = (overlayEl?.querySelector("#pe-name") as HTMLInputElement)?.value.trim() ?? "";
  if (!editorData.name) { alert("Preset name required"); return; }
  normalizeEditorDurations(editorData);
  config.onSave(editorOriginalName, editorData).then(() => closeEditor()).catch((err) => {
    alert(err instanceof Error ? err.message : "Failed to save preset");
  });
}

function editorAddTrack(): void {
  if (!editorData || !config) return;
  const available = config.permissions
    ? config.collars.filter((c) => config!.permissions!.some((cp) => cp.collarName === c.name))
    : config.collars;
  const defaultCollar = available[0]?.name ?? "";
  editorData.tracks.push({ collar_name: defaultCollar, steps: [] });
  editorOpenTrack = editorData.tracks.length - 1;
  renderEditorTracks();
}

function editorRemoveTrack(ti: number): void {
  if (!editorData) return;
  editorData.tracks.splice(ti, 1);
  if (editorOpenTrack === ti) editorOpenTrack = -1;
  else if (editorOpenTrack > ti) editorOpenTrack--;
  if (editorOpenTrack >= editorData.tracks.length) editorOpenTrack = editorData.tracks.length - 1;
  renderEditorTracks();
}

function editorToggleTrack(ti: number): void {
  editorOpenTrack = editorOpenTrack === ti ? -1 : ti;
  renderEditorTracks();
}

function renderEditorTracks(): void {
  if (!editorData || !overlayEl || !config) return;
  const collars = config.permissions
    ? config.collars.filter((c) => config!.permissions!.some((cp) => cp.collarName === c.name))
    : config.collars;
  const container = overlayEl.querySelector("#pe-tracks")!;
  container.innerHTML = "";

  editorData.tracks.forEach((track, ti) => {
    const isOpen = ti === editorOpenTrack;
    const div = document.createElement("div");
    div.className = "pe-track";

    const collarOpts = collars.map((c) =>
      `<option value="${esc(c.name)}" ${c.name === track.collar_name ? "selected" : ""}>${esc(c.name)}</option>`
    ).join("");

    div.innerHTML = `
      <div class="pe-track-header" data-track-toggle="${ti}">
        <span class="fold-arrow ${isOpen ? "open" : ""}">&#9654;</span>
        <span style="color:var(--pe-text2);font-size:0.85em">Collar:</span>
        <select data-track-collar="${ti}" onclick="event.stopPropagation()" style="flex:1;min-width:0">${collarOpts}</select>
        <span style="color:var(--pe-text2);font-size:0.85em">(${track.steps.length} steps)</span>
        <button class="danger" data-track-remove="${ti}" style="padding:0.3rem 0.6rem">X</button>
      </div>
      <div class="pe-track-body ${isOpen ? "open" : ""}" id="pe-track-body-${ti}"></div>`;

    div.querySelector("[data-track-toggle]")!.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).closest("select, button")) return;
      editorToggleTrack(ti);
    });
    div.querySelector("[data-track-collar]")!.addEventListener("change", (e) => {
      track.collar_name = (e.target as HTMLSelectElement).value;
      schedulePreviewRefresh();
    });
    div.querySelector("[data-track-remove]")!.addEventListener("click", (e) => {
      e.stopPropagation();
      editorRemoveTrack(ti);
    });

    const body = div.querySelector(".pe-track-body")!;
    track.steps.forEach((step, si) => {
      body.appendChild(renderEditorStep(track, ti, si));
    });

    const addBtn = document.createElement("button");
    addBtn.textContent = "+ Step";
    addBtn.style.marginTop = "0.4rem";
    addBtn.style.fontSize = "0.85em";
    addBtn.addEventListener("click", () => {
      track.steps.push({ mode: "vibrate", intensity: 30, duration_ms: 1000 });
      renderEditorTracks();
    });
    body.appendChild(addBtn);
    container.appendChild(div);
  });

  schedulePreviewRefresh();
}

function renderEditorStep(track: { steps: EditorPresetStep[]; collar_name: string }, ti: number, si: number): HTMLElement {
  const step = track.steps[si]!;
  const noLevel = step.mode === "pause" || step.mode === "beep";
  const isPause = step.mode === "pause";
  const maxInt = getMaxIntensity(track.collar_name, step.mode);
  const maxDurMs = getMaxDuration(track.collar_name, step.mode);
  const minDurMs = isPause ? 0 : DURATION_MIN_MS;
  const durSec = (normalizeDuration(step.duration_ms, minDurMs) / 1000).toFixed(1);
  const maxDurSec = maxDurMs / 1000;
  const minDurSec = minDurMs / 1000;
  const intensity = Math.min(step.intensity, maxInt);
  const durVal = Math.min(parseFloat(durSec), maxDurSec);
  const hasIntRange = step.intensity_max !== undefined;
  const hasDurRange = step.duration_max_ms !== undefined;
  const intMax = step.intensity_max ?? maxInt;
  const durMaxVal = hasDurRange ? Math.min(normalizeDuration(step.duration_max_ms!, minDurMs) / 1000, maxDurSec) : maxDurSec;

  const div = document.createElement("div");
  div.className = "pe-step";

  div.innerHTML = `
    <div style="flex:1">
      <div class="pe-step-header">
        <span class="pe-drag-handle" title="Drag to reorder">&#x2630;</span>
        <select data-step-mode>
          ${(["shock", "vibrate", "beep", "pause"] as const).map((m) => `<option value="${m}" ${step.mode === m ? "selected" : ""}>${describeMode(m)}</option>`).join("")}
        </select>
        ${noLevel ? "" : `<select class="pe-mode-select" data-intensity-mode title="Level mode">
          <option value="fixed" ${!hasIntRange ? "selected" : ""}>\uD83D\uDCC8 Fixed</option>
          <option value="random" ${hasIntRange && step.intensity_distribution !== "gaussian" ? "selected" : ""}>\uD83D\uDCC8 Random</option>
          <option value="gaussian" ${hasIntRange && step.intensity_distribution === "gaussian" ? "selected" : ""}>\uD83D\uDCC8 Gaussian</option>
        </select>`}
        <select class="pe-mode-select" data-duration-mode title="Duration mode">
          <option value="fixed" ${!hasDurRange ? "selected" : ""}>\u23F1 Fixed</option>
          <option value="random" ${hasDurRange && step.duration_distribution !== "gaussian" ? "selected" : ""}>\u23F1 Random</option>
          <option value="gaussian" ${hasDurRange && step.duration_distribution === "gaussian" ? "selected" : ""}>\u23F1 Gaussian</option>
        </select>
        <button data-step-copy style="padding:0.3rem 0.6rem" title="Duplicate step">📋</button>
        <button class="danger" data-step-remove style="padding:0.3rem 0.6rem">X</button>
      </div>
      <div class="pe-step-controls">
        ${noLevel ? "" : `<div class="pe-slider">
          <span class="slider-label">Level</span>
          <input type="range" min="0" max="${maxInt}" value="${intensity}" data-step-intensity style="${hasIntRange ? "display:none" : ""}">
          <div class="pe-range-slider" style="${hasIntRange ? "" : "display:none"}" data-intensity-range>
            <input type="range" class="range-min" min="0" max="${maxInt}" value="${intensity}">
            <input type="range" class="range-max" min="0" max="${maxInt}" value="${intMax}">
          </div>
          <span class="slider-val" data-intensity-val>${formatIntensityVal(step)}</span>
        </div>`}
        <div class="pe-slider">
          <span class="slider-label">Duration</span>
          <input type="range" min="${minDurSec}" max="${maxDurSec}" step="${DURATION_STEP_MS / 1000}" value="${durVal}" data-step-duration style="${hasDurRange ? "display:none" : ""}">
          <div class="pe-range-slider" style="${hasDurRange ? "" : "display:none"}" data-duration-range>
            <input type="range" class="range-min" min="${minDurSec}" max="${maxDurSec}" step="${DURATION_STEP_MS / 1000}" value="${durVal}">
            <input type="range" class="range-max" min="${minDurSec}" max="${maxDurSec}" step="${DURATION_STEP_MS / 1000}" value="${durMaxVal}">
          </div>
          <span class="slider-val" data-duration-val>${formatDurationVal(step)}</span>
        </div>
      </div>
    </div>`;

  div.querySelector("[data-step-mode]")!.addEventListener("change", (e) => {
    step.mode = (e.target as HTMLSelectElement).value as EditorPresetStep["mode"];
    renderEditorTracks();
  });

  div.querySelector("[data-step-copy]")!.addEventListener("click", () => {
    track.steps.splice(si + 1, 0, JSON.parse(JSON.stringify(step)));
    renderEditorTracks();
  });

  div.querySelector("[data-step-remove]")!.addEventListener("click", () => {
    track.steps.splice(si, 1);
    renderEditorTracks();
  });

  // Intensity fixed slider
  const intSlider = div.querySelector("[data-step-intensity]") as HTMLInputElement | null;
  if (intSlider) {
    intSlider.addEventListener("input", () => {
      step.intensity = parseInt(intSlider.value, 10);
      div.querySelector("[data-intensity-val]")!.textContent = formatIntensityVal(step);
      schedulePreviewRefresh();
    });
  }

  // Intensity mode toggle (Fixed / Random / Gaussian)
  const intModeSelect = div.querySelector("[data-intensity-mode]") as HTMLSelectElement | null;
  if (intModeSelect) {
    intModeSelect.addEventListener("change", () => {
      const mode = intModeSelect.value;
      const fixedSlider = div.querySelector("[data-step-intensity]") as HTMLInputElement;
      const rangeDiv = div.querySelector("[data-intensity-range]") as HTMLElement;
      if (mode === "random" || mode === "gaussian") {
        fixedSlider.style.display = "none";
        rangeDiv.style.display = "";
        if (step.intensity_max === undefined) {
          const cur = step.intensity;
          step.intensity_max = Math.min(cur + 10, getMaxIntensity(track.collar_name, step.mode));
          if (step.intensity_max <= step.intensity) step.intensity_max = step.intensity + 1;
          const rangeMinInput = rangeDiv.querySelector(".range-min") as HTMLInputElement;
          const rangeMaxInput = rangeDiv.querySelector(".range-max") as HTMLInputElement;
          rangeMinInput.value = String(step.intensity);
          rangeMaxInput.value = String(step.intensity_max);
        }
        if (mode === "gaussian") {
          step.intensity_distribution = "gaussian";
        } else {
          delete step.intensity_distribution;
        }
      } else {
        fixedSlider.style.display = "";
        rangeDiv.style.display = "none";
        const rangeMinInput = rangeDiv.querySelector(".range-min") as HTMLInputElement;
        const rangeMaxInput = rangeDiv.querySelector(".range-max") as HTMLInputElement;
        const mid = Math.round((parseInt(rangeMinInput.value, 10) + parseInt(rangeMaxInput.value, 10)) / 2);
        step.intensity = mid;
        delete step.intensity_max;
        delete step.intensity_distribution;
        fixedSlider.value = String(mid);
      }
      div.querySelector("[data-intensity-val]")!.textContent = formatIntensityVal(step);
      schedulePreviewRefresh();
    });
  }

  // Intensity range slider (dual-thumb)
  const intRangeDiv = div.querySelector("[data-intensity-range]") as HTMLElement | null;
  if (intRangeDiv) {
    const intRangeMin = intRangeDiv.querySelector(".range-min") as HTMLInputElement;
    const intRangeMax = intRangeDiv.querySelector(".range-max") as HTMLInputElement;
    intRangeMin.addEventListener("input", () => {
      let v = parseInt(intRangeMin.value, 10);
      if (v > parseInt(intRangeMax.value, 10)) { v = parseInt(intRangeMax.value, 10); intRangeMin.value = String(v); }
      step.intensity = v;
      div.querySelector("[data-intensity-val]")!.textContent = formatIntensityVal(step);
      schedulePreviewRefresh();
    });
    intRangeMax.addEventListener("input", () => {
      let v = parseInt(intRangeMax.value, 10);
      if (v < parseInt(intRangeMin.value, 10)) { v = parseInt(intRangeMin.value, 10); intRangeMax.value = String(v); }
      step.intensity_max = v;
      div.querySelector("[data-intensity-val]")!.textContent = formatIntensityVal(step);
      schedulePreviewRefresh();
    });
  }

  // Duration fixed slider
  const durSlider = div.querySelector("[data-step-duration]") as HTMLInputElement;
  durSlider.addEventListener("input", () => {
    step.duration_ms = normalizeDuration(Math.round(parseFloat(durSlider.value) * 1000), minDurMs);
    div.querySelector("[data-duration-val]")!.textContent = formatDurationVal(step);
    schedulePreviewRefresh();
  });

  // Duration mode toggle (Fixed / Random / Gaussian)
  const durModeSelect = div.querySelector("[data-duration-mode]") as HTMLSelectElement;
  durModeSelect.addEventListener("change", () => {
    const mode = durModeSelect.value;
    const fixedSlider = div.querySelector("[data-step-duration]") as HTMLInputElement;
    const rangeDiv = div.querySelector("[data-duration-range]") as HTMLElement;
    if (mode === "random" || mode === "gaussian") {
      fixedSlider.style.display = "none";
      rangeDiv.style.display = "";
      if (step.duration_max_ms === undefined) {
        const curMs = step.duration_ms;
        step.duration_max_ms = normalizeDuration(Math.min(curMs + DURATION_STEP_MS, getMaxDuration(track.collar_name, step.mode)), minDurMs);
        if (step.duration_max_ms <= step.duration_ms) step.duration_max_ms = step.duration_ms + DURATION_STEP_MS;
        const rangeMinInput = rangeDiv.querySelector(".range-min") as HTMLInputElement;
        const rangeMaxInput = rangeDiv.querySelector(".range-max") as HTMLInputElement;
        rangeMinInput.value = String(step.duration_ms / 1000);
        rangeMaxInput.value = String(step.duration_max_ms / 1000);
      }
      if (mode === "gaussian") {
        step.duration_distribution = "gaussian";
      } else {
        delete step.duration_distribution;
      }
    } else {
      fixedSlider.style.display = "";
      rangeDiv.style.display = "none";
      const rangeMinInput = rangeDiv.querySelector(".range-min") as HTMLInputElement;
      const rangeMaxInput = rangeDiv.querySelector(".range-max") as HTMLInputElement;
      const midMs = normalizeDuration(Math.round((parseFloat(rangeMinInput.value) + parseFloat(rangeMaxInput.value)) / 2 * 1000), minDurMs);
      step.duration_ms = midMs;
      delete step.duration_max_ms;
      delete step.duration_distribution;
      fixedSlider.value = String(midMs / 1000);
    }
    div.querySelector("[data-duration-val]")!.textContent = formatDurationVal(step);
    schedulePreviewRefresh();
  });

  // Duration range slider (dual-thumb)
  const durRangeDiv = div.querySelector("[data-duration-range]") as HTMLElement;
  const durRangeMin = durRangeDiv.querySelector(".range-min") as HTMLInputElement;
  const durRangeMax = durRangeDiv.querySelector(".range-max") as HTMLInputElement;
  durRangeMin.addEventListener("input", () => {
    let v = parseFloat(durRangeMin.value);
    if (v > parseFloat(durRangeMax.value)) { v = parseFloat(durRangeMax.value); durRangeMin.value = String(v); }
    step.duration_ms = normalizeDuration(Math.round(v * 1000), minDurMs);
    div.querySelector("[data-duration-val]")!.textContent = formatDurationVal(step);
    schedulePreviewRefresh();
  });
  durRangeMax.addEventListener("input", () => {
    let v = parseFloat(durRangeMax.value);
    if (v < parseFloat(durRangeMin.value)) { v = parseFloat(durRangeMin.value); durRangeMax.value = String(v); }
    step.duration_max_ms = normalizeDuration(Math.round(v * 1000), minDurMs);
    div.querySelector("[data-duration-val]")!.textContent = formatDurationVal(step);
    schedulePreviewRefresh();
  });

  // Drag-and-drop reordering within same track — only via handle
  const handle = div.querySelector(".pe-drag-handle")!;
  handle.addEventListener("mousedown", () => { div.draggable = true; });
  handle.addEventListener("touchstart", () => { div.draggable = true; }, { passive: true });
  div.addEventListener("dragstart", (e) => {
    e.dataTransfer!.setData("text/plain", `step:${ti}:${si}`);
    div.style.opacity = "0.5";
    e.stopPropagation();
  });
  div.addEventListener("dragend", () => { div.style.opacity = ""; div.draggable = false; });
  div.addEventListener("dragover", (e) => { e.preventDefault(); div.style.borderTop = "2px solid var(--pe-accent)"; e.stopPropagation(); });
  div.addEventListener("dragleave", () => { div.style.borderTop = ""; });
  div.addEventListener("drop", (e) => {
    e.preventDefault(); e.stopPropagation();
    div.style.borderTop = "";
    const data = e.dataTransfer!.getData("text/plain");
    if (!data.startsWith("step:")) return;
    const parts = data.split(":");
    const fromTrack = parseInt(parts[1] ?? "", 10);
    const fromStep = parseInt(parts[2] ?? "", 10);
    if (fromTrack === ti && fromStep !== si) {
      const steps = editorData!.tracks[ti]!.steps;
      const [moved] = steps.splice(fromStep, 1);
      steps.splice(fromStep < si ? si - 1 : si, 0, moved!);
      renderEditorTracks();
    }
  });

  return div;
}

// ── Preview ──

function schedulePreviewRefresh(): void {
  if (!editorData || !config?.onPreview) return;
  if (previewTimer !== null) clearTimeout(previewTimer);
  previewState.loading = true;
  renderEditorPreview();
  previewTimer = setTimeout(requestPreview, PREVIEW_DEBOUNCE_MS);
}

function requestPreview(): void {
  previewTimer = null;
  if (!editorData || !config?.onPreview) return;
  const clone = JSON.parse(JSON.stringify(editorData)) as EditorPreset;
  clone.name = (overlayEl?.querySelector("#pe-name") as HTMLInputElement)?.value.trim() || "__preview__";
  normalizeEditorDurations(clone);
  const nonce = ++previewNonce;
  config.onPreview(nonce, clone);
}

function renderEditorPreview(): void {
  if (!overlayEl) return;
  const statusEl = overlayEl.querySelector("#ep-status");
  const summaryEl = overlayEl.querySelector("#ep-summary");
  const timelineEl = overlayEl.querySelector("#ep-timeline");
  const eventsEl = overlayEl.querySelector("#ep-events");
  if (!statusEl || !summaryEl || !timelineEl || !eventsEl) return;

  statusEl.classList.remove("ep-error");
  summaryEl.textContent = "";
  timelineEl.innerHTML = "";
  eventsEl.innerHTML = "";

  if (!editorData) { statusEl.textContent = "No preview yet."; return; }
  if (previewState.loading) { statusEl.textContent = "Updating preview..."; return; }
  if (previewState.error) { statusEl.textContent = previewState.error; statusEl.classList.add("ep-error"); return; }
  if (!previewState.data) { statusEl.textContent = "No preview data available yet."; return; }

  const preview = previewState.data;
  const endUs = Math.max(preview.total_duration_us, ...preview.events.map((e) => e.actual_time_us + e.transmit_duration_us));
  const delayed = preview.events.filter((e) => e.actual_time_us !== e.requested_time_us).length;
  statusEl.textContent = "Preview reflects the exact serialized RF transmit order and encoder-derived timing.";
  summaryEl.textContent = `${preview.events.length} RF messages. Requested preset span ${fmtUs(preview.total_duration_us)}; serialized RF timeline ${fmtUs(endUs)}. ${delayed} delayed by single-transmitter serialization.`;

  // Timeline bar
  if (preview.events.length > 0) {
    const sorted = preview.events.map((e, i) => ({ e, i })).sort((a, b) => a.e.actual_time_us - b.e.actual_time_us);
    const segCap = endUs / sorted.length / 8;
    let segs = "";
    let cursor = 0;
    for (const { e: ev, i } of sorted) {
      const gap = ev.actual_time_us - cursor;
      if (gap > 0 && endUs > 0) segs += `<div style="flex:${gap}"></div>`;
      const sf = Math.max(1, Math.min(ev.transmit_duration_us, segCap));
      segs += `<button type="button" class="ep-seg" data-pi="${i}" title="${esc(segTitle(ev))}" style="flex:${sf};min-width:1px;background:${TRACK_COLORS[ev.track_index % TRACK_COLORS.length]}"></button>`;
      cursor = ev.actual_time_us + ev.transmit_duration_us;
    }
    if (cursor < endUs) segs += `<div style="flex:${endUs - cursor}"></div>`;
    timelineEl.innerHTML = `<div class="ep-timeline-scale"><span>0ms</span><span>${fmtUs(endUs)}</span></div><div class="ep-timeline-bar">${segs}</div>`;
  }

  // Event table
  eventsEl.innerHTML = `<table class="ep-table"><thead><tr>
    <th>At</th>
    <th title="Requested">&#x1F3AF;</th>
    <th>Delay</th>
    <th title="Track">&#x1F9F5;</th>
    <th title="Step">&#x1F43E;</th>
    <th title="RF TX Duration">&#x23F1;&#xFE0F;</th>
    <th>Collar</th>
    <th title="Mode">&#x1F39B;&#xFE0F;</th>
    <th title="Level">&#x1F4F6;</th>
    <th>Frame</th>
  </tr></thead><tbody>${preview.events.map((ev, i) => `<tr class="ep-row" data-pi="${i}" title="${esc(segTitle(ev))}">
    <td class="mono">${fmtUs(ev.actual_time_us)}</td>
    <td class="mono">${fmtUs(ev.requested_time_us)}</td>
    <td class="mono">${fmtUs(ev.actual_time_us - ev.requested_time_us)}</td>
    <td style="color:${TRACK_COLORS[ev.track_index % TRACK_COLORS.length]}">${ev.track_index + 1}</td>
    <td>${ev.step_index + 1}</td>
    <td class="mono">${fmtUs(ev.transmit_duration_us)}</td>
    <td>${esc(ev.collar_name)}</td>
    <td title="${esc(describeMode(ev.mode))}">${MODE_EMOJI[ev.mode] ?? esc(describeMode(ev.mode))}</td>
    <td>${ev.intensity}</td>
    <td class="mono frame-hex">${ev.raw_hex}</td>
  </tr>`).join("")}</tbody></table>`;

  // Cross-highlight timeline segments <-> table rows
  const all = [
    ...Array.from(timelineEl.querySelectorAll("[data-pi]")),
    ...Array.from(eventsEl.querySelectorAll("[data-pi]")),
  ];
  for (const el of all) {
    const pi = (el as HTMLElement).dataset.pi!;
    el.addEventListener("mouseenter", () => { for (const x of all) (x as HTMLElement).classList.toggle("active", (x as HTMLElement).dataset.pi === pi); });
    el.addEventListener("mouseleave", () => { for (const x of all) (x as HTMLElement).classList.remove("active"); });
    el.addEventListener("focus", () => { for (const x of all) (x as HTMLElement).classList.toggle("active", (x as HTMLElement).dataset.pi === pi); });
    el.addEventListener("blur", () => { for (const x of all) (x as HTMLElement).classList.remove("active"); });
  }
}
