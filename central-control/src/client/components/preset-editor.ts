import type { Collar, Preset, PresetStep, PresetPreview, PresetPreviewEvent } from "../../shared/protocol.js";
import type { DevicePermission, CollarPermission } from "../../shared/types.js";
import { esc, formatDuration, fmtUs } from "../utils.js";
import * as ws from "../ws.js";

const DURATION_MIN_MS = 500;
const DURATION_MAX_MS = 10000;
const DURATION_STEP_MS = 500;
const PREVIEW_DEBOUNCE_MS = 200;
const TRACK_COLORS = ["#4ecca3", "#ffc947", "#e94560", "#6fa8ff", "#ff8fab", "#b8de6f"];

type SaveCallback = (preset: Preset) => Promise<void>;

let editorData: Preset | null = null;
let editorOriginalName: string | null = null;
let editorOpenTrack = -1;
let editorCollars: Collar[] = [];
let editorSaveCallback: SaveCallback | null = null;
let editorPermissions: DevicePermission | undefined;
let editorDeviceUuid: string | null = null;
let overlayEl: HTMLDivElement | null = null;

// Preview state
let previewNonce = 0;
let previewTimer: ReturnType<typeof setTimeout> | null = null;
let previewData: { loading: boolean; error: string | null; data: PresetPreview | null } = { loading: false, error: null, data: null };

function normalizeDuration(ms: number): number {
  const v = Number.isFinite(ms) ? ms : DURATION_MIN_MS;
  return Math.round(Math.min(DURATION_MAX_MS, Math.max(DURATION_MIN_MS, v)) / DURATION_STEP_MS) * DURATION_STEP_MS;
}

function normalizePresetDurations(preset: Preset): void {
  for (const track of preset.tracks) {
    for (const step of track.steps) {
      step.duration_ms = normalizeDuration(step.duration_ms);
    }
  }
}

