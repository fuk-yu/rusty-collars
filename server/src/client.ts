import type {
  BoardCommand,
  BoardSnapshot,
  Collar,
  CommandMode,
  EventLogEntry,
  Preset,
  PresetPreview,
  PresetPreviewEvent,
  PresetPreviewMessage,
  SnapshotMessage,
  UiIncomingMessage,
  UiOutgoingMessage,
} from "./protocol.js";

// --- Constants ---

const PRESET_DURATION_MIN_MS = 500;
const PRESET_DURATION_MAX_MS = 10000;
const PRESET_DURATION_STEP_MS = 500;
const PRESET_PREVIEW_DEBOUNCE_MS = 150;
const TRACK_COLORS = ["#4ecca3", "#ffc947", "#e94560", "#6fa8ff", "#ff8fab", "#b8de6f"];
const MODE_EMOJI: Record<string, string> = { shock: "\u26A1", vibrate: "\u3030\uFE0F", beep: "\uD83D\uDD14", pause: "\u23F8\uFE0F" };

// --- State ---

interface FormState {
  selectedBoardId: string | null;
  collarEditor: { originalName: string; name: string; collarId: string; channel: string };
  quickAction: { collarName: string; mode: CommandMode; intensity: string; durationMs: string; presetName: string };
  banner: { kind: "info" | "error"; text: string } | null;
}

const state: FormState = {
  selectedBoardId: null,
  collarEditor: { originalName: "", name: "", collarId: "", channel: "0" },
  quickAction: { collarName: "", mode: "vibrate", intensity: "30", durationMs: "1500", presetName: "" },
  banner: null,
};

let socket: WebSocket | null = null;
let snapshot: SnapshotMessage = { type: "snapshot", server_started_at_ms: Date.now(), boards: [] };

// Editor state
let editorData: Preset | null = null;
let editorOriginalName: string | null = null;
let editorOpenTrack = -1;
let editorPreview: { loading: boolean; error: string | null; data: PresetPreview | null } = { loading: false, error: null, data: null };
let editorPreviewNonce = 0;
let editorPreviewTimer: ReturnType<typeof setTimeout> | null = null;

// --- Boot ---

const appRoot = document.getElementById("app");
if (!appRoot) throw new Error("Missing #app");
const root: HTMLElement = appRoot;

connect();
render();
bindEditorEvents();

// --- WebSocket ---

function connect(): void {
  const protocol = location.protocol === "https:" ? "wss://" : "ws://";
  socket = new WebSocket(`${protocol}${location.host}/ui`);
  socket.addEventListener("message", (event) => handleMessage(JSON.parse(event.data) as UiOutgoingMessage));
  socket.addEventListener("close", () => {
    state.banner = { kind: "error", text: "UI socket disconnected, retrying..." };
    render();
    setTimeout(connect, 1_000);
  });
}

// --- Delegated event handlers ---

root.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const el = target.closest<HTMLElement>("[data-action]");
  if (!el) return;
  const action = el.dataset.action;

  switch (action) {
    case "select-board": setSelectedBoardId(el.dataset.boardId ?? null); break;
    case "save-collar": saveCollar(); break;
    case "reset-collar": resetCollarEditor(); break;
    case "edit-collar": editCollar(el.dataset.collarName ?? ""); break;
    case "delete-collar": deleteCollar(el.dataset.collarName ?? ""); break;
    case "run-action": sendQuickAction("run_action"); break;
    case "start-action": sendQuickAction("start_action"); break;
    case "stop-action": sendQuickAction("stop_action"); break;
    case "stop-all": sendBoardCommand({ type: "stop_all" }); break;
    case "run-preset": runPreset(el.dataset.presetName ?? state.quickAction.presetName); break;
    case "stop-preset": sendBoardCommand({ type: "stop_preset" }); break;
    case "ping-board": pingBoard(); break;
    case "new-preset": openPresetEditor(null); break;
    case "edit-preset": openPresetEditor(el.dataset.presetName ?? ""); break;
    case "delete-preset": deletePreset(el.dataset.presetName ?? ""); break;
    case "move-preset-up": reorderPreset(el.dataset.presetName ?? "", -1); break;
    case "move-preset-down": reorderPreset(el.dataset.presetName ?? "", 1); break;
    default: break;
  }
});

