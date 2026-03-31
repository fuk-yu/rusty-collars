import "./style.css";
import {
  openEditor,
  handlePreviewResult,
  type PresetEditorConfig,
  type EditorPreset,
} from "../../ui-shared/preset-editor.js";

// === Constants ===

const FRONTEND_APP_VERSION = "__RUSTY_COLLARS_APP_VERSION__";
const WS_RECONNECT_BASE_DELAY_MS = 500;
const WS_RECONNECT_MAX_DELAY_MS = 5000;
const RF_LOCKOUT_TICK_MS = 100;
const WS_PING_INTERVAL_MS = 1000;
const WS_PING_TIMEOUT_MS = 3000;
const MODE_EMOJI = { shock: '\u26A1', vibrate: '\u3030\uFE0F', beep: '\uD83D\uDD0A', pause: '\u23F8\uFE0F' };
const MODE_LABEL = { shock: 'Shock', vibrate: 'Vibrate', beep: 'Beep', pause: 'Pause' };

// === State ===

let ws: WebSocket | null = null;
let state: any = { device_id: null, app_version: null, collars: [], presets: [], preset_running: null, rf_lockout_remaining_ms: 0, device_settings: null };
let remoteStatus: any = { enabled: false, connected: false, url: '', validate_cert: true, rtt_ms: null, status_text: 'Off' };
let eventLog: any = { enabled: false, events: [] };
let rfDebug: any = { listening: false, events: [] };
let rfDebugWanted = false;
let wsReconnectDelayMs = WS_RECONNECT_BASE_DELAY_MS;
let wsReconnectTimer: ReturnType<typeof setTimeout> | null = null;
let rfLockoutDeadlineAt = 0;
let rfLockoutTicker: ReturnType<typeof setInterval> | null = null;
const activeHoldStops = new Set<() => void>();
let statusConnected = false;
let statusText = 'Connecting...';
let wsPingMs: number | null = null;
let wsPingInterval: ReturnType<typeof setInterval> | null = null;
let wsPingNonce = 0;
let pendingPing: { nonce: number; startedAt: number } | null = null;
let appReloadPending = false;
let presetDragActive = false;
let presetRenderDeferred = false;

// === WebSocket ===

function renderConnectionStatus() {
  const dot = document.getElementById('dot');
  const connText = document.getElementById('conn-text');
  if (!dot || !connText) return;
  dot.className = statusConnected ? 'dot on' : 'dot off';
  if (statusConnected) {
    connText.textContent = wsPingMs === null ? '\uD83C\uDFD3 ...' : `\uD83C\uDFD3 ${wsPingMs}ms`;
  } else {
    connText.textContent = statusText;
  }
}

function renderRemoteControlStatus() {
  const el = document.getElementById('remote-conn-text');
  if (!el) return;
  el.title = remoteStatus.url || '';
  if (!remoteStatus.enabled) {
    el.textContent = '\u2197 off';
    return;
  }
  if (remoteStatus.connected) {
    el.textContent = remoteStatus.rtt_ms == null ? '\u2197 ...' : `\u2197 ${remoteStatus.rtt_ms}ms`;
    return;
  }
  el.textContent = '\u2197 ' + (remoteStatus.status_text || 'down');
}

function formatUptime(totalSeconds: number) {
  if (totalSeconds == null) return '';
  const d = Math.floor(totalSeconds / 86400);
  const h = Math.floor((totalSeconds % 86400) / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  const parts: string[] = [];
  if (d > 0) parts.push(d + 'd');
  if (h > 0 || d > 0) parts.push(h + 'h');
  if (m > 0 || h > 0 || d > 0) parts.push(m + 'm');
  parts.push(s + 's');
  return parts.join(' ');
}

function renderUptime() {
  const el = document.getElementById('uptime-text');
  if (!el || state.server_uptime_s == null) return;
  el.textContent = '\u23F1\uFE0F ' + formatUptime(state.server_uptime_s);
}

function formatBytes(bytes: number) {
  if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + 'MB';
  if (bytes >= 1024) return (bytes / 1024).toFixed(1) + 'KB';
  return bytes + 'B';
}

function renderClients() {
  const el = document.getElementById('clients-text');
  if (!el || state.connected_clients == null) return;
  el.textContent = '\uD83D\uDC65 ' + state.connected_clients;
  const tooltip = document.getElementById('clients-tooltip');
  if (tooltip) {
    const ips = state.client_ips || [];
    tooltip.innerHTML = ips.length ? ips.map((ip: string) => `<div>${ip}</div>`).join('') : '<div>No clients</div>';
  }
}

function renderHeap() {
  const el = document.getElementById('heap-text');
  if (!el || state.free_heap_bytes == null) return;
  el.textContent = '\uD83D\uDCBE ' + formatBytes(state.free_heap_bytes);
}

function renderAppVersion() {
  const el = document.getElementById('app-version');
  if (el) el.textContent = state.app_version || FRONTEND_APP_VERSION;
}

function setConnectionStatus(connected: boolean, text: string) {
  statusConnected = connected;
  statusText = text;
  renderConnectionStatus();
}

function resetPingState() {
  wsPingMs = null;
  pendingPing = null;
}

function clearPingTimer() {
  if (wsPingInterval !== null) {
    clearInterval(wsPingInterval);
    wsPingInterval = null;
  }
}

function sendPing() {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const now = performance.now();
  if (pendingPing !== null) {
    if (now - pendingPing.startedAt >= WS_PING_TIMEOUT_MS) {
      // Pong never arrived - connection is dead. Force close to trigger reconnect.
      console.warn('Ping timeout - forcing reconnect');
      ws.close();
    }
    return;
  }
  const nonce = ++wsPingNonce;
  pendingPing = { nonce, startedAt: now };
  ws.send(JSON.stringify({ type: 'ping', nonce }));
}

function startPingTimer() {
  clearPingTimer();
  sendPing();
  wsPingInterval = setInterval(sendPing, WS_PING_INTERVAL_MS);
}

function handlePong(msg: any) {
  if (pendingPing === null || pendingPing.nonce !== msg.nonce) return;
  wsPingMs = Math.max(1, Math.round(performance.now() - pendingPing.startedAt));
  pendingPing = null;
  if (msg.server_uptime_s != null) state.server_uptime_s = msg.server_uptime_s;
  if (msg.free_heap_bytes != null) state.free_heap_bytes = msg.free_heap_bytes;
  if (msg.connected_clients != null) state.connected_clients = msg.connected_clients;
  if (msg.client_ips) state.client_ips = msg.client_ips;
  renderConnectionStatus();
  renderUptime();
  renderClients();
  renderHeap();
}

function clearReconnectTimer() {
  if (wsReconnectTimer !== null) {
    clearTimeout(wsReconnectTimer);
    wsReconnectTimer = null;
  }
}

function reloadIfAppVersionChanged(serverAppVersion: string) {
  if (appReloadPending || !serverAppVersion || serverAppVersion === FRONTEND_APP_VERSION) return;
  appReloadPending = true;
  setConnectionStatus(false, 'Firmware updated - reloading...');
  window.location.reload();
}

function scheduleReconnect() {
  if (wsReconnectTimer !== null) return;
  const delayMs = wsReconnectDelayMs;
  setConnectionStatus(false, 'Disconnected - reconnecting in ' + (delayMs / 1000).toFixed(1) + 's...');
  wsReconnectTimer = setTimeout(() => {
    wsReconnectTimer = null;
    wsReconnectDelayMs = Math.min(wsReconnectDelayMs * 2, WS_RECONNECT_MAX_DELAY_MS);
    connect();
  }, delayMs);
}

function connect() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  setConnectionStatus(false, 'Connecting...');
  const wsProtocol = location.protocol === 'https:' ? 'wss://' : 'ws://';
  const socket = new WebSocket(wsProtocol + location.host + '/ws');
  ws = socket;
  socket.onopen = () => {
    if (ws !== socket) {
      socket.close();
      return;
    }
    clearReconnectTimer();
    wsReconnectDelayMs = WS_RECONNECT_BASE_DELAY_MS;
    resetPingState();
    setConnectionStatus(true, 'Ping ...');
    startPingTimer();
    loadDeviceSettings();
    if (rfDebugWanted) {
      send({ type: 'start_rf_debug' });
    }
  };
  socket.onclose = () => {
    if (ws !== socket) return;
    ws = null;
    clearPingTimer();
    resetPingState();
    stopAllHeldButtons();
    rfDebug.listening = false;
    renderDebug();
    scheduleReconnect();
  };
  socket.onerror = () => {
    if (ws !== socket) return;
    console.warn('WebSocket error');
  };
  socket.onmessage = (e) => {
    if (ws !== socket) return;
    const msg = JSON.parse(e.data);
    switch (msg.type) {
      case 'state':
        reloadIfAppVersionChanged(msg.app_version);
        state = { ...msg, device_settings: state.device_settings };
        setRfLockoutRemainingMs(msg.rf_lockout_remaining_ms || 0);
        render();
        break;
      case 'pong':
        handlePong(msg);
        break;
      case 'remote_control_status':
        remoteStatus = msg.status;
        renderRemoteControlStatus();
        break;
      case 'event_log_state':
        eventLog = { enabled: !!msg.enabled, events: msg.events || [] };
        renderEventLog();
        break;
      case 'event_log_event':
        eventLog.events.push(msg.event);
        if (eventLog.events.length > 100) {
          eventLog.events.splice(0, eventLog.events.length - 100);
        }
        renderEventLog();
        break;
      case 'rf_debug_state':
        rfDebug = { listening: msg.listening, events: msg.events };
        renderDebug();
        break;
      case 'rf_debug_event':
        rfDebug.events.push(msg.event);
        if (rfDebug.events.length > 100) {
          rfDebug.events.splice(0, rfDebug.events.length - 100);
        }
        renderDebug();
        break;
      case 'export_data':
        downloadJson(msg.data);
        break;
      case 'device_settings':
        state.device_settings = msg.settings;
        handleDeviceSettings(msg);
        renderDebug();
        break;
      case 'preset_preview':
        handlePreviewResult(msg.nonce, msg.preview ?? null, msg.error ?? null);
        break;
      case 'error':
        alert(msg.message);
        break;
    }
  };
}