function formatEditorDuration(ms: number): string {
  const s = normalizeDuration(ms) / 1000;
  return Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`;
}

function getMaxIntensity(collarName: string, mode: string): number {
  if (!editorPermissions) return 99;
  const cp = editorPermissions.collars.find((c) => c.collarName === collarName);
  if (!cp) return 99;
  if (mode === "shock" && cp.shock) return cp.shock.maxIntensity;
  if (mode === "vibrate" && cp.vibrate) return cp.vibrate.maxIntensity;
  return 99;
}

function getMaxDuration(collarName: string, mode: string): number {
  if (!editorPermissions) return DURATION_MAX_MS;
  const cp = editorPermissions.collars.find((c) => c.collarName === collarName);
  if (!cp) return DURATION_MAX_MS;
  if (mode === "shock" && cp.shock) return Math.min(cp.shock.maxDurationMs, DURATION_MAX_MS);
  if (mode === "vibrate" && cp.vibrate) return Math.min(cp.vibrate.maxDurationMs, DURATION_MAX_MS);
  if (mode === "beep" && cp.beep) return Math.min(cp.beep.maxDurationMs, DURATION_MAX_MS);
  return DURATION_MAX_MS;
}

export function openPresetEditor(
  preset: Preset | null,
  originalName: string | null,
  collars: Collar[],
  onSave: SaveCallback,
  permissions?: DevicePermission,
  deviceUuid?: string,
): void {
  editorCollars = collars;
  editorSaveCallback = onSave;
  editorPermissions = permissions;
  editorDeviceUuid = deviceUuid ?? null;
  editorOpenTrack = -1;
  previewData = { loading: false, error: null, data: null };
  previewNonce = 0;

  if (preset) {
    editorData = JSON.parse(JSON.stringify(preset));
    editorOriginalName = originalName;
  } else {
    editorData = { name: "", tracks: [] };
    editorOriginalName = null;
  }
  normalizePresetDurations(editorData!);
  showOverlay();
}

// Called from app.ts when a preset_preview WS message arrives
export function handlePresetPreview(nonce: number, preview: PresetPreview | null, error: string | null): void {
  if (!editorData || nonce !== previewNonce) return;
  previewData = { loading: false, error, data: preview };
  renderPreviewPanel();
}

function showOverlay(): void {
  if (!overlayEl) {
    overlayEl = document.createElement("div");
    overlayEl.className = "fixed inset-0 bg-black/70 z-50 flex justify-center p-5 overflow-y-auto";
    document.body.appendChild(overlayEl);
  }
  overlayEl.style.display = "flex";
  renderEditor();
}

function hideOverlay(): void {
  if (overlayEl) {
    overlayEl.style.display = "none";
  }
  if (previewTimer) clearTimeout(previewTimer);
  previewTimer = null;
  editorData = null;
  editorOriginalName = null;
  editorSaveCallback = null;
  editorDeviceUuid = null;
}

function renderEditor(): void {
  if (!overlayEl || !editorData) return;

  overlayEl.innerHTML = `
    <div class="bg-gray-950 border border-gray-800 rounded-2xl p-6 w-full max-w-2xl max-h-[90vh] overflow-y-auto shadow-2xl">
      <h2 class="text-xl font-bold mb-4">${editorOriginalName ? "Edit Preset" : "New Preset"}</h2>
      <label class="flex flex-col gap-1 mb-4">
        <span class="text-sm text-gray-400">Name</span>
        <input type="text" id="pe-name" value="${esc(editorData.name)}" placeholder="Preset name"
          class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
      </label>

      <div id="pe-tracks"></div>

      <button id="pe-add-track" class="mt-3 bg-gray-800 hover:bg-gray-700 text-gray-300 text-sm rounded-lg px-4 py-2 border border-gray-700">
        + Add Track
      </button>

      ${editorPermissions ? `
        <div class="mt-4 bg-gray-900 border border-gray-700 rounded-lg p-3">
          <div class="text-xs text-gray-500 font-semibold mb-1">Your Limits</div>
          ${editorPermissions.collars.map((cp) => {
            const modes: string[] = [];
            if (cp.shock) modes.push(`shock(max ${cp.shock.maxIntensity}, ${formatDuration(cp.shock.maxDurationMs)})`);
            if (cp.vibrate) modes.push(`vibrate(max ${cp.vibrate.maxIntensity}, ${formatDuration(cp.vibrate.maxDurationMs)})`);
            if (cp.beep) modes.push(`beep(${formatDuration(cp.beep.maxDurationMs)})`);
            return `<div class="text-xs text-gray-400">${esc(cp.collarName)}: ${modes.join(", ")}</div>`;
          }).join("")}
        </div>` : ""}

      <details class="mt-4 bg-gray-900 border border-gray-700 rounded-lg p-3" id="pe-preview-panel" ${editorDeviceUuid ? "" : "hidden"}>
        <summary class="cursor-pointer font-semibold text-sm">Preview</summary>
        <div id="pe-preview-status" class="text-xs text-gray-500 mt-2">No preview yet.</div>
        <div id="pe-preview-summary" class="text-xs text-gray-500 mt-1"></div>
        <div id="pe-preview-timeline" class="mt-2"></div>
        <div id="pe-preview-events"></div>
      </details>

      <div class="flex gap-3 mt-6 justify-end">
        <button id="pe-cancel" class="bg-gray-800 hover:bg-gray-700 text-gray-300 rounded-lg px-6 py-2 border border-gray-700">Cancel</button>
        <button id="pe-save" class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-6 py-2">Save</button>
      </div>
    </div>`;

  renderTracks();
  bindEditorEvents();
}

function renderTracks(): void {
  if (!editorData || !overlayEl) return;
  const container = overlayEl.querySelector("#pe-tracks")!;
  container.innerHTML = "";

  editorData.tracks.forEach((track, ti) => {
    const isOpen = ti === editorOpenTrack;
    const div = document.createElement("div");
    div.className = "bg-gray-900 border border-gray-700 rounded-xl p-3 mb-3";

    const availableCollars = editorPermissions
      ? editorCollars.filter((c) => editorPermissions!.collars.some((cp) => cp.collarName === c.name))
      : editorCollars;

    const collarOpts = availableCollars
      .map((c) => `<option value="${esc(c.name)}" ${c.name === track.collar_name ? "selected" : ""}>${esc(c.name)}</option>`)
      .join("");

    div.innerHTML = `
      <div class="flex items-center justify-between cursor-pointer select-none pe-track-header" data-ti="${ti}">
        <div class="flex items-center gap-2">
          <span class="text-xs transition-transform ${isOpen ? "rotate-90" : ""}">&#9654;</span>
          <span class="text-sm">Track:</span>
          <select class="pe-track-collar bg-gray-800 border border-gray-700 rounded px-2 py-1 text-sm" data-ti="${ti}">
            ${collarOpts}
          </select>
          <span class="text-xs text-gray-500">(${track.steps.length} steps)</span>
        </div>
        <button class="pe-remove-track text-red-400 hover:text-red-300 text-sm px-2" data-ti="${ti}">X</button>
      </div>
      <div class="pe-track-body mt-2 ${isOpen ? "" : "hidden"}" data-ti="${ti}"></div>`;

    const body = div.querySelector(".pe-track-body")!;
    track.steps.forEach((step, si) => {
      body.appendChild(renderStep(track, ti, si));
    });

    const addStepBtn = document.createElement("button");
    addStepBtn.textContent = "+ Step";
    addStepBtn.className = "mt-2 text-sm text-gray-400 hover:text-white";
    addStepBtn.addEventListener("click", () => {
      track.steps.push({ mode: "vibrate", intensity: 30, duration_ms: 1000 });
      renderTracks();
    });
    body.appendChild(addStepBtn);

    div.querySelector(".pe-track-header")!.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).closest("select, button")) return;
      editorOpenTrack = editorOpenTrack === ti ? -1 : ti;
      renderTracks();
    });

    div.querySelector(".pe-track-collar")!.addEventListener("change", (e) => {
      track.collar_name = (e.target as HTMLSelectElement).value;
      schedulePreviewRefresh();
    });

    div.querySelector(".pe-remove-track")!.addEventListener("click", (e) => {
      e.stopPropagation();
      editorData!.tracks.splice(ti, 1);
      if (editorOpenTrack === ti) editorOpenTrack = -1;
      else if (editorOpenTrack > ti) editorOpenTrack--;
      renderTracks();
    });

    container.appendChild(div);
  });

  schedulePreviewRefresh();
}

function renderStep(track: { steps: PresetStep[]; collar_name: string }, ti: number, si: number): HTMLElement {
  const step = track.steps[si]!;
  const noLevel = step.mode === "pause" || step.mode === "beep";
  const maxInt = getMaxIntensity(track.collar_name, step.mode);
  const maxDur = getMaxDuration(track.collar_name, step.mode);
  const durSec = (normalizeDuration(step.duration_ms) / 1000).toFixed(1);
  const maxDurSec = Math.min(maxDur, DURATION_MAX_MS) / 1000;

  const div = document.createElement("div");
  div.className = "bg-gray-800 border border-gray-700 rounded-lg p-3 mb-2";

  div.innerHTML = `
    <div class="flex items-center gap-2 mb-2">
      <select class="pe-step-mode bg-gray-700 border border-gray-600 rounded px-2 py-1 text-sm flex-1">
        ${(["shock", "vibrate", "beep", "pause"] as const).map((m) => `<option value="${m}" ${step.mode === m ? "selected" : ""}>${m[0]!.toUpperCase() + m.slice(1)}</option>`).join("")}
      </select>
      <button class="pe-step-remove text-red-400 hover:text-red-300 text-sm px-2">X</button>
    </div>
    ${noLevel ? "" : `
      <div class="flex items-center gap-2 mb-1">
        <span class="text-xs text-gray-500 w-14">Level</span>
        <input type="range" min="0" max="${maxInt}" value="${Math.min(step.intensity, maxInt)}" class="pe-step-intensity flex-1">
        <span class="pe-intensity-val text-xs text-gray-400 w-8 text-right">${Math.min(step.intensity, maxInt)}</span>
      </div>`}
    <div class="flex items-center gap-2">
      <span class="text-xs text-gray-500 w-14">Duration</span>
      <input type="range" min="${DURATION_MIN_MS / 1000}" max="${maxDurSec}" step="${DURATION_STEP_MS / 1000}"
        value="${Math.min(parseFloat(durSec), maxDurSec)}" class="pe-step-duration flex-1">
      <span class="pe-duration-val text-xs text-gray-400 w-10 text-right">${formatEditorDuration(step.duration_ms)}</span>
    </div>`;

  div.querySelector(".pe-step-mode")!.addEventListener("change", (e) => {
    step.mode = (e.target as HTMLSelectElement).value as PresetStep["mode"];
    renderTracks();
  });

  div.querySelector(".pe-step-remove")!.addEventListener("click", () => {
    track.steps.splice(si, 1);
    renderTracks();
  });

  const intSlider = div.querySelector(".pe-step-intensity") as HTMLInputElement | null;
  if (intSlider) {
    intSlider.addEventListener("input", () => {
      step.intensity = parseInt(intSlider.value, 10);
      div.querySelector(".pe-intensity-val")!.textContent = String(step.intensity);
      schedulePreviewRefresh();
    });
  }

  const durSlider = div.querySelector(".pe-step-duration") as HTMLInputElement;
  durSlider.addEventListener("input", () => {
    step.duration_ms = normalizeDuration(Math.round(parseFloat(durSlider.value) * 1000));
    div.querySelector(".pe-duration-val")!.textContent = formatEditorDuration(step.duration_ms);
    schedulePreviewRefresh();
  });

  // Drag and drop
  div.draggable = true;
  div.addEventListener("dragstart", (e) => {
    e.dataTransfer!.setData("text/plain", `step:${ti}:${si}`);
    div.style.opacity = "0.5";
    e.stopPropagation();
  });
  div.addEventListener("dragend", () => { div.style.opacity = ""; });
  div.addEventListener("dragover", (e) => { e.preventDefault(); div.classList.add("drag-over"); e.stopPropagation(); });
  div.addEventListener("dragleave", () => { div.classList.remove("drag-over"); });
  div.addEventListener("drop", (e) => {
    e.preventDefault(); e.stopPropagation();
    div.classList.remove("drag-over");
    const data = e.dataTransfer!.getData("text/plain");
    if (!data.startsWith("step:")) return;
    const parts = data.split(":");
    const fromTrack = parseInt(parts[1] ?? "", 10);
    const fromStep = parseInt(parts[2] ?? "", 10);
    if (fromTrack === ti && fromStep !== si) {
      const steps = editorData!.tracks[ti]!.steps;
      const [moved] = steps.splice(fromStep, 1);
      steps.splice(fromStep < si ? si - 1 : si, 0, moved!);
      renderTracks();
    }
  });

  return div;
}

// ── Preview ──

function schedulePreviewRefresh(): void {
  if (!editorData || !editorDeviceUuid) return;
  if (previewTimer !== null) clearTimeout(previewTimer);
  previewData.loading = true;
  renderPreviewPanel();
  previewTimer = setTimeout(requestPreview, PREVIEW_DEBOUNCE_MS);
}

function requestPreview(): void {
  previewTimer = null;
  if (!editorData || !editorDeviceUuid) return;
  if (!ws.isConnected()) {
    previewData = { loading: false, error: "Preview unavailable while disconnected.", data: null };
    renderPreviewPanel();
    return;
  }
  const clone = JSON.parse(JSON.stringify(editorData)) as Preset;
  clone.name = (overlayEl?.querySelector("#pe-name") as HTMLInputElement)?.value.trim() || "__preview__";
  normalizePresetDurations(clone);
  const nonce = ++previewNonce;
  ws.sendDeviceCommand(editorDeviceUuid, { type: "preview_preset", nonce, preset: clone });
}

function renderPreviewPanel(): void {
  if (!overlayEl) return;
  const statusEl = overlayEl.querySelector("#pe-preview-status");
  const summaryEl = overlayEl.querySelector("#pe-preview-summary");
  const timelineEl = overlayEl.querySelector("#pe-preview-timeline");
  const eventsEl = overlayEl.querySelector("#pe-preview-events");
  if (!statusEl || !summaryEl || !timelineEl || !eventsEl) return;

  statusEl.classList.remove("text-red-400");
  summaryEl.textContent = "";
  timelineEl.innerHTML = "";
  eventsEl.innerHTML = "";

  if (!editorData) { statusEl.textContent = "No preview yet."; return; }
  if (!editorDeviceUuid) { statusEl.textContent = "Preview requires a connected device."; return; }
  if (previewData.loading) { statusEl.textContent = "Updating preview..."; return; }
  if (previewData.error) { statusEl.textContent = previewData.error; statusEl.classList.add("text-red-400"); return; }
  if (!previewData.data) { statusEl.textContent = "No preview data yet."; return; }

  const preview = previewData.data;
  const endUs = Math.max(preview.total_duration_us, ...preview.events.map((e) => e.actual_time_us + e.transmit_duration_us));
  const delayed = preview.events.filter((e) => e.actual_time_us !== e.requested_time_us).length;
  statusEl.textContent = "Preview reflects the exact serialized RF transmit order.";
  summaryEl.textContent = `${preview.events.length} RF messages. Span ${fmtUs(preview.total_duration_us)}; timeline ${fmtUs(endUs)}. ${delayed} delayed.`;

  // Timeline bar
  if (preview.events.length > 0) {
    const sorted = preview.events.map((e, i) => ({ e, i })).sort((a, b) => a.e.actual_time_us - b.e.actual_time_us);
    let segs = "";
    let cursor = 0;
    for (const { e: ev, i } of sorted) {
      const gap = ev.actual_time_us - cursor;
      if (gap > 0) segs += `<div style="flex:${gap}"></div>`;
      const sf = Math.max(1, ev.transmit_duration_us);
      segs += `<div class="h-full" style="flex:${sf};min-width:1px;background:${TRACK_COLORS[ev.track_index % TRACK_COLORS.length]}" title="T${ev.track_index + 1} S${ev.step_index + 1} ${ev.collar_name} ${ev.mode} @ ${fmtUs(ev.actual_time_us)}"></div>`;
      cursor = ev.actual_time_us + ev.transmit_duration_us;
    }
    if (cursor < endUs) segs += `<div style="flex:${endUs - cursor}"></div>`;
    timelineEl.innerHTML = `
      <div class="flex justify-between text-[10px] text-gray-500 font-mono mb-0.5"><span>0ms</span><span>${fmtUs(endUs)}</span></div>
      <div class="flex rounded overflow-hidden bg-gray-800 border border-gray-700 h-4">${segs}</div>`;
  }

  // Event table
  eventsEl.innerHTML = `<table class="w-full text-[11px] mt-2 border-collapse">
    <thead><tr class="text-gray-500">
      <th class="text-left p-1">At</th><th class="text-left p-1">Track</th><th class="text-left p-1">Step</th>
      <th class="text-left p-1">Collar</th><th class="text-left p-1">Mode</th><th class="text-left p-1">Level</th><th class="text-left p-1">TX</th>
    </tr></thead>
    <tbody>${preview.events.map((ev) => `<tr class="border-t border-gray-800">
      <td class="p-1 font-mono">${fmtUs(ev.actual_time_us)}</td>
      <td class="p-1" style="color:${TRACK_COLORS[ev.track_index % TRACK_COLORS.length]}">${ev.track_index + 1}</td>
      <td class="p-1">${ev.step_index + 1}</td>
      <td class="p-1">${esc(ev.collar_name)}</td>
      <td class="p-1">${ev.mode}</td>
      <td class="p-1">${ev.intensity}</td>
      <td class="p-1 font-mono">${fmtUs(ev.transmit_duration_us)}</td>
    </tr>`).join("")}</tbody></table>`;
}

function bindEditorEvents(): void {
  if (!overlayEl) return;

  overlayEl.querySelector("#pe-name")?.addEventListener("input", (e) => {
    if (editorData) editorData.name = (e.target as HTMLInputElement).value;
    schedulePreviewRefresh();
  });

  overlayEl.querySelector("#pe-add-track")?.addEventListener("click", () => {
    if (!editorData) return;
    const availableCollars = editorPermissions
      ? editorCollars.filter((c) => editorPermissions!.collars.some((cp) => cp.collarName === c.name))
      : editorCollars;
    const defaultCollar = availableCollars[0]?.name ?? "";
    editorData.tracks.push({ collar_name: defaultCollar, steps: [] });
    editorOpenTrack = editorData.tracks.length - 1;
    renderTracks();
  });

  overlayEl.querySelector("#pe-cancel")?.addEventListener("click", hideOverlay);

  overlayEl.querySelector("#pe-save")?.addEventListener("click", async () => {
    if (!editorData || !editorSaveCallback) return;
    editorData.name = ((overlayEl!.querySelector("#pe-name") as HTMLInputElement)?.value ?? "").trim();
    if (!editorData.name) {
      alert("Preset name required");
      return;
    }
    normalizePresetDurations(editorData);
    try {
      await editorSaveCallback(editorData);
      hideOverlay();
    } catch (err) {
      alert(err instanceof Error ? err.message : "Failed to save preset");
    }
  });
}
