// Shared preset editor — used by both the debug server and central-control.
// Self-contained: injects its own CSS, creates its own overlay DOM.

// ── Types (structurally compatible with both projects' protocol types) ──

export interface EditorCollar {
  name: string;
  collar_id: number;
  channel: number;
}

export interface EditorPresetStep {
  mode: "shock" | "vibrate" | "beep" | "pause";
  intensity: number;
  duration_ms: number;
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
const MODE_EMOJI: Record<string, string> = { shock: "\u26A1", vibrate: "\u3030\uFE0F", beep: "\uD83D\uDD14", pause: "\u23F8\uFE0F" };

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

function normalizeDuration(ms: number): number {
  const v = Number.isFinite(ms) ? ms : DURATION_MIN_MS;
  return Math.round(Math.min(DURATION_MAX_MS, Math.max(DURATION_MIN_MS, v)) / DURATION_STEP_MS) * DURATION_STEP_MS;
}

function formatEditorDuration(ms: number): string {
  const s = normalizeDuration(ms) / 1000;
  return Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`;
}

function normalizeEditorDurations(preset: EditorPreset): void {
  for (const track of preset.tracks) {
    for (const step of track.steps) {
      step.duration_ms = normalizeDuration(step.duration_ms);
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

function segTitle(ev: EditorPresetPreviewEvent): string {
  return `Track ${ev.track_index + 1}, step ${ev.step_index + 1}, ${ev.collar_name}, ${ev.mode}, level ${ev.intensity}, at ${fmtUs(ev.actual_time_us)}, requested ${fmtUs(ev.requested_time_us)}, TX ${fmtUs(ev.transmit_duration_us)}`;
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

let stylesInjected = false;

function injectStyles(): void {
  if (stylesInjected) return;
  stylesInjected = true;
  const style = document.createElement("style");
  style.textContent = `
    .pe-overlay { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 100; overflow-y: auto; }
    .pe-overlay.active { display: flex; justify-content: center; padding: 20px; }
    .pe-box { background: #0d1117; border: 1px solid rgba(255,255,255,0.08); border-radius: 18px; padding: 1.25rem; width: 100%; max-width: 650px; max-height: 90vh; overflow-y: auto; box-shadow: 0 24px 60px rgba(0,0,0,0.5); color: #e6edf3; font-family: "IBM Plex Sans", "Segoe UI", sans-serif; }
    .pe-box h2 { margin: 0 0 1rem; font-size: 1.1rem; }
    .pe-box label span { color: #9da7b3; font-size: 0.88rem; }
    .pe-box input, .pe-box select { border-radius: 10px; border: 1px solid rgba(255,255,255,0.08); background: #1f2630; color: #e6edf3; font: inherit; width: 100%; padding: 0.7rem 0.8rem; box-sizing: border-box; }
    .pe-box button { font: inherit; border-radius: 10px; border: 1px solid rgba(255,255,255,0.08); background: #1f2630; color: #e6edf3; padding: 0.7rem 1rem; cursor: pointer; }
    .pe-box button:hover { border-color: rgba(255,255,255,0.16); }
    .pe-box button.accent { background: linear-gradient(135deg, #1f8bff, #125ec2); }
    .pe-box button.danger { background: linear-gradient(135deg, #cc4d5f, #922c3a); }
    .pe-track { background: #161b22; border-radius: 14px; padding: 0.75rem; margin-bottom: 0.75rem; }
    .pe-track-header { cursor: pointer; user-select: none; display: flex; justify-content: space-between; align-items: center; gap: 0.5rem; }
    .pe-track-header .fold-arrow { transition: transform 0.15s; display: inline-block; }
    .pe-track-header .fold-arrow.open { transform: rotate(90deg); }
    .pe-track-body { display: none; margin-top: 0.5rem; }
    .pe-track-body.open { display: block; }
    .pe-step { display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 0.5rem; padding: 0.6rem; border-radius: 10px; background: rgba(255,255,255,0.03); border: 1px solid rgba(255,255,255,0.08); }
    .pe-step-header { display: flex; gap: 0.5rem; align-items: center; }
    .pe-step-header select { flex: 1; min-width: 0; }
    .pe-slider { display: flex; align-items: center; gap: 0.5rem; }
    .pe-slider .slider-label { width: 5rem; flex-shrink: 0; font-size: 0.84rem; color: #9da7b3; }
    .pe-slider input[type=range] { flex: 1; accent-color: #1f8bff; }
    .pe-slider .slider-val { min-width: 2.5rem; text-align: right; font-size: 0.84rem; color: #9da7b3; }
    .pe-actions { display: flex; gap: 0.6rem; margin-top: 1rem; justify-content: flex-end; }
    .pe-limits { margin-top: 0.75rem; padding: 0.6rem; border-radius: 10px; background: #161b22; border: 1px solid rgba(255,255,255,0.08); font-size: 0.8rem; color: #9da7b3; }
    .pe-preview { margin-top: 1rem; background: #161b22; border-radius: 14px; padding: 0.75rem; }
    .pe-preview summary { cursor: pointer; font-weight: 600; font-size: 0.92rem; }
    .pe-preview .ep-status { font-size: 0.84rem; color: #9da7b3; margin-top: 0.5rem; }
    .pe-preview .ep-status.ep-error { color: #cc4d5f; }
    .pe-preview .ep-summary { font-size: 0.8rem; color: #9da7b3; margin-top: 0.5rem; }
    .ep-timeline { margin-top: 0.5rem; }
    .ep-timeline-scale { display: flex; justify-content: space-between; margin-bottom: 3px; font-size: 0.72rem; color: #9da7b3; font-family: monospace; }
    .ep-timeline-bar { display: flex; border-radius: 6px; overflow: hidden; background: rgba(255,255,255,0.04); border: 1px solid rgba(255,255,255,0.08); height: 20px; }
    .ep-seg { flex: none; height: 100%; border: none; border-radius: 0; cursor: pointer; opacity: 0.85; min-width: 1px; padding: 0; background: none; }
    .ep-seg:hover, .ep-seg.active { opacity: 1; outline: none; }
    .ep-table { width: 100%; border-collapse: collapse; font-size: 0.72rem; margin-top: 0.5rem; }
    .ep-table th, .ep-table td { text-align: left; padding: 3px 5px; border-bottom: 1px solid rgba(255,255,255,0.08); vertical-align: top; white-space: nowrap; }
    .ep-table th { color: #9da7b3; font-weight: 600; }
    .ep-table .hex { white-space: normal; word-break: break-all; }
    .ep-row { transition: background-color 0.12s; }
    .ep-row.active { background: rgba(255,255,255,0.08); }
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
  const nameInput = `<input type="text" id="pe-name" value="${esc(editorData.name)}" placeholder="Preset name">`;

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
    <h2 id="pe-title">${editorOriginalName ? "Edit Preset" : "New Preset"}</h2>
    <div style="margin-bottom:0.75rem"><label><span>Name</span>${nameInput}</label></div>
    <div id="pe-tracks"></div>
    <button id="pe-add-track" style="margin-top:0.5rem">+ Add Track</button>
    ${limitsHtml}
    <details class="pe-preview" id="pe-preview-panel" ${config.onPreview ? "" : "hidden"}>
      <summary>Preview</summary>
      <div class="ep-status" id="ep-status">No preview yet.</div>
      <div class="ep-summary" id="ep-summary"></div>
      <div class="ep-timeline" id="ep-timeline"></div>
      <div id="ep-events"></div>
    </details>
    <div class="pe-actions">
      <button id="pe-cancel">Cancel</button>
      <button class="accent" id="pe-save">Save</button>
    </div>
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
        <span><span class="fold-arrow ${isOpen ? "open" : ""}">&#9654;</span>
          Track: <select data-track-collar="${ti}" onclick="event.stopPropagation()">${collarOpts}</select>
          <span style="color:#9da7b3;font-size:0.84rem">(${track.steps.length} steps)</span>
        </span>
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
  const maxInt = getMaxIntensity(track.collar_name, step.mode);
  const maxDurMs = getMaxDuration(track.collar_name, step.mode);
  const durSec = (normalizeDuration(step.duration_ms) / 1000).toFixed(1);
  const maxDurSec = maxDurMs / 1000;

  const div = document.createElement("div");
  div.className = "pe-step";
  div.draggable = true;

  div.innerHTML = `
    <div class="pe-step-header">
      <select data-step-mode>
        ${(["shock", "vibrate", "beep", "pause"] as const).map((m) => `<option value="${m}" ${step.mode === m ? "selected" : ""}>${m[0]!.toUpperCase() + m.slice(1)}</option>`).join("")}
      </select>
      <button class="danger" data-step-remove style="padding:0.3rem 0.6rem">X</button>
    </div>
    ${noLevel ? "" : `<div class="pe-slider">
      <span class="slider-label">Level</span>
      <input type="range" min="0" max="${maxInt}" value="${Math.min(step.intensity, maxInt)}" data-step-intensity>
      <span class="slider-val" data-intensity-val>${Math.min(step.intensity, maxInt)}</span>
    </div>`}
    <div class="pe-slider">
      <span class="slider-label">Duration</span>
      <input type="range" min="${DURATION_MIN_MS / 1000}" max="${maxDurSec}" step="${DURATION_STEP_MS / 1000}" value="${Math.min(parseFloat(durSec), maxDurSec)}" data-step-duration>
      <span class="slider-val" data-duration-val>${formatEditorDuration(step.duration_ms)}</span>
    </div>`;

  div.querySelector("[data-step-mode]")!.addEventListener("change", (e) => {
    step.mode = (e.target as HTMLSelectElement).value as EditorPresetStep["mode"];
    renderEditorTracks();
  });

  div.querySelector("[data-step-remove]")!.addEventListener("click", () => {
    track.steps.splice(si, 1);
    renderEditorTracks();
  });

  const intSlider = div.querySelector("[data-step-intensity]") as HTMLInputElement | null;
  if (intSlider) {
    intSlider.addEventListener("input", () => {
      step.intensity = parseInt(intSlider.value, 10);
      div.querySelector("[data-intensity-val]")!.textContent = String(step.intensity);
      schedulePreviewRefresh();
    });
  }

  const durSlider = div.querySelector("[data-step-duration]") as HTMLInputElement;
  durSlider.addEventListener("input", () => {
    step.duration_ms = normalizeDuration(Math.round(parseFloat(durSlider.value) * 1000));
    div.querySelector("[data-duration-val]")!.textContent = formatEditorDuration(step.duration_ms);
    schedulePreviewRefresh();
  });

  // Drag-and-drop reordering within same track
  div.addEventListener("dragstart", (e) => {
    e.dataTransfer!.setData("text/plain", `step:${ti}:${si}`);
    div.style.opacity = "0.5";
    e.stopPropagation();
  });
  div.addEventListener("dragend", () => { div.style.opacity = ""; });
  div.addEventListener("dragover", (e) => { e.preventDefault(); div.style.borderTop = "2px solid #1f8bff"; e.stopPropagation(); });
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
  statusEl.textContent = "Preview reflects the exact serialized RF transmit order.";
  summaryEl.textContent = `${preview.events.length} RF messages. Span ${fmtUs(preview.total_duration_us)}; timeline ${fmtUs(endUs)}. ${delayed} delayed.`;

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
    <th>At</th><th title="Requested">&#x1F3AF;</th><th>Delay</th><th title="Track">&#x1F9F5;</th><th title="Step">&#x1F43E;</th>
    <th title="TX">&#x23F1;</th><th>Collar</th><th title="Mode">&#x1F39B;</th><th title="Level">&#x1F4F6;</th><th>Frame</th>
  </tr></thead><tbody>${preview.events.map((ev, i) => `<tr class="ep-row" data-pi="${i}" title="${esc(segTitle(ev))}">
    <td style="font-family:monospace">${fmtUs(ev.actual_time_us)}</td>
    <td style="font-family:monospace">${fmtUs(ev.requested_time_us)}</td>
    <td style="font-family:monospace">${fmtUs(ev.actual_time_us - ev.requested_time_us)}</td>
    <td style="color:${TRACK_COLORS[ev.track_index % TRACK_COLORS.length]}">${ev.track_index + 1}</td>
    <td>${ev.step_index + 1}</td>
    <td style="font-family:monospace">${fmtUs(ev.transmit_duration_us)}</td>
    <td>${esc(ev.collar_name)}</td>
    <td>${MODE_EMOJI[ev.mode] ?? ev.mode}</td>
    <td>${ev.intensity}</td>
    <td class="hex" style="font-family:monospace">${ev.raw_hex}</td>
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
  }
}

function fmtMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}