function send(obj: any) {
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    const message = 'WebSocket is not connected';
    alert(message);
    throw new Error(message);
  }
  ws.send(JSON.stringify(obj));
}

// === Tabs ===

function showTab(name: string) {
  document.querySelectorAll('.tab-content').forEach(el => el.classList.remove('active'));
  document.querySelectorAll('.tabs button').forEach(el => el.classList.remove('active'));
  document.getElementById('tab-' + name)!.classList.add('active');
  document.querySelectorAll('.tabs button').forEach(el => {
    if (el.textContent!.toLowerCase() === name) el.classList.add('active');
  });
}

// === Render ===

function render() {
  if (isRfLocked()) {
    stopAllHeldButtons();
  }
  renderAppVersion();
  renderUptime();
  renderClients();
  renderHeap();
  renderRemoteControlStatus();
  renderStopAllButton();
  renderRemotes();
  renderPresets();
  renderEventLog();
  renderDebug();
}

function getCollarPref(name: string, key: string, def: string) {
  return sessionStorage.getItem(`collar:${name}:${key}`) ?? def;
}
function setCollarPref(name: string, key: string, val: string) {
  sessionStorage.setItem(`collar:${name}:${key}`, val);
}

function renderRemotes() {
  const list = document.getElementById('remote-list')!;
  list.innerHTML = '';
  const rfLocked = isRfLocked();
  for (const c of state.collars) {
    const card = document.createElement('div');
    card.className = 'card';
    const name = esc(c.name);
    const idHex = '0x' + c.collar_id.toString(16).toUpperCase().padStart(4, '0');
    const savedLevel = getCollarPref(c.name, 'level', '30');
    const savedDur = getCollarPref(c.name, 'duration', '2');
    const intensityMode = getCollarPref(c.name, 'intensityMode', 'fixed');
    const durationMode = getCollarPref(c.name, 'durationMode', 'fixed');
    const levelMin = getCollarPref(c.name, 'levelMin', '10');
    const levelMax = getCollarPref(c.name, 'levelMax', '60');
    const durMin = getCollarPref(c.name, 'durationMin', '1');
    const durMax = getCollarPref(c.name, 'durationMax', '5');

    const intensityValText = intensityMode !== 'fixed' ? levelMin + '-' + levelMax : savedLevel;
    const durationValText = (durationMode === 'random' || durationMode === 'gaussian') ? durMin + '-' + durMax + 's' : savedDur + 's';

    card.innerHTML = `
      <h3>
        ${name} <span class="id">${idHex} CH${c.channel + 1}</span>
        <span style="flex:1"></span>
        <button class="btn-edit" data-action="edit-remote" data-name="${name}">Edit</button>
        <button class="btn-del" data-action="delete-remote" data-name="${name}">Del</button>
      </h3>
      <div class="controls">
        <div class="slider-group">
          <label style="font-size:0.8em;color:var(--text2)">Level</label>
          <input type="range" min="0" max="99" value="${savedLevel}" id="slider-${name}"
            ${intensityMode !== 'fixed' ? 'style="display:none"' : ''}
            oninput="setCollarPref('${name}','level',this.value);document.getElementById('val-${name}').textContent=this.value">
          <div class="range-slider" id="range-intensity-${name}" ${intensityMode === 'fixed' ? 'style="display:none"' : ''}>
            <input type="range" class="range-min" min="0" max="99" value="${levelMin}" oninput="updateRangeMin('intensity','${name}',this)">
            <input type="range" class="range-max" min="0" max="99" value="${levelMax}" oninput="updateRangeMax('intensity','${name}',this)">
          </div>
          <span class="val" id="val-${name}">${intensityValText}</span>
        </div>
      </div>
      ${durationMode !== 'held' ? `<div class="controls" style="margin-top:4px">
        <div class="slider-group" id="dur-group-${name}">
          <label style="font-size:0.8em;color:var(--text2)">Duration</label>
          <input type="range" min="0.5" max="10" step="0.5" value="${savedDur}"
            id="dur-slider-${name}"
            ${durationMode === 'random' || durationMode === 'gaussian' ? 'style="display:none"' : ''}
            oninput="setCollarPref('${name}','duration',this.value);document.getElementById('dur-val-${name}').textContent=this.value+'s'">
          <div class="range-slider" id="range-duration-${name}" ${durationMode !== 'random' && durationMode !== 'gaussian' ? 'style="display:none"' : ''}>
            <input type="range" class="range-min" min="0.5" max="10" step="0.5" value="${durMin}" oninput="updateRangeMin('duration','${name}',this)">
            <input type="range" class="range-max" min="0.5" max="10" step="0.5" value="${durMax}" oninput="updateRangeMax('duration','${name}',this)">
          </div>
          <span class="val" id="dur-val-${name}">${durationValText}</span>
        </div>
      </div>` : ''}
      <div class="controls action-btns" style="margin-top:6px">
        <button class="btn-shock" data-collar="${name}" data-mode="shock" ${rfLocked ? 'disabled' : ''}>Zap</button>
        <button class="btn-vibrate" data-collar="${name}" data-mode="vibrate" ${rfLocked ? 'disabled' : ''}>Vibrate</button>
        <button class="btn-beep" data-collar="${name}" data-mode="beep" ${rfLocked ? 'disabled' : ''}>Beep</button>
        <span style="flex:1"></span>
        <select class="mode-select" id="intensity-mode-${name}" onchange="updateIntensityMode('${name}',this.value)">
          <option value="fixed" ${intensityMode === 'fixed' ? 'selected' : ''}>\uD83D\uDCC8 Fixed</option>
          <option value="random" ${intensityMode === 'random' ? 'selected' : ''}>\uD83D\uDCC8 Random</option>
          <option value="gaussian" ${intensityMode === 'gaussian' ? 'selected' : ''}>\uD83D\uDCC8 Gaussian</option>
        </select>
        <select class="mode-select" id="duration-mode-${name}" onchange="updateDurationMode('${name}',this.value)">
          <option value="fixed" ${durationMode === 'fixed' ? 'selected' : ''}>\u23F1 Fixed</option>
          <option value="held" ${durationMode === 'held' ? 'selected' : ''}>\u23F1 Held</option>
          <option value="random" ${durationMode === 'random' ? 'selected' : ''}>\u23F1 Random</option>
          <option value="gaussian" ${durationMode === 'gaussian' ? 'selected' : ''}>\u23F1 Gaussian</option>
        </select>
      </div>
    `;
    list.appendChild(card);
  }

  // Attach action button handlers
  document.querySelectorAll('.action-btns button').forEach(btn => {
    const collar = (btn as HTMLElement).dataset.collar;
    const durationMode = getCollarPref(collar!, 'durationMode', 'fixed');
    if (durationMode === 'held') {
      setupHoldButton(btn as HTMLButtonElement);
    } else {
      setupTimedButton(btn as HTMLButtonElement);
    }
  });
  list.querySelectorAll('[data-action="edit-remote"]').forEach(btn => {
    btn.addEventListener('click', () => openRemoteEditor((btn as HTMLElement).dataset.name!));
  });
  list.querySelectorAll('[data-action="delete-remote"]').forEach(btn => {
    btn.addEventListener('click', () => deleteRemote((btn as HTMLElement).dataset.name!));
  });
}