root.addEventListener("input", (event) => {
  const target = event.target;
  if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement)) return;
  switch (target.dataset.field) {
    case "collar-name": state.collarEditor.name = target.value; break;
    case "collar-id": state.collarEditor.collarId = target.value; break;
    case "collar-channel": state.collarEditor.channel = target.value; break;
    case "action-collar": state.quickAction.collarName = target.value; break;
    case "action-mode": state.quickAction.mode = target.value as CommandMode; break;
    case "action-intensity": state.quickAction.intensity = target.value; break;
    case "action-duration": state.quickAction.durationMs = target.value; break;
    case "action-preset": state.quickAction.presetName = target.value; break;
    default: return;
  }
});

// --- Message handling ---

function handleMessage(message: UiOutgoingMessage): void {
  switch (message.type) {
    case "snapshot":
      snapshot = message;
      if (!state.selectedBoardId || !snapshot.boards.some((b) => b.id === state.selectedBoardId)) {
        setSelectedBoardId(snapshot.boards[0]?.id ?? null);
      } else {
        normalizeState();
      }
      render();
      break;
    case "preset_preview":
      handlePresetPreview(message);
      break;
    case "ui_error":
      state.banner = { kind: "error", text: message.message };
      render();
      break;
    case "ui_info":
      state.banner = { kind: "info", text: message.message };
      render();
      break;
    default:
      break;
  }
}

// --- State helpers ---

function selectedBoard(): BoardSnapshot | null {
  return snapshot.boards.find((b) => b.id === state.selectedBoardId) ?? null;
}

function setSelectedBoardId(boardId: string | null): void {
  state.selectedBoardId = boardId;
  normalizeState();
  render();
}

function normalizeState(): void {
  const board = selectedBoard();
  if (!board?.state) {
    state.quickAction.collarName = "";
    state.quickAction.presetName = "";
    return;
  }
  const collarNames = board.state.collars.map((c) => c.name);
  if (!collarNames.includes(state.quickAction.collarName)) state.quickAction.collarName = collarNames[0] ?? "";
  const presetNames = board.state.presets.map((p) => p.name);
  if (!presetNames.includes(state.quickAction.presetName)) state.quickAction.presetName = presetNames[0] ?? "";
}

// --- Main render ---

function render(): void {
  const board = selectedBoard();
  root.innerHTML = `
    <div class="shell">
      <aside class="sidebar">
        <div class="sidebar-header"><h1>Boards</h1><div class="meta">${snapshot.boards.length} connected</div></div>
        <div class="board-list">${snapshot.boards.length === 0 ? `<div class="empty">No boards connected</div>` : snapshot.boards.map(renderBoardListItem).join("")}</div>
      </aside>
      <main class="main">
        ${state.banner ? `<div class="banner banner-${state.banner.kind}">${esc(state.banner.text)}</div>` : ""}
        ${board ? renderBoard(board) : `<section class="panel empty-panel"><h2>No board selected</h2><p>Point a device at <code>/device</code> and it will appear here.</p></section>`}
      </main>
    </div>`;
}

function renderBoardListItem(board: BoardSnapshot): string {
  const active = board.id === state.selectedBoardId ? " board-card-active" : "";
  return `<button class="board-card${active}" data-action="select-board" data-board-id="${esc(board.id)}">
    <div class="board-card-top"><strong>${esc(board.id)}</strong><span>${board.rtt_ms == null ? "..." : `${board.rtt_ms}ms`}</span></div>
    <div class="board-card-body"><div>${esc(board.state?.app_version ?? "awaiting state")}</div><div>${esc(board.peer)}</div>
    <div>${board.state ? `${board.state.collars.length} collars, ${board.state.presets.length} presets` : "no state yet"}</div></div>
  </button>`;
}

