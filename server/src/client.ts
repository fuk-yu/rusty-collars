import type {
  BoardCommand,
  BoardSnapshot,
  Collar,
  CommandMode,
  EventLogEntry,
  Preset,
  PresetPreviewMessage,
  SnapshotMessage,
  UiIncomingMessage,
  UiOutgoingMessage,
} from "./protocol.js";

import {
  openEditor,
  closeEditor,
  handlePreviewResult,
  type PresetEditorConfig,
  type EditorPreset,
} from "../../ui-shared/preset-editor.js";

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

// --- Boot ---

const appRoot = document.getElementById("app");
if (!appRoot) throw new Error("Missing #app");
const root: HTMLElement = appRoot;

connect();
render();

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
    case "new-preset": openPresetEditorWrapper(null); break;
    case "edit-preset": openPresetEditorWrapper(el.dataset.presetName ?? ""); break;
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
      handlePreviewResult(message.nonce, message.preview, message.error);
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
// Preset Editor — uses shared ui-shared/preset-editor.ts
// =============================================================================

function openPresetEditorWrapper(nameOrNull: string | null): void {
  const board = selectedBoard();
  const collars: Collar[] = board?.state?.collars ?? [];
  let preset: Preset | null = null;
  let originalName: string | null = null;

  if (nameOrNull) {
    preset = board?.state?.presets.find((p) => p.name === nameOrNull) ?? null;
    if (!preset) { showError(`Unknown preset: ${nameOrNull}`); return; }
    originalName = preset.name;
  }

  const cfg: PresetEditorConfig = {
    collars,
    onSave: async (origName, edited) => {
      sendBoardCommand({ type: "save_preset", original_name: origName, preset: edited as Preset });
    },
    onPreview: (nonce, previewPreset) => {
      sendBoardCommand({ type: "preview_preset", nonce, preset: previewPreset as Preset });
    },
  };

  openEditor(cfg, preset as EditorPreset | null, originalName);
}

// --- Collar/preset actions ---

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