function updateIntensityMode(name: string, mode: string) {
  setCollarPref(name, 'intensityMode', mode);
  renderRemotes();
}

function updateDurationMode(name: string, mode: string) {
  setCollarPref(name, 'durationMode', mode);
  renderRemotes();
}

function updateRangeMin(type: string, name: string, input: HTMLInputElement) {
  const key = type === 'intensity' ? 'level' : 'duration';
  const maxKey = key + 'Max';
  const minKey = key + 'Min';
  const maxVal = parseFloat(getCollarPref(name, maxKey, type === 'intensity' ? '60' : '5'));
  if (parseFloat(input.value) > maxVal) {
    input.value = String(maxVal);
  }
  setCollarPref(name, minKey, input.value);
  const valEl = type === 'intensity'
    ? document.getElementById('val-' + name)
    : document.getElementById('dur-val-' + name);
  if (valEl) {
    valEl.textContent = type === 'intensity'
      ? input.value + '-' + maxVal
      : input.value + '-' + maxVal + 's';
  }
}

function updateRangeMax(type: string, name: string, input: HTMLInputElement) {
  const key = type === 'intensity' ? 'level' : 'duration';
  const maxKey = key + 'Max';
  const minKey = key + 'Min';
  const minVal = parseFloat(getCollarPref(name, minKey, type === 'intensity' ? '10' : '1'));
  if (parseFloat(input.value) < minVal) {
    input.value = String(minVal);
  }
  setCollarPref(name, maxKey, input.value);
  const valEl = type === 'intensity'
    ? document.getElementById('val-' + name)
    : document.getElementById('dur-val-' + name);
  if (valEl) {
    valEl.textContent = type === 'intensity'
      ? minVal + '-' + input.value
      : minVal + '-' + input.value + 's';
  }
}

function setupTimedButton(btn: HTMLButtonElement) {
  function fire(e: Event) {
    e.preventDefault();
    if (isRfLocked()) return;
    const collar = btn.dataset.collar!;
    const mode = btn.dataset.mode;
    const intensityMode = getCollarPref(collar, 'intensityMode', 'fixed');
    const durationMode = getCollarPref(collar, 'durationMode', 'fixed');

    const msg: any = { type: 'run_action', collar_name: collar, mode };

    if (intensityMode === 'random' || intensityMode === 'gaussian') {
      msg.intensity = parseInt(getCollarPref(collar, 'levelMin', '10'));
      msg.intensity_max = parseInt(getCollarPref(collar, 'levelMax', '60'));
      if (intensityMode === 'gaussian') {
        msg.intensity_distribution = 'gaussian';
      }
    } else {
      const slider = document.getElementById('slider-' + collar) as HTMLInputElement | null;
      msg.intensity = slider ? parseInt(slider.value) : 30;
    }

    if (durationMode === 'random' || durationMode === 'gaussian') {
      msg.duration_ms = Math.round(parseFloat(getCollarPref(collar, 'durationMin', '1')) * 1000);
      msg.duration_max_ms = Math.round(parseFloat(getCollarPref(collar, 'durationMax', '5')) * 1000);
      if (durationMode === 'gaussian') {
        msg.duration_distribution = 'gaussian';
      }
    } else {
      const durSlider = document.getElementById('dur-slider-' + collar) as HTMLInputElement | null;
      const durationSec = durSlider ? parseFloat(durSlider.value) : 2;
      msg.duration_ms = Math.round(durationSec * 1000);
    }

    send(msg);
  }
  btn.addEventListener('mousedown', fire);
  btn.addEventListener('touchstart', fire, { passive: false });
}