function renderBoard(board: BoardSnapshot): string {
  const deviceId = board.state?.device_id;
  return `
    <section class="panel">
      <div class="panel-header">
        <div><h2>${esc(board.id)}</h2><div class="meta">${esc(board.peer)} · ${esc(board.path)}${deviceId ? ` · ${esc(deviceId)}` : ""}</div></div>
        <div class="toolbar"><button data-action="ping-board">Ping</button><button class="danger" data-action="stop-all">STOP ALL</button></div>
      </div>
      <div class="stats">
        <div><span>Firmware</span><strong>${esc(board.state?.app_version ?? "unknown")}</strong></div>
        <div><span>RTT</span><strong>${board.rtt_ms == null ? "n/a" : `${board.rtt_ms}ms`}</strong></div>
        <div><span>Preset</span><strong>${esc(board.state?.preset_running ?? "idle")}</strong></div>
        <div><span>Lockout</span><strong>${formatDuration(board.state?.rf_lockout_remaining_ms ?? 0)}</strong></div>
      </div>
      ${board.last_error ? `<div class="error-box">${esc(board.last_error)}</div>` : ""}
    </section>
    ${renderQuickActions(board)}
    ${renderCollars(board)}
    ${renderPresets(board)}
    ${renderEventLog(board)}`;
}

function renderQuickActions(board: BoardSnapshot): string {
  const collars = board.state?.collars ?? [];
  const presets = board.state?.presets ?? [];
  const intDisabled = state.quickAction.mode === "beep" ? "disabled" : "";
  return `<section class="panel">
    <div class="panel-header"><h2>Quick Actions</h2><div class="toolbar"><button data-action="stop-preset">Stop Preset</button></div></div>
    <div class="grid grid-5">
      <label><span>Collar</span><select data-field="action-collar">${collars.map((c) => `<option value="${esc(c.name)}" ${c.name === state.quickAction.collarName ? "selected" : ""}>${esc(c.name)}</option>`).join("")}</select></label>
      <label><span>Mode</span><select data-field="action-mode">${(["shock", "vibrate", "beep"] as const).map((m) => `<option value="${m}" ${state.quickAction.mode === m ? "selected" : ""}>${m}</option>`).join("")}</select></label>
      <label><span>Intensity</span><input type="number" min="0" max="99" data-field="action-intensity" value="${esc(state.quickAction.intensity)}" ${intDisabled}></label>
      <label><span>Duration ms</span><input type="number" min="1" max="30000" data-field="action-duration" value="${esc(state.quickAction.durationMs)}"></label>
      <div class="button-stack"><button class="accent" data-action="run-action">Run Timed</button><button data-action="start-action">Start Held</button><button data-action="stop-action">Stop Held</button></div>
    </div>
    <div class="grid grid-2">
      <label><span>Preset</span><select data-field="action-preset">${presets.map((p) => `<option value="${esc(p.name)}" ${p.name === state.quickAction.presetName ? "selected" : ""}>${esc(p.name)}</option>`).join("")}</select></label>
      <div class="button-row"><button class="accent" data-action="run-preset">Run Preset</button></div>
    </div>
  </section>`;
}