function renderPresets() {
  if (presetDragActive) {
    presetRenderDeferred = true;
    return;
  }
  const list = document.getElementById('preset-list')!;
  list.innerHTML = '';
  const rfLocked = isRfLocked();

  // Drag state shared across all cards
  let dragEnterCounters = new Map<HTMLElement, number>();
  let currentDropTarget: HTMLElement | null = null;

  function clearDropIndicator() {
    if (currentDropTarget) {
      currentDropTarget.style.borderTop = '';
      currentDropTarget = null;
    }
  }

  function getDropIndex(e: DragEvent): number {
    // Find the card closest to the cursor, considering gaps between cards
    const children = Array.from(list.children) as HTMLElement[];
    for (let i = 0; i < children.length; i++) {
      const rect = children[i]!.getBoundingClientRect();
      if (e.clientY < rect.top + rect.height / 2) return i;
    }
    return children.length;
  }

  for (let pi = 0; pi < state.presets.length; pi++) {
    const p = state.presets[pi];
    const isRunning = state.preset_running === p.name;
    const card = document.createElement('div');
    card.className = 'card' + (isRunning ? ' preset-running' : '');
    card.draggable = true;
    card.dataset.presetIndex = String(pi);

    let summary = p.tracks.map((t: any) => {
      const steps = t.steps.map((s: any) => describePresetStep(s)).join(' > ');
      return `<em>${esc(t.collar_name)}</em>: ${steps}`;
    }).join('<br>');

    card.innerHTML = `
      <h3>
        ${esc(p.name)}
        <span style="flex:1"></span>
        ${isRunning
          ? '<button class="btn-stop" onclick="stopPreset()">Stop</button>'
          : `<button class="btn-run" data-action="run-preset" data-name="${esc(p.name)}" ${rfLocked ? 'disabled' : ''}>Run</button>`}
        <button class="btn-edit" data-action="dup-preset" data-name="${esc(p.name)}" style="background:var(--accent2)">Dup</button>
        <button class="btn-edit" data-action="edit-preset" data-name="${esc(p.name)}">Edit</button>
        <button class="btn-del" data-action="delete-preset" data-name="${esc(p.name)}">Del</button>
      </h3>
      <div style="font-size:0.85em;color:var(--text2);margin-top:6px">${summary}</div>
    `;

    card.addEventListener('dragstart', e => {
      presetDragActive = true;
      e.dataTransfer!.setData('text/plain', String(pi));
      e.dataTransfer!.effectAllowed = 'move';
      card.style.opacity = '0.5';
    });
    card.addEventListener('dragend', () => {
      card.style.opacity = '';
      clearDropIndicator();
      presetDragActive = false;
      if (presetRenderDeferred) {
        presetRenderDeferred = false;
        renderPresets();
      }
    });

    // Use dragenter counter to handle child-element dragleave noise
    card.addEventListener('dragenter', e => {
      e.preventDefault();
      const count = (dragEnterCounters.get(card) ?? 0) + 1;
      dragEnterCounters.set(card, count);
      if (count === 1) {
        clearDropIndicator();
        currentDropTarget = card;
        card.style.borderTop = '2px solid var(--accent)';
      }
    });
    card.addEventListener('dragover', e => { e.preventDefault(); });
    card.addEventListener('dragleave', () => {
      const count = (dragEnterCounters.get(card) ?? 1) - 1;
      dragEnterCounters.set(card, count);
      if (count <= 0) {
        dragEnterCounters.delete(card);
        if (currentDropTarget === card) {
          card.style.borderTop = '';
          currentDropTarget = null;
        }
      }
    });
    card.addEventListener('drop', e => {
      e.preventDefault();
      e.stopPropagation();
      clearDropIndicator();
      dragEnterCounters.clear();
      const from = parseInt(e.dataTransfer!.getData('text/plain'));
      const to = pi;
      if (from !== to) reorderPresets(from, to);
    });
    list.appendChild(card);
  }

  // Handle drops in gaps between cards (on the container itself)
  list.addEventListener('dragover', e => { e.preventDefault(); });
  list.addEventListener('drop', e => {
    e.preventDefault();
    clearDropIndicator();
    dragEnterCounters.clear();
    const from = parseInt(e.dataTransfer!.getData('text/plain'));
    if (isNaN(from)) return;
    const to = getDropIndex(e);
    // getDropIndex returns a positional index (0..N); reorderPresets handles the adjustment
    if (from !== to) reorderPresets(from, to);
  });

  list.querySelectorAll('[data-action="run-preset"]').forEach(btn => {
    btn.addEventListener('click', () => runPreset((btn as HTMLElement).dataset.name!));
  });
  list.querySelectorAll('[data-action="edit-preset"]').forEach(btn => {
    btn.addEventListener('click', () => openPresetEditor((btn as HTMLElement).dataset.name!));
  });
  list.querySelectorAll('[data-action="dup-preset"]').forEach(btn => {
    btn.addEventListener('click', () => duplicatePreset((btn as HTMLElement).dataset.name!));
  });
  list.querySelectorAll('[data-action="delete-preset"]').forEach(btn => {
    btn.addEventListener('click', () => deletePreset((btn as HTMLElement).dataset.name!));
  });
}

// === Collar Actions ===

function addRemote() {
  const name = (document.getElementById('add-name') as HTMLInputElement).value.trim();
  const idStr = (document.getElementById('add-id') as HTMLInputElement).value.trim();
  const channel = parseInt((document.getElementById('add-ch') as HTMLSelectElement).value);
  if (!name || !idStr) { alert('Name and ID required'); return; }
  const collar_id = parseCollarId(idStr);
  if (collar_id === null) { alert('Invalid ID. Use decimal (39802) or hex (0x9B7A or 9B7A)'); return; }
  send({ type: 'add_collar', name, collar_id, channel });
  (document.getElementById('add-name') as HTMLInputElement).value = '';
  (document.getElementById('add-id') as HTMLInputElement).value = '';
}

function openRemoteEditor(name: string | null) {
  const overlay = document.getElementById('remote-editor-overlay')!;
  const title = document.getElementById('remote-editor-title')!;
  const origField = document.getElementById('remote-editor-original-name') as HTMLInputElement;
  const nameField = document.getElementById('remote-editor-name') as HTMLInputElement;
  const idField = document.getElementById('remote-editor-id') as HTMLInputElement;
  const chField = document.getElementById('remote-editor-ch') as HTMLSelectElement;
  if (name) {
    const c = state.collars.find((x: any) => x.name === name);
    if (!c) return;
    title.textContent = 'Edit Remote';
    origField.value = name;
    nameField.value = c.name;
    idField.value = '0x' + c.collar_id.toString(16).toUpperCase().padStart(4, '0');
    chField.value = c.channel;
  } else {
    title.textContent = 'New Remote';
    origField.value = '';
    nameField.value = '';
    idField.value = '';
    chField.value = '0';
  }
  overlay.classList.add('active');
  nameField.focus();
}

function closeRemoteEditor() {
  document.getElementById('remote-editor-overlay')!.classList.remove('active');
}