function renderCollars(board: BoardSnapshot): string {
  const collars = board.state?.collars ?? [];
  return `<section class="panel">
    <div class="panel-header"><h2>Collars</h2></div>
    <div class="grid grid-4">
      <label><span>Name</span><input type="text" data-field="collar-name" value="${esc(state.collarEditor.name)}"></label>
      <label><span>ID</span><input type="text" data-field="collar-id" value="${esc(state.collarEditor.collarId)}" placeholder="39802 or 0x9B7A"></label>
      <label><span>Channel</span><select data-field="collar-channel">${[0, 1, 2].map((ch) => `<option value="${ch}" ${String(ch) === state.collarEditor.channel ? "selected" : ""}>CH${ch + 1}</option>`).join("")}</select></label>
      <div class="button-stack"><button class="accent" data-action="save-collar">${state.collarEditor.originalName ? "Update" : "Add"} Collar</button><button data-action="reset-collar">Reset</button></div>
    </div>
    <table class="table"><thead><tr><th>Name</th><th>ID</th><th>Channel</th><th>Actions</th></tr></thead>
    <tbody>${collars.map((c) => `<tr><td>${esc(c.name)}</td><td>${esc(formatCollarId(c.collar_id))}</td><td>CH${c.channel + 1}</td><td class="row-actions"><button data-action="edit-collar" data-collar-name="${esc(c.name)}">Edit</button><button class="danger" data-action="delete-collar" data-collar-name="${esc(c.name)}">Delete</button></td></tr>`).join("") || `<tr><td colspan="4" class="empty-row">No collars configured</td></tr>`}</tbody></table>
  </section>`;
}

function renderPresets(board: BoardSnapshot): string {
  const presets = board.state?.presets ?? [];
  return `<section class="panel">
    <div class="panel-header"><h2>Presets</h2><div class="toolbar"><button data-action="new-preset">+ New Preset</button></div></div>
    <div class="preset-list">
      ${presets.map((preset, i) => `<div class="preset-card">
        <div class="preset-card-top"><strong>${esc(preset.name)}</strong><span>${preset.tracks.length} track${preset.tracks.length === 1 ? "" : "s"}</span></div>
        <div class="preset-card-body">${esc(describePreset(preset))}</div>
        <div class="row-actions">
          <button class="accent" data-action="run-preset" data-preset-name="${esc(preset.name)}">Run</button>
          <button data-action="edit-preset" data-preset-name="${esc(preset.name)}">Edit</button>
          <button data-action="move-preset-up" data-preset-name="${esc(preset.name)}" ${i === 0 ? "disabled" : ""}>Up</button>
          <button data-action="move-preset-down" data-preset-name="${esc(preset.name)}" ${i === presets.length - 1 ? "disabled" : ""}>Down</button>
          <button class="danger" data-action="delete-preset" data-preset-name="${esc(preset.name)}">Delete</button>
        </div>
      </div>`).join("") || `<div class="empty">No presets configured</div>`}
    </div>
  </section>`;
}

function renderEventLog(board: BoardSnapshot): string {
  return `<section class="panel">
    <div class="panel-header"><h2>Event Log</h2><div class="meta">${board.event_log_enabled ? `${board.event_log_events.length}/100 entries` : "disabled on device"}</div></div>
    <div class="event-list">
      ${board.event_log_events.length === 0 ? `<div class="empty">No events available</div>` : [...board.event_log_events].reverse().map((e) => `<div class="event-item"><span class="event-time">${esc(formatTimestamp(e.unix_ms, e.monotonic_ms))}</span><span class="event-source">${esc(e.source)}</span><span>${esc(describeEvent(e))}</span></div>`).join("")}
    </div>
  </section>`;
}

// =============================================================================
// Preset Visual Editor (persistent overlay, direct DOM manipulation)
// =============================================================================

function bindEditorEvents(): void {
  const nameInput = document.getElementById("editor-name") as HTMLInputElement;
  nameInput.addEventListener("input", () => { if (editorData) { editorData.name = nameInput.value; schedulePreviewRefresh(); } });
  document.getElementById("editor-add-track")!.addEventListener("click", editorAddTrack);
  document.getElementById("editor-cancel")!.addEventListener("click", closePresetEditor);
  document.getElementById("editor-save")!.addEventListener("click", editorSave);
}