function saveRemoteEditor() {
  const origName = (document.getElementById('remote-editor-original-name') as HTMLInputElement).value;
  const name = (document.getElementById('remote-editor-name') as HTMLInputElement).value.trim();
  const idStr = (document.getElementById('remote-editor-id') as HTMLInputElement).value.trim();
  const channel = parseInt((document.getElementById('remote-editor-ch') as HTMLSelectElement).value);
  if (!name || !idStr) { alert('Name and ID required'); return; }
  const collar_id = parseCollarId(idStr);
  if (collar_id === null) { alert('Invalid ID. Use decimal (39802) or hex (0x9B7A or 9B7A)'); return; }
  if (origName) {
    send({ type: 'update_collar', original_name: origName, name, collar_id, channel });
  } else {
    send({ type: 'add_collar', name, collar_id, channel });
  }
  closeRemoteEditor();
}

function deleteRemote(name: string) {
  if (confirm('Delete remote "' + name + '"?')) {
    send({ type: 'delete_collar', name });
  }
}

// === Preset Actions ===

function runPreset(name: string) { send({ type: 'run_preset', name }); }
function stopPreset() {
  send({ type: 'stop_preset' });
  state.preset_running = null;
  render();
}
function deletePreset(name: string) {
  if (confirm('Delete preset "' + name + '"?')) send({ type: 'delete_preset', name });
}
function duplicatePreset(name: string) {
  const p = state.presets.find((x: any) => x.name === name);
  if (!p) return;
  const copy = JSON.parse(JSON.stringify(p));
  copy.name = p.name + ' copy';
  send({ type: 'save_preset', original_name: null, preset: copy });
}
function reorderPresets(fromIndex: number, toIndex: number) {
  const presets = state.presets.slice();
  const [moved] = presets.splice(fromIndex, 1);
  // After removing the source, indices shift: adjust for forward drags
  const insertAt = toIndex > fromIndex ? toIndex - 1 : toIndex;
  presets.splice(insertAt, 0, moved);
  send({ type: 'reorder_presets', names: presets.map((p: any) => p.name) });
}

// === Preset Editor (shared component) ===

function openPresetEditor(name: string | null) {
  let preset: EditorPreset | null = null;
  let originalName: string | null = null;

  if (name) {
    const p = state.presets.find((x: any) => x.name === name);
    if (!p) return;
    preset = JSON.parse(JSON.stringify(p));
    originalName = p.name;
  }

  const cfg: PresetEditorConfig = {
    collars: state.collars,
    onSave: async (origName, edited) => {
      send({ type: 'save_preset', original_name: origName, preset: edited });
    },
    onPreview: (nonce, previewPreset) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: 'preview_preset', nonce, preset: previewPreset }));
    },
  };

  openEditor(cfg, preset, originalName);
}

// === Export / Import ===

function doExport() { send({ type: 'export' }); }

// === OTA Firmware Update ===

async function startOta(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  if (!file) return;
  input.value = '';

  if (!file.name.endsWith('.bin')) {
    alert('Please select a .bin firmware file');
    return;
  }
  if (!confirm(`Upload ${file.name} (${(file.size / 1024).toFixed(0)} KB)? Device will reboot after update.`)) return;

  const btn = document.getElementById('ota-btn') as HTMLButtonElement;
  const progress = document.getElementById('ota-progress')!;
  const bar = document.getElementById('ota-bar')!;
  const statusEl = document.getElementById('ota-status')!;

  btn.disabled = true;
  btn.textContent = 'Uploading...';
  progress.style.display = 'block';
  bar.style.width = '0%';
  statusEl.textContent = 'Starting upload...';

  try {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/ota');
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.timeout = 120000;

    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) {
        const pct = (e.loaded / e.total * 100).toFixed(0);
        bar.style.width = pct + '%';
        statusEl.textContent = `Uploading: ${(e.loaded / 1024).toFixed(0)} / ${(e.total / 1024).toFixed(0)} KB (${pct}%)`;
      }
    };

    const result = await new Promise<XMLHttpRequest>((resolve, reject) => {
      xhr.onload = () => resolve(xhr);
      xhr.onerror = () => reject(new Error('Network error'));
      xhr.ontimeout = () => reject(new Error('Upload timeout'));
      xhr.send(file);
    });

    if (result.status === 200) {
      bar.style.width = '100%';
      bar.style.background = 'var(--ok)';
      statusEl.textContent = 'Update complete! Device is rebooting...';
      setConnectionStatus(false, 'Rebooting after OTA...');
    } else {
      throw new Error(result.responseText || `HTTP ${result.status}`);
    }
  } catch (e: any) {
    bar.style.background = 'var(--danger)';
    statusEl.textContent = 'OTA failed: ' + e.message;
    statusEl.style.color = 'var(--danger)';
  } finally {
    btn.disabled = false;
    btn.textContent = 'Select firmware .bin';
  }
}

// === Stress Test ===

async function runStressTest() {
  const numClients = parseInt((document.getElementById('stress-clients') as HTMLInputElement).value) || 3;
  const durationSec = parseInt((document.getElementById('stress-duration') as HTMLInputElement).value) || 10;
  const btn = document.getElementById('stress-btn') as HTMLButtonElement;
  const log = document.getElementById('stress-log')!;
  btn.disabled = true;
  btn.textContent = 'Running...';
  log.textContent = '';

  const slog = (msg: string) => { log.textContent += msg + '\n'; log.scrollTop = log.scrollHeight; };
  slog(`Starting stress test: ${numClients} clients, ${durationSec}s`);

  const results = { opened: 0, failed: 0, pings: 0, pongs: 0, errors: 0, latencies: [] as number[] };
  const sockets: WebSocket[] = [];

  // Open connections
  for (let i = 0; i < numClients; i++) {
    try {
      const s = await new Promise<WebSocket>((resolve, reject) => {
        const sock = new WebSocket('ws://' + location.host + '/ws');
        sock.onopen = () => { results.opened++; resolve(sock); };
        sock.onerror = () => { results.failed++; reject(new Error('connect failed')); };
        setTimeout(() => reject(new Error('connect timeout')), 5000);
      });
      sockets.push(s);
      slog(`  Client ${i + 1}/${numClients} connected`);
    } catch (e: any) {
      slog(`  Client ${i + 1}/${numClients} FAILED: ${e.message}`);
    }
  }

  slog(`${results.opened} connected, ${results.failed} failed. Hammering for ${durationSec}s...`);

  // Track pending pings for RTT measurement
  let nextNonce = 1;
  const pendingPings = new Map<number, number>(); // nonce -> timestamp

  // Set up pong handlers
  sockets.forEach((s) => {
    s.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data);
        if (msg.type === 'pong' && pendingPings.has(msg.nonce)) {
          results.pongs++;
          const rtt = performance.now() - pendingPings.get(msg.nonce)!;
          pendingPings.delete(msg.nonce);
          results.latencies.push(Math.round(rtt));
        }
      } catch (_) {}
    };
    s.onerror = () => results.errors++;
    s.onclose = () => {};
  });

  // Hammer with pings + random messages
  const endAt = Date.now() + durationSec * 1000;
  const interval = setInterval(() => {
    if (Date.now() >= endAt) {
      clearInterval(interval);
      return;
    }
    for (const s of sockets) {
      if (s.readyState !== WebSocket.OPEN) continue;
      // 60% pings (with RTT tracking), 20% export, 20% settings
      const r = Math.random();
      if (r < 0.6) {
        const nonce = nextNonce++;
        pendingPings.set(nonce, performance.now());
        results.pings++;
        s.send(JSON.stringify({ type: 'ping', nonce }));
      } else if (r < 0.8) {
        s.send(JSON.stringify({ type: 'export' }));
      } else {
        s.send(JSON.stringify({ type: 'get_device_settings' }));
      }
    }
  }, 50); // ~20 msgs/sec per client

  // Wait for duration + drain time for in-flight pongs
  await new Promise(r => setTimeout(r, durationSec * 1000));
  clearInterval(interval);
  slog(`Sending complete. Waiting 2s for in-flight responses...`);
  await new Promise(r => setTimeout(r, 2000));

  // Close all
  sockets.forEach(s => { try { s.close(); } catch (_) {} });

  // Report
  const lats = results.latencies.sort((a, b) => a - b);
  const avg = lats.length ? Math.round(lats.reduce((a, b) => a + b, 0) / lats.length) : 0;
  const p50 = lats.length ? lats[Math.floor(lats.length * 0.5)] : 0;
  const p99 = lats.length ? lats[Math.floor(lats.length * 0.99)] : 0;
  const max = lats.length ? lats[lats.length - 1] : 0;

  slog('');
  slog(`=== Results ===`);
  slog(`Connections: ${results.opened} ok, ${results.failed} failed`);
  slog(`Pings sent: ${results.pings}, Pongs received: ${results.pongs}`);
  slog(`Loss: ${results.pings > 0 ? ((1 - results.pongs / results.pings) * 100).toFixed(1) : 0}%`);
  slog(`Errors: ${results.errors}`);
  slog(`Latency: avg=${avg}ms p50=${p50}ms p99=${p99}ms max=${max}ms`);
  slog(`=== Done ===`);

  btn.disabled = false;
  btn.textContent = 'Run';
}