function openPresetEditor(nameOrNull: string | null): void {
  const board = selectedBoard();
  if (nameOrNull) {
    const preset = board?.state?.presets.find((p) => p.name === nameOrNull);
    if (!preset) { showError(`Unknown preset: ${nameOrNull}`); return; }
    editorData = JSON.parse(JSON.stringify(preset)) as Preset;
    editorOriginalName = preset.name;
    document.getElementById("editor-title")!.textContent = "Edit Preset";
  } else {
    editorData = { name: "", tracks: [] };
    editorOriginalName = null;
    document.getElementById("editor-title")!.textContent = "New Preset";
  }
  editorOpenTrack = -1;
  resetPreview();
  normalizeEditorDurations(editorData);
  (document.getElementById("editor-name") as HTMLInputElement).value = editorData.name;
  renderEditorTracks();
  document.getElementById("editor-overlay")!.classList.add("active");
}

function closePresetEditor(): void {
  document.getElementById("editor-overlay")!.classList.remove("active");
  editorData = null;
  editorOriginalName = null;
  editorOpenTrack = -1;
  resetPreview();
}

function editorSave(): void {
  if (!editorData) return;
  editorData.name = (document.getElementById("editor-name") as HTMLInputElement).value.trim();
  if (!editorData.name) { alert("Preset name required"); return; }
  normalizeEditorDurations(editorData);
  sendBoardCommand({ type: "save_preset", original_name: editorOriginalName, preset: editorData });
  closePresetEditor();
}