function loadDeviceSettings() { send({ type: 'get_device_settings' }); }

function updateNtpSettingsUi() {
  const enabled = (document.getElementById('settings-ntp-enabled') as HTMLInputElement).checked;
  const row = document.getElementById('settings-ntp-server-row')!;
  const input = document.getElementById('settings-ntp-server') as HTMLInputElement;
  row.classList.toggle('settings-disabled', !enabled);
  input.disabled = !enabled;
}

function updateRemoteControlSettingsUi() {
  const enabled = (document.getElementById('settings-remote-control-enabled') as HTMLInputElement).checked;
  const urlRow = document.getElementById('settings-remote-control-url-row')!;
  const urlInput = document.getElementById('settings-remote-control-url') as HTMLInputElement;
  const validateRow = document.getElementById('settings-remote-control-validate-row')!;
  const validateInput = document.getElementById('settings-remote-control-validate-cert') as HTMLInputElement;
  const isWss = urlInput.value.trim().toLowerCase().startsWith('wss://');
  urlRow.classList.toggle('settings-disabled', !enabled);
  urlInput.disabled = !enabled;
  validateRow.classList.toggle('settings-disabled', !enabled || !isWss);
  validateInput.disabled = !enabled || !isWss;
}

function rebootDevice() {
  if (!confirm('Reboot the device? Active connections will be dropped.')) return;
  send({ type: 'reboot' });
  setConnectionStatus(false, 'Rebooting...');
}

function saveDeviceSettings() {
  const txLed = parseInt((document.getElementById('settings-tx-led-pin') as HTMLInputElement).value);
  const rxLed = parseInt((document.getElementById('settings-rx-led-pin') as HTMLInputElement).value);
  const tx = parseInt((document.getElementById('settings-tx-pin') as HTMLInputElement).value);
  const rx = parseInt((document.getElementById('settings-rx-pin') as HTMLInputElement).value);
  if ([txLed, rxLed, tx, rx].some(v => isNaN(v) || v < 0 || v > 54)) {
    alert('Pin numbers must be 0-54');
    return;
  }
  const maxClients = parseInt((document.getElementById('settings-max-clients') as HTMLInputElement).value) || 8;
  const ntpEnabled = (document.getElementById('settings-ntp-enabled') as HTMLInputElement).checked;
  const ntpServer = (document.getElementById('settings-ntp-server') as HTMLInputElement).value.trim();
  const remoteControlEnabled = (document.getElementById('settings-remote-control-enabled') as HTMLInputElement).checked;
  const remoteControlUrl = (document.getElementById('settings-remote-control-url') as HTMLInputElement).value.trim();
  const remoteControlValidateCert = (document.getElementById('settings-remote-control-validate-cert') as HTMLInputElement).checked;
  if (ntpEnabled && !ntpServer) {
    alert('NTP server is required when time sync is enabled');
    return;
  }
  if (remoteControlEnabled) {
    if (!remoteControlUrl) {
      alert('Remote control URL is required when remote control is enabled');
      return;
    }
    if (!/^wss?:\/\//i.test(remoteControlUrl)) {
      alert('Remote control URL must start with ws:// or wss://');
      return;
    }
  }
  send({
    type: 'save_device_settings',
    settings: {
      device_id: (document.getElementById('settings-device-id') as HTMLInputElement).value.trim(),
      tx_led_pin: txLed,
      rx_led_pin: rxLed,
      rf_tx_pin: tx,
      rf_rx_pin: rx,
      wifi_ssid: (document.getElementById('settings-wifi-ssid') as HTMLInputElement).value,
      wifi_password: (document.getElementById('settings-wifi-password') as HTMLInputElement).value,
      ap_enabled: (document.getElementById('settings-ap-enabled') as HTMLInputElement).checked,
      ap_password: (document.getElementById('settings-ap-password') as HTMLInputElement).value,
      max_clients: maxClients,
      ntp_enabled: ntpEnabled,
      ntp_server: ntpServer,
      remote_control_enabled: remoteControlEnabled,
      remote_control_url: remoteControlUrl,
      remote_control_validate_cert: remoteControlValidateCert,
      record_event_log: (document.getElementById('settings-record-event-log') as HTMLInputElement).checked,
    }
  });
}

function handleDeviceSettings(msg: any) {
  const s = msg.settings;
  const hasWifi = msg.has_wifi;
  document.getElementById('card-wifi-sta')!.style.display = hasWifi ? '' : 'none';
  document.getElementById('card-wifi-ap')!.style.display = hasWifi ? '' : 'none';
  (document.getElementById('settings-device-id') as HTMLInputElement).value = s.device_id || '';
  (document.getElementById('settings-tx-led-pin') as HTMLInputElement).value = s.tx_led_pin;
  (document.getElementById('settings-rx-led-pin') as HTMLInputElement).value = s.rx_led_pin;
  (document.getElementById('settings-tx-pin') as HTMLInputElement).value = s.rf_tx_pin;
  (document.getElementById('settings-rx-pin') as HTMLInputElement).value = s.rf_rx_pin;
  (document.getElementById('settings-wifi-ssid') as HTMLInputElement).value = s.wifi_ssid || '';
  (document.getElementById('settings-wifi-password') as HTMLInputElement).value = s.wifi_password || '';
  (document.getElementById('settings-ap-enabled') as HTMLInputElement).checked = s.ap_enabled;
  (document.getElementById('settings-ap-password') as HTMLInputElement).value = s.ap_password || '';
  (document.getElementById('settings-max-clients') as HTMLInputElement).value = s.max_clients || 8;
  (document.getElementById('settings-ntp-enabled') as HTMLInputElement).checked = s.ntp_enabled !== false;
  (document.getElementById('settings-ntp-server') as HTMLInputElement).value = s.ntp_server || 'pool.ntp.org';
  (document.getElementById('settings-remote-control-enabled') as HTMLInputElement).checked = !!s.remote_control_enabled;
  (document.getElementById('settings-remote-control-url') as HTMLInputElement).value = s.remote_control_url || '';
  (document.getElementById('settings-remote-control-validate-cert') as HTMLInputElement).checked = s.remote_control_validate_cert !== false;
  (document.getElementById('settings-record-event-log') as HTMLInputElement).checked = !!s.record_event_log;
  updateNtpSettingsUi();
  updateRemoteControlSettingsUi();
  const statusEl = document.getElementById('settings-status')!;
  if (msg.reboot_required) {
    statusEl.textContent = 'Saved. Reboot required to apply changes.';
    statusEl.style.color = 'var(--warn)';
  } else {
    statusEl.textContent = '';
  }
}

function downloadJson(data: any) {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'collars-config.json';
  a.click();
  URL.revokeObjectURL(a.href);
}

function doImport(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  if (!file) return;
  const reader = new FileReader();
  reader.onload = () => {
    try {
      const data = JSON.parse(reader.result as string);
      if (!data.collars || !data.presets) throw new Error('Missing collars/presets');
      if (confirm('Import will replace all current settings. Continue?')) {
        send({ type: 'import', data });
      }
    } catch (err: any) {
      alert('Invalid config file: ' + err.message);
    }
  };
  reader.readAsText(file);
  input.value = '';
}

// === RF Debug ===

function toggleRfDebug() {
  if (rfDebug.listening) {
    rfDebugWanted = false;
    send({ type: 'stop_rf_debug' });
  } else {
    rfDebugWanted = true;
    send({ type: 'start_rf_debug' });
  }
}

function clearRfDebug() {
  send({ type: 'clear_rf_debug' });
}

function stopAllRf() {
  stopAllHeldButtons();
  send({ type: 'stop_all' });
  state.preset_running = null;
  setRfLockoutRemainingMs(10000);
  render();
}

function setRfLockoutRemainingMs(remainingMs: number) {
  rfLockoutDeadlineAt = performance.now() + Math.max(0, remainingMs);
  if (remainingMs > 0) {
    stopAllHeldButtons();
    ensureRfLockoutTicker();
  } else if (rfLockoutTicker !== null) {
    clearInterval(rfLockoutTicker);
    rfLockoutTicker = null;
  }
}

function ensureRfLockoutTicker() {
  if (rfLockoutTicker !== null) return;
  rfLockoutTicker = setInterval(() => {
    renderStopAllButton();
    if (!isRfLocked()) {
      clearInterval(rfLockoutTicker!);
      rfLockoutTicker = null;
      render();
    }
  }, RF_LOCKOUT_TICK_MS);
}

function getRfLockoutRemainingMs() {
  return Math.max(0, rfLockoutDeadlineAt - performance.now());
}

function isRfLocked() {
  return getRfLockoutRemainingMs() > 0;
}

function renderStopAllButton() {
  const btn = document.getElementById('stop-all-btn');
  if (!btn) return;
  const remainingMs = getRfLockoutRemainingMs();
  (btn as HTMLButtonElement).disabled = remainingMs > 0;
  btn.textContent = remainingMs > 0 ? `STOP ${Math.ceil(remainingMs / 100) / 10}s` : 'STOP';
}

function stopAllHeldButtons() {
  [...activeHoldStops].forEach(stop => stop());
}

function renderEventLog() {
  const statusEl = document.getElementById('event-log-status');
  const list = document.getElementById('event-log-events');
  if (!statusEl || !list) return;

  if (!eventLog.enabled) {
    statusEl.textContent = 'Recording off';
    list.innerHTML = '<div class="event-log-empty">Enable "Record event log" in Settings to capture recent actions, presets, NTP syncs, and remote-control connectivity.</div>';
    return;
  }

  statusEl.textContent = `${eventLog.events.length}/100 events`;
  if (eventLog.events.length === 0) {
    list.innerHTML = '<div class="event-log-empty">No events recorded yet.</div>';
    return;
  }

  list.innerHTML = '';
  [...eventLog.events].reverse().forEach((entry: any) => {
    const item = document.createElement('div');
    item.className = 'event-log-item';
    item.innerHTML = `
      <span class="ts mono">${esc(formatEventLogTime(entry))}</span>
      <span class="src mono">${esc(formatEventLogSource(entry.source))}</span>
      <span class="body">${esc(formatEventLogBody(entry))}</span>
    `;
    list.appendChild(item);
  });
  list.scrollTop = 0;
}

function renderDebug() {
  const toggle = document.getElementById('rf-debug-toggle');
  const statusEl = document.getElementById('rf-debug-status');
  const list = document.getElementById('rf-debug-events');
  if (!toggle || !statusEl || !list) return;

  toggle.textContent = rfDebug.listening ? 'Stop Listening' : 'Start Listening';
  toggle.className = rfDebug.listening ? 'btn-stop' : 'btn-run';
  statusEl.textContent = rfDebug.listening
    ? 'Listening on GPIO' + (state.device_settings?.rf_rx_pin ?? '?') + ' for type-1 433 MHz frames'
    : 'Receiver idle on GPIO' + (state.device_settings?.rf_rx_pin ?? '?');

  if (rfDebug.events.length === 0) {
    list.innerHTML = '<div style="font-size:0.85em;color:var(--text2)">No RF frames captured yet.</div>';
    return;
  }

  list.innerHTML = '';
  [...rfDebug.events].reverse().forEach((event: any) => {
    const item = document.createElement('div');
    item.className = 'debug-event mono';
    const id = event.collar_id.toString(16).toUpperCase().padStart(4, '0');
    const ckClass = event.checksum_ok ? 'cksum-ok' : 'cksum-bad';
    const ckText = event.checksum_ok ? '\u2713' : '\u2717';
    item.innerHTML = `<span class="ts">${formatDebugTime(event.received_at_ms)}</span> 0x${id} ch${event.channel} ${describeRfMode(event)} i=${event.intensity} <span class="raw">${esc(event.raw_hex)}</span> <span class="${ckClass}">${ckText}</span>`;
    list.appendChild(item);
  });
  list.scrollTop = 0;
}

// === Utilities ===