function editorAddTrack(): void {
  if (!editorData) return;
  const board = selectedBoard();
  const defaultCollar = board?.state?.collars[0]?.name ?? "";
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

// --- Track/Step Rendering ---

function renderEditorTracks(): void {
  if (!editorData) return;
  const board = selectedBoard();
  const collars: Collar[] = board?.state?.collars ?? [];
  const container = document.getElementById("editor-tracks")!;
  container.innerHTML = "";

  editorData.tracks.forEach((track, ti) => {
    const isOpen = ti === editorOpenTrack;
    const div = document.createElement("div");
    div.className = "editor-track";

    const collarOpts = collars.map((c) =>
      `<option value="${esc(c.name)}" ${c.name === track.collar_name ? "selected" : ""}>${esc(c.name)}</option>`
    ).join("");

    div.innerHTML = `
      <div class="editor-track-header" data-track-toggle="${ti}">
        <span><span class="fold-arrow ${isOpen ? "open" : ""}">&#9654;</span>
          Track: <select data-track-collar="${ti}" onclick="event.stopPropagation()">${collarOpts}</select>
          <span style="color:var(--muted);font-size:0.84rem">(${track.steps.length} steps)</span>
        </span>
        <button class="danger" data-track-remove="${ti}" style="padding:0.3rem 0.6rem">X</button>
      </div>
      <div class="editor-track-body ${isOpen ? "open" : ""}" id="editor-track-body-${ti}"></div>`;

    // Track header click → toggle fold
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

    const body = div.querySelector(".editor-track-body")!;
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

function renderEditorStep(track: { steps: Preset["tracks"][0]["steps"] }, ti: number, si: number): HTMLElement {
  const step = track.steps[si]!;
  const noLevel = step.mode === "pause" || step.mode === "beep";
  const durSec = (normalizeDuration(step.duration_ms) / 1000).toFixed(1);

  const div = document.createElement("div");
  div.className = "editor-step";
  div.draggable = true;

  div.innerHTML = `
    <div class="editor-step-header">
      <select data-step-mode>
        ${(["shock", "vibrate", "beep", "pause"] as const).map((m) => `<option value="${m}" ${step.mode === m ? "selected" : ""}>${m[0]!.toUpperCase() + m.slice(1)}</option>`).join("")}
      </select>
      <button class="danger" data-step-remove style="padding:0.3rem 0.6rem">X</button>
    </div>
    ${noLevel ? "" : `<div class="editor-slider">
      <span class="slider-label">Level</span>
      <input type="range" min="0" max="99" value="${step.intensity}" data-step-intensity>
      <span class="slider-val" data-intensity-val>${step.intensity}</span>
    </div>`}
    <div class="editor-slider">
      <span class="slider-label">Duration</span>
      <input type="range" min="${PRESET_DURATION_MIN_MS / 1000}" max="${PRESET_DURATION_MAX_MS / 1000}" step="${PRESET_DURATION_STEP_MS / 1000}" value="${durSec}" data-step-duration>
      <span class="slider-val" data-duration-val>${formatEditorDuration(step.duration_ms)}</span>
    </div>`;

  // Mode change
  div.querySelector("[data-step-mode]")!.addEventListener("change", (e) => {
    step.mode = (e.target as HTMLSelectElement).value as Preset["tracks"][0]["steps"][0]["mode"];
    renderEditorTracks();
  });

  // Remove step
  div.querySelector("[data-step-remove]")!.addEventListener("click", () => {
    track.steps.splice(si, 1);
    renderEditorTracks();
  });

  // Intensity slider
  const intSlider = div.querySelector("[data-step-intensity]") as HTMLInputElement | null;
  if (intSlider) {
    intSlider.addEventListener("input", () => {
      step.intensity = parseInt(intSlider.value, 10);
      div.querySelector("[data-intensity-val]")!.textContent = String(step.intensity);
      schedulePreviewRefresh();
    });
  }

  // Duration slider
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
  div.addEventListener("dragover", (e) => { e.preventDefault(); div.style.borderTop = "2px solid var(--accent)"; e.stopPropagation(); });
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

// --- Preview ---

function resetPreview(): void {
  if (editorPreviewTimer !== null) clearTimeout(editorPreviewTimer);
  editorPreviewTimer = null;
  editorPreview = { loading: false, error: null, data: null };
  editorPreviewNonce = 0;
  renderEditorPreview();
}

function schedulePreviewRefresh(): void {
  if (!editorData) return;
  if (editorPreviewTimer !== null) clearTimeout(editorPreviewTimer);
  editorPreview.loading = true;
  renderEditorPreview();
  editorPreviewTimer = setTimeout(requestPreview, PRESET_PREVIEW_DEBOUNCE_MS);
}

function requestPreview(): void {
  editorPreviewTimer = null;
  if (!editorData) return;
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    editorPreview = { loading: false, error: "Preview unavailable while disconnected.", data: null };
    renderEditorPreview();
    return;
  }
  const clone = JSON.parse(JSON.stringify(editorData)) as Preset;
  clone.name = (document.getElementById("editor-name") as HTMLInputElement).value.trim() || "__preview__";
  normalizeEditorDurations(clone);
  const nonce = ++editorPreviewNonce;
  sendBoardCommand({ type: "preview_preset", nonce, preset: clone });
}

function handlePresetPreview(msg: PresetPreviewMessage): void {
  if (!editorData || msg.nonce !== editorPreviewNonce) return;
  editorPreview = { loading: false, error: msg.error, data: msg.preview };
  renderEditorPreview();
}

function renderEditorPreview(): void {
  const statusEl = document.getElementById("ep-status");
  const summaryEl = document.getElementById("ep-summary");
  const timelineEl = document.getElementById("ep-timeline");
  const eventsEl = document.getElementById("ep-events");
  if (!statusEl || !summaryEl || !timelineEl || !eventsEl) return;

  statusEl.classList.remove("ep-error");
  summaryEl.textContent = "";
  timelineEl.innerHTML = "";
  eventsEl.innerHTML = "";

  if (!editorData) { statusEl.textContent = "No preview yet."; return; }
  if (editorPreview.loading) { statusEl.textContent = "Updating preview..."; return; }
  if (editorPreview.error) { statusEl.textContent = editorPreview.error; statusEl.classList.add("ep-error"); return; }
  if (!editorPreview.data) { statusEl.textContent = "No preview data available yet."; return; }

  const preview = editorPreview.data;
  const endUs = Math.max(preview.total_duration_us, ...preview.events.map((e) => e.actual_time_us + e.transmit_duration_us));
  const delayed = preview.events.filter((e) => e.actual_time_us !== e.requested_time_us).length;
  statusEl.textContent = "Preview reflects the exact serialized RF transmit order.";
  summaryEl.textContent = `${preview.events.length} RF messages. Span ${fmtUs(preview.total_duration_us)}; timeline ${fmtUs(endUs)}. ${delayed} delayed.`;

  // Timeline bar — sequential flex segments, no overlap
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

  // Cross-highlight timeline segments ↔ table rows
  const all = [...timelineEl.querySelectorAll("[data-pi]"), ...eventsEl.querySelectorAll("[data-pi]")];
  for (const el of all) {
    const pi = (el as HTMLElement).dataset.pi!;
    el.addEventListener("mouseenter", () => { for (const x of all) (x as HTMLElement).classList.toggle("active", (x as HTMLElement).dataset.pi === pi); });
    el.addEventListener("mouseleave", () => { for (const x of all) (x as HTMLElement).classList.remove("active"); });
  }
}

// --- Duration helpers ---

function normalizeDuration(ms: number): number {
  const v = Number.isFinite(ms) ? ms : PRESET_DURATION_MIN_MS;
  return Math.round(Math.min(PRESET_DURATION_MAX_MS, Math.max(PRESET_DURATION_MIN_MS, v)) / PRESET_DURATION_STEP_MS) * PRESET_DURATION_STEP_MS;
}

function formatEditorDuration(ms: number): string {
  const s = normalizeDuration(ms) / 1000;
  return Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`;
}

function normalizeEditorDurations(preset: Preset): void {
  for (const track of preset.tracks) for (const step of track.steps) step.duration_ms = normalizeDuration(step.duration_ms);
}

function fmtUs(us: number): string {
  const ms = Math.round(us / 1000);
  if (ms === 0) return "0ms";
  if (ms % 1000 === 0) return `${ms / 1000}s`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(3)}s`;
  return `${ms}ms`;
}

function segTitle(ev: PresetPreviewEvent): string {
  return `Track ${ev.track_index + 1}, step ${ev.step_index + 1}, ${ev.collar_name}, ${ev.mode}, level ${ev.intensity}, at ${fmtUs(ev.actual_time_us)}, requested ${fmtUs(ev.requested_time_us)}, TX ${fmtUs(ev.transmit_duration_us)}`;
}

// --- Collar/preset actions (unchanged logic) ---

function saveCollar(): void {
  const collarId = parseCollarId(state.collarEditor.collarId.trim());
  if (!state.collarEditor.name.trim()) { showError("Collar name is required"); return; }
  if (collarId === null) { showError("Invalid collar ID"); return; }
  const channel = Number(state.collarEditor.channel);
  if (!Number.isInteger(channel) || channel < 0 || channel > 2) { showError("Channel must be 0, 1, or 2"); return; }
  if (state.collarEditor.originalName) {
    sendBoardCommand({ type: "update_collar", original_name: state.collarEditor.originalName, name: state.collarEditor.name.trim(), collar_id: collarId, channel });
  } else {
    sendBoardCommand({ type: "add_collar", name: state.collarEditor.name.trim(), collar_id: collarId, channel });
  }
  resetCollarEditor();
}

function resetCollarEditor(): void {
  state.collarEditor = { originalName: "", name: "", collarId: "", channel: "0" };
  render();
}

function editCollar(name: string): void {
  const collar = selectedBoard()?.state?.collars.find((c) => c.name === name);
  if (!collar) { showError(`Unknown collar: ${name}`); return; }
  state.collarEditor = { originalName: collar.name, name: collar.name, collarId: formatCollarId(collar.collar_id), channel: String(collar.channel) };
  render();
}

function deleteCollar(name: string): void { sendBoardCommand({ type: "delete_collar", name }); }
function deletePreset(name: string): void { sendBoardCommand({ type: "delete_preset", name }); }
function runPreset(name: string): void { if (!name) { showError("Select a preset first"); return; } sendBoardCommand({ type: "run_preset", name }); }
function pingBoard(): void { const b = selectedBoard(); if (!b) { showError("No board selected"); return; } sendUi({ type: "ping_board", board_id: b.id }); }

function reorderPreset(name: string, direction: -1 | 1): void {
  const presets = selectedBoard()?.state?.presets ?? [];
  const names = presets.map((p) => p.name);
  const i = names.indexOf(name);
  if (i < 0) return;
  const j = i + direction;
  if (j < 0 || j >= names.length) return;
  [names[i], names[j]] = [names[j]!, names[i]!];
  sendBoardCommand({ type: "reorder_presets", names });
}

function sendQuickAction(action: "run_action" | "start_action" | "stop_action"): void {
  if (!state.quickAction.collarName) { showError("Select a collar first"); return; }
  const intensity = state.quickAction.mode === "beep" ? 0 : Number(state.quickAction.intensity);
  if (!Number.isInteger(intensity) || intensity < 0 || intensity > 99) { showError("Intensity must be between 0 and 99"); return; }
  if (action === "run_action") {
    const dur = Number(state.quickAction.durationMs);
    if (!Number.isInteger(dur) || dur <= 0) { showError("Duration must be a positive integer"); return; }
    sendBoardCommand({ type: "run_action", collar_name: state.quickAction.collarName, mode: state.quickAction.mode, intensity, duration_ms: dur });
  } else if (action === "start_action") {
    sendBoardCommand({ type: "start_action", collar_name: state.quickAction.collarName, mode: state.quickAction.mode, intensity });
  } else {
    sendBoardCommand({ type: "stop_action", collar_name: state.quickAction.collarName, mode: state.quickAction.mode });
  }
}

// --- Comms ---

function sendBoardCommand(command: BoardCommand): void {
  const b = selectedBoard();
  if (!b) { showError("No board selected"); return; }
  sendUi({ type: "board_command", board_id: b.id, command });
}

function sendUi(message: UiIncomingMessage): void {
  if (!socket || socket.readyState !== WebSocket.OPEN) { showError("UI socket is not connected"); return; }
  socket.send(JSON.stringify(message));
}

function showError(text: string): void { state.banner = { kind: "error", text }; render(); }

// --- Formatting ---

function describePreset(preset: Preset): string {
  return preset.tracks.map((t) => `${t.collar_name}: ${t.steps.map((s) => `${s.mode} ${s.duration_ms}ms`).join(" > ")}`).join(" | ");
}

function describeEvent(entry: EventLogEntry): string {
  switch (entry.event) {
    case "action": return `${entry.collar_name} ${entry.mode} ${entry.duration_ms}ms${entry.intensity == null ? "" : ` @ ${entry.intensity}%`}`;
    case "preset_run": return `Preset ${entry.preset_name} started`;
    case "ntp_sync": return `NTP sync via ${entry.server}`;
    case "remote_control_connection": return entry.connected ? `Remote connected ${entry.url}` : `Remote disconnected${entry.reason ? `: ${entry.reason}` : ""}`;
    default: return JSON.stringify(entry);
  }
}

function formatCollarId(id: number): string { return `0x${id.toString(16).toUpperCase().padStart(4, "0")}`; }

function parseCollarId(raw: string): number | null {
  if (!raw) return null;
  const v = raw.startsWith("0x") || raw.startsWith("0X") ? Number.parseInt(raw.slice(2), 16)
    : /^[0-9a-fA-F]+$/.test(raw) && /[a-fA-F]/.test(raw) ? Number.parseInt(raw, 16)
    : Number.parseInt(raw, 10);
  return Number.isInteger(v) && v >= 0 && v <= 0xffff ? v : null;
}

function formatDuration(ms: number): string {
  if (ms <= 0) return "0ms";
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

function formatTimestamp(unixMs: number | null, monotonicMs: number): string {
  return unixMs == null ? `${(monotonicMs / 1000).toFixed(3)}s` : new Date(unixMs).toLocaleString();
}

function esc(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\"", "&quot;").replaceAll("'", "&#39;");
}