function esc(s: string) { return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;'); }

function formatDebugTime(ms: number) {
  return (ms / 1000).toFixed(3) + 's';
}

function describeRfMode(event: any) {
  if (event.mode) return event.mode;
  return 'unknown(' + event.mode_raw + ')';
}

function formatEventLogTime(entry: any) {
  if (entry.unix_ms != null) {
    const date = new Date(entry.unix_ms);
    const hh = String(date.getHours()).padStart(2, '0');
    const mm = String(date.getMinutes()).padStart(2, '0');
    const ss = String(date.getSeconds()).padStart(2, '0');
    const ms = String(date.getMilliseconds()).padStart(3, '0');
    return `${hh}:${mm}:${ss}.${ms}`;
  }
  return formatDebugTime(entry.monotonic_ms);
}

function formatEventLogSource(source: string) {
  if (source === 'local_ui') return 'local';
  if (source === 'remote_control') return 'remote';
  return 'system';
}

function formatEventLogBody(entry: any) {
  switch (entry.event) {
    case 'action': {
      const duration = entry.duration_ms >= 1000
        ? `${(entry.duration_ms / 1000).toFixed(1)}s`
        : `${entry.duration_ms}ms`;
      const intensity = entry.intensity == null ? '' : ` @ ${entry.intensity}%`;
      return `${describeMode(entry.mode)} ${entry.collar_name} ${duration}${intensity}`;
    }
    case 'preset_run': {
      // Use resolved_preset (actual random picks) if available, else look up from state
      const preset = entry.resolved_preset ?? state.presets.find((p: any) => p.name === entry.preset_name);
      const summary = preset ? ` (${describePresetSummary(preset)})` : '';
      return `Preset ${entry.preset_name} started${summary}`;
    }
    case 'ntp_sync':
      return `NTP sync via ${entry.server}`;
    case 'remote_control_connection':
      if (entry.connected) {
        return `Remote control connected ${entry.url}`;
      }
      return entry.reason
        ? `Remote control disconnected ${entry.reason}`
        : 'Remote control disconnected';
    default:
      return 'Unknown event';
  }
}

function parseCollarId(s: string) {
  s = s.trim();
  let n;
  if (s.startsWith('0x') || s.startsWith('0X')) {
    n = parseInt(s.slice(2), 16);
  } else if (/^[0-9a-fA-F]+$/.test(s) && /[a-fA-F]/.test(s)) {
    // Contains hex letters, treat as hex
    n = parseInt(s, 16);
  } else {
    n = parseInt(s, 10);
  }
  if (isNaN(n) || n < 0 || n > 0xFFFF) return null;
  return n;
}

function setupHoldButton(btn: HTMLButtonElement) {
  function collarCommandPayload() {
    const collar = btn.dataset.collar!;
    const mode = btn.dataset.mode;
    const intensityMode = getCollarPref(collar, 'intensityMode', 'fixed');
    const msg: any = { collar_name: collar, mode };
    if (intensityMode === 'random' || intensityMode === 'gaussian') {
      msg.intensity = parseInt(getCollarPref(collar, 'levelMin', '10'));
      msg.intensity_max = parseInt(getCollarPref(collar, 'levelMax', '60'));
      if (intensityMode === 'gaussian') {
        msg.intensity_distribution = 'gaussian';
      }
    } else {
      const slider = document.getElementById('slider-' + collar) as HTMLInputElement | null;
      msg.intensity = slider ? parseInt(slider.value) : 30;
    }
    return msg;
  }
  function start(e: Event) {
    e.preventDefault();
    if (isRfLocked()) return;
    if (activeHoldStops.has(stop)) return;
    activeHoldStops.add(stop);
    send({ type: 'start_action', ...collarCommandPayload() });
  }
  function stop() {
    if (!activeHoldStops.has(stop)) return;
    activeHoldStops.delete(stop);
    if (ws && ws.readyState === WebSocket.OPEN) {
      send({ type: 'stop_action', collar_name: btn.dataset.collar, mode: btn.dataset.mode });
    }
  }
  btn.addEventListener('mousedown', start);
  btn.addEventListener('touchstart', start, { passive: false });
  btn.addEventListener('mouseup', stop);
  btn.addEventListener('mouseleave', stop);
  btn.addEventListener('touchend', stop);
  btn.addEventListener('touchcancel', stop);
}

function describeMode(mode: string) {
  return (MODE_LABEL as any)[mode] || mode;
}

function describePresetSummary(preset: any): string {
  return preset.tracks.map((t: any) => {
    const steps = t.steps.map((s: any) => describePresetStep(s)).join(' > ');
    return `${t.collar_name}: ${steps}`;
  }).join(' | ');
}

function fmtDurMs(ms: number) {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

function describePresetStep(step: any) {
  const icon = (MODE_EMOJI as any)[step.mode] || step.mode;
  const hasDurRange = step.duration_max_ms != null && step.duration_max_ms > step.duration_ms;
  const hasIntRange = step.intensity_max != null && step.intensity_max > step.intensity;
  const durRangeIcon = step.duration_distribution === 'gaussian' ? '\uD83D\uDD14' : '\uD83C\uDFB2';
  const intRangeIcon = step.intensity_distribution === 'gaussian' ? '\uD83D\uDD14' : '\uD83C\uDFB2';
  const dur = hasDurRange
    ? `${durRangeIcon}${fmtDurMs(step.duration_ms)}-${fmtDurMs(step.duration_max_ms)}`
    : fmtDurMs(step.duration_ms);
  if (step.mode === 'pause' || step.mode === 'beep') return `${icon} ${dur}`;
  const int = hasIntRange
    ? `${intRangeIcon}${step.intensity}-${step.intensity_max}%`
    : `${step.intensity}%`;
  return `${icon} ${dur} @ ${int}`;
}

// === Expose globals for inline HTML handlers ===

const w = window as any;
w.showTab = showTab;
w.addRemote = addRemote;
w.openPresetEditor = openPresetEditor;
w.openRemoteEditor = openRemoteEditor;
w.closeRemoteEditor = closeRemoteEditor;
w.saveRemoteEditor = saveRemoteEditor;
w.toggleRfDebug = toggleRfDebug;
w.clearRfDebug = clearRfDebug;
w.runStressTest = runStressTest;
w.updateRemoteControlSettingsUi = updateRemoteControlSettingsUi;
w.updateNtpSettingsUi = updateNtpSettingsUi;
w.saveDeviceSettings = saveDeviceSettings;
w.rebootDevice = rebootDevice;
w.startOta = startOta;
w.doExport = doExport;
w.doImport = doImport;
w.stopAllRf = stopAllRf;
w.setCollarPref = setCollarPref;
w.updateIntensityMode = updateIntensityMode;
w.updateDurationMode = updateDurationMode;
w.updateRangeMin = updateRangeMin;
w.updateRangeMax = updateRangeMax;
w.stopPreset = stopPreset;

// === Init ===

renderAppVersion();
renderRemoteControlStatus();
renderEventLog();
connect();

// Tooltip: click/tap to show, click/tap elsewhere to hide
document.addEventListener('click', (e) => {
  const wrapper = document.getElementById('clients-wrapper');
  if (!wrapper) return;
  if (wrapper.contains(e.target as Node)) {
    wrapper.classList.toggle('show-tooltip');
  } else {
    wrapper.classList.remove('show-tooltip');
  }
});
