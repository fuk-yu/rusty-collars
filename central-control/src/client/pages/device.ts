import * as api from "../api.js";
import { state, showBanner, triggerRender } from "../state.js";
import * as ws from "../ws.js";
import { navigate } from "../router.js";
import { esc, formatCollarId, formatDuration } from "../utils.js";
import { openPresetEditor } from "../components/preset-editor.js";
import type { Collar, CommandMode, Preset, StartActionCommand, RunActionCommand } from "../../shared/protocol.js";
import type { CollarPermission, DevicePermission, DeviceSnapshot, ModeLimit, UserPreset } from "../../shared/types.js";

interface CollarState {
  intensity: number;
  intensityMax: number;
  intensityMode: "fixed" | "random";
  durationMs: number;
  durationMaxMs: number;
  durationMode: "fixed" | "held" | "random";
}

interface DevicePageState {
  uuid: string;
  isOwner: boolean;
  collarStates: Record<string, CollarState>;
  activeActions: Set<string>; // "collarName:mode"
  deviceUsers: { userId: string; login: string; hasPermission: boolean; permission: DevicePermission | null }[];
  ownerPresets: { preset: Preset; withinLimits: boolean }[];
  userPresets: UserPreset[];
  editingPermUserId: string | null;
  editingPermData: CollarPermission[];
}

function getCollarState(collarName: string): CollarState {
  if (!pageState.collarStates[collarName]) {
    pageState.collarStates[collarName] = { intensity: 30, intensityMax: 60, intensityMode: "fixed", durationMs: 1500, durationMaxMs: 5000, durationMode: "fixed" };
  }
  return pageState.collarStates[collarName]!;
}

let pageState: DevicePageState = {
  uuid: "",
  isOwner: false,
  collarStates: {},
  activeActions: new Set(),
  deviceUsers: [],
  ownerPresets: [],
  userPresets: [],
  editingPermUserId: null,
  editingPermData: [],
};

function getSnap(): DeviceSnapshot | null {
  return state.devices.get(pageState.uuid) ?? null;
}

export async function loadDeviceData(uuid: string): Promise<void> {
  pageState.uuid = uuid;
  const snap = getSnap();
  pageState.isOwner = snap?.isOwner ?? false;

  try {
    if (pageState.isOwner) {
      pageState.deviceUsers = await api.listDeviceUsers(uuid);
    }
    pageState.ownerPresets = await api.listOwnerPresets(uuid);
    pageState.userPresets = await api.listUserPresets(uuid);
  } catch (err) {
    showBanner("error", err instanceof Error ? err.message : "Failed to load device data");
  }
}

// ── Main render ──

export function renderDevicePage(): string {
  const snap = getSnap();

  if (!snap) {
    return `
      <div class="max-w-6xl mx-auto p-6">
        ${renderNav()}
        <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 text-center">
          <p class="text-gray-400">Device not found or not accessible. It may not have connected yet.</p>
          <p class="text-sm text-gray-500 mt-2 font-mono">${esc(pageState.uuid)}</p>
        </div>
      </div>`;
  }

  return `
    <div class="max-w-6xl mx-auto p-6 space-y-4">
      ${renderNav()}
      ${renderDeviceInfo(snap)}
      ${renderCollarControls(snap)}
      ${renderPresetsSection(snap)}
      ${snap.isOwner ? renderUserManagement() : ""}
    </div>`;
}

function renderNav(): string {
  return `
    <div class="flex items-center gap-4 mb-2">
      <a href="#/" class="text-blue-400 hover:underline text-sm">&larr; Dashboard</a>
      <span class="text-gray-600">|</span>
      <span class="text-sm text-gray-400">${esc(state.user?.login ?? "")}</span>
    </div>`;
}

function renderDeviceInfo(snap: DeviceSnapshot): string {
  const statusColor = snap.connected ? "bg-green-500" : "bg-gray-600";
  const statusText = snap.connected ? "Connected" : "Disconnected";

  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <div class="flex items-center gap-3">
          <span class="w-3 h-3 rounded-full ${statusColor}"></span>
          <h2 class="text-xl font-bold">${esc(snap.nickname)}</h2>
          <span class="text-xs text-gray-500">${statusText}</span>
        </div>
        <button id="stop-all-btn" class="bg-red-600 hover:bg-red-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">
          STOP ALL
        </button>
      </div>
      <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
        <div class="bg-gray-800 rounded-xl p-3 border border-gray-700">
          <div class="text-xs text-gray-500">UUID</div>
          <div class="text-sm font-mono mt-1 break-all">${esc(snap.uuid)}</div>
        </div>
        <div class="bg-gray-800 rounded-xl p-3 border border-gray-700">
          <div class="text-xs text-gray-500">Firmware</div>
          <div class="text-sm font-mono mt-1">${esc(snap.state?.app_version ?? "unknown")}</div>
        </div>
        <div class="bg-gray-800 rounded-xl p-3 border border-gray-700">
          <div class="text-xs text-gray-500">RTT</div>
          <div class="text-sm font-mono mt-1">${snap.rttMs !== null ? `${snap.rttMs}ms` : "n/a"}</div>
        </div>
        <div class="bg-gray-800 rounded-xl p-3 border border-gray-700">
          <div class="text-xs text-gray-500">Status</div>
          <div class="text-sm mt-1">${snap.state?.preset_running ? `Running: ${esc(snap.state.preset_running)}` : "Idle"}</div>
        </div>
      </div>
    </div>`;
}

// ── Per-collar controls with hold-to-activate ──

function getCollarPerm(snap: DeviceSnapshot, collarName: string): CollarPermission | undefined {
  return snap.permissions?.collars.find((c) => c.collarName === collarName);
}

function getVisibleCollars(snap: DeviceSnapshot): Collar[] {
  const collars = snap.state?.collars ?? [];
  if (snap.isOwner || !snap.permissions) return collars;
  const permitted = new Set(snap.permissions.collars.map((c) => c.collarName));
  return collars.filter((c) => permitted.has(c.name));
}

function renderCollarControls(snap: DeviceSnapshot): string {
  const collars = getVisibleCollars(snap);
  if (collars.length === 0) return "";

  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <h3 class="font-semibold mb-4">Collar Controls</h3>
      <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
        ${collars.map((c) => renderCollarPanel(c, snap)).join("")}
      </div>
    </div>`;
}

function renderCollarPanel(collar: Collar, snap: DeviceSnapshot): string {
  const perm = snap.isOwner ? undefined : getCollarPerm(snap, collar.name);
  const cs = getCollarState(collar.name);

  const shockAllowed = snap.isOwner || (perm?.shock !== null && perm?.shock !== undefined);
  const vibrateAllowed = snap.isOwner || (perm?.vibrate !== null && perm?.vibrate !== undefined);
  const beepAllowed = snap.isOwner || (perm?.beep !== null && perm?.beep !== undefined);

  const maxInt = snap.isOwner ? 99 : Math.max(perm?.shock?.maxIntensity ?? 0, perm?.vibrate?.maxIntensity ?? 0, 0);
  const effectiveMaxInt = maxInt > 0 ? maxInt : 99;
  const clampedInt = Math.min(cs.intensity, effectiveMaxInt);
  const clampedIntMax = Math.min(cs.intensityMax, effectiveMaxInt);
  const maxDur = snap.isOwner ? 30000 : Math.max(
    perm?.shock?.maxDurationMs ?? 0, perm?.vibrate?.maxDurationMs ?? 0, perm?.beep?.maxDurationMs ?? 0, 0);
  const effectiveMaxDur = maxDur > 0 ? maxDur : 30000;
  const clampedDur = Math.min(cs.durationMs, effectiveMaxDur);
  const clampedDurMax = Math.min(cs.durationMaxMs, effectiveMaxDur);
  const durSec = (clampedDur / 1000).toFixed(1);
  const durMaxSec = (clampedDurMax / 1000).toFixed(1);
  const maxDurSec = (effectiveMaxDur / 1000).toFixed(1);

  const isHeld = cs.durationMode === "held";
  const isIntRandom = cs.intensityMode === "random";
  const isDurRandom = cs.durationMode === "random";

  const intensityDisplay = isIntRandom ? `${clampedInt}-${clampedIntMax}` : String(clampedInt);
  const durationDisplay = isHeld ? "hold" : isDurRandom ? `${durSec}s-${durMaxSec}s` : `${durSec}s`;

  const limitsText = !snap.isOwner && perm ? buildLimitsText(perm) : "";

  return `
    <div class="bg-gray-800 border border-gray-700 rounded-xl p-4">
      <div class="flex justify-between items-center mb-3">
        <span class="font-medium">${esc(collar.name)}</span>
        <span class="text-xs text-gray-500">${esc(formatCollarId(collar.collar_id))} CH${collar.channel + 1}</span>
      </div>
      <div class="flex items-center gap-3 mb-2">
        <span class="text-xs text-gray-500 w-14 shrink-0">Level</span>
        <select class="collar-intensity-mode bg-gray-700 border border-gray-600 rounded text-xs px-1 py-0.5 text-gray-300" data-collar="${esc(collar.name)}">
          <option value="fixed" ${cs.intensityMode === "fixed" ? "selected" : ""}>Fixed</option>
          <option value="random" ${cs.intensityMode === "random" ? "selected" : ""}>Random</option>
        </select>
        <input type="range" min="0" max="${effectiveMaxInt}" value="${clampedInt}"
          class="collar-intensity flex-1 accent-blue-500" data-collar="${esc(collar.name)}" style="${isIntRandom ? "display:none" : ""}">
        <div class="cc-range-slider flex-1" data-collar="${esc(collar.name)}" data-field="intensity" style="${isIntRandom ? "" : "display:none"}">
          <input type="range" class="range-min" min="0" max="${effectiveMaxInt}" value="${clampedInt}">
          <input type="range" class="range-max" min="0" max="${effectiveMaxInt}" value="${clampedIntMax}">
        </div>
        <span class="collar-intensity-val text-sm font-mono w-12 text-right text-gray-300" data-collar="${esc(collar.name)}">${intensityDisplay}</span>
      </div>
      <div class="flex items-center gap-3 mb-3">
        <span class="text-xs text-gray-500 w-14 shrink-0">Duration</span>
        <select class="collar-duration-mode bg-gray-700 border border-gray-600 rounded text-xs px-1 py-0.5 text-gray-300" data-collar="${esc(collar.name)}">
          <option value="fixed" ${cs.durationMode === "fixed" ? "selected" : ""}>Fixed</option>
          <option value="held" ${cs.durationMode === "held" ? "selected" : ""}>Held</option>
          <option value="random" ${cs.durationMode === "random" ? "selected" : ""}>Random</option>
        </select>
        <input type="range" min="0.5" max="${maxDurSec}" step="0.5" value="${durSec}"
          class="collar-duration flex-1 accent-blue-500" data-collar="${esc(collar.name)}" style="${cs.durationMode === "fixed" ? "" : "display:none"}">
        <div class="cc-range-slider flex-1" data-collar="${esc(collar.name)}" data-field="duration" style="${isDurRandom ? "" : "display:none"}">
          <input type="range" class="range-min" min="0.5" max="${maxDurSec}" step="0.5" value="${durSec}">
          <input type="range" class="range-max" min="0.5" max="${maxDurSec}" step="0.5" value="${durMaxSec}">
        </div>
        <span class="collar-duration-val text-sm font-mono w-16 text-right text-gray-300" data-collar="${esc(collar.name)}">${durationDisplay}</span>
      </div>
      <div class="grid grid-cols-3 gap-2">
        ${renderActionBtn(collar.name, "shock", shockAllowed, isHeld)}
        ${renderActionBtn(collar.name, "vibrate", vibrateAllowed, isHeld)}
        ${renderActionBtn(collar.name, "beep", beepAllowed, isHeld)}
      </div>
      ${limitsText ? `<div class="text-xs text-gray-500 mt-3">${limitsText}</div>` : ""}
    </div>`;
}

function renderActionBtn(collarName: string, mode: CommandMode, allowed: boolean, isHeld: boolean): string {
  const active = pageState.activeActions.has(`${collarName}:${mode}`);
  const colorMap: Record<string, { normal: string; active: string }> = {
    shock: { normal: "bg-red-800 hover:bg-red-700 border-red-700", active: "bg-red-500 ring-2 ring-red-400 border-red-400" },
    vibrate: { normal: "bg-blue-800 hover:bg-blue-700 border-blue-700", active: "bg-blue-500 ring-2 ring-blue-400 border-blue-400" },
    beep: { normal: "bg-yellow-800 hover:bg-yellow-700 border-yellow-700", active: "bg-yellow-500 ring-2 ring-yellow-400 border-yellow-400" },
  };
  const colors = colorMap[mode]!;
  const cls = active ? colors.active : colors.normal;
  const hint = isHeld ? "hold" : "timed";

  return `
    <button class="action-btn ${cls} ${allowed ? "" : "opacity-25 cursor-not-allowed"} text-white text-sm font-semibold rounded-xl py-3 border select-none transition-all"
      data-collar="${esc(collarName)}" data-mode="${mode}" data-held="${isHeld ? "1" : "0"}" ${allowed ? "" : "disabled"}>
      ${mode[0]!.toUpperCase() + mode.slice(1)}
      <div class="text-[10px] font-normal opacity-60 mt-0.5">${hint}</div>
    </button>`;
}

function buildLimitsText(perm: CollarPermission): string {
  const parts: string[] = [];
  if (perm.shock) parts.push(`shock \u2264${perm.shock.maxIntensity} / ${formatDuration(perm.shock.maxDurationMs)}`);
  if (perm.vibrate) parts.push(`vibrate \u2264${perm.vibrate.maxIntensity} / ${formatDuration(perm.vibrate.maxDurationMs)}`);
  if (perm.beep) parts.push(`beep \u2264${formatDuration(perm.beep.maxDurationMs)}`);
  return parts.length > 0 ? `Limits: ${parts.join(", ")}` : "No modes permitted";
}

// ── Presets ──

function renderPresetsSection(snap: DeviceSnapshot): string {
  if (snap.isOwner) return renderOwnerPresets(snap);
  return renderInviteePresets(snap);
}

function renderOwnerPresets(snap: DeviceSnapshot): string {
  const presets = snap.state?.presets ?? [];

  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <h3 class="font-semibold">Device Presets</h3>
        <div class="flex gap-2">
          <button id="stop-preset-btn" class="text-sm text-gray-400 hover:text-white transition-colors">Stop Preset</button>
          <button id="new-device-preset-btn" class="bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">
            + New Preset
          </button>
        </div>
      </div>
      ${presets.length === 0
        ? `<p class="text-sm text-gray-500">No presets on device</p>`
        : `<div class="space-y-2">${presets.map((p, i) => renderOwnerPresetCard(p, i, presets.length)).join("")}</div>`}
    </div>`;
}

function renderOwnerPresetCard(preset: Preset, index: number, total: number): string {
  const desc = describePreset(preset);
  return `
    <div class="bg-gray-800 border border-gray-700 rounded-xl p-4">
      <div class="flex items-center justify-between mb-1">
        <span class="font-medium">${esc(preset.name)}</span>
        <span class="text-xs text-gray-500">${preset.tracks.length} track${preset.tracks.length === 1 ? "" : "s"}</span>
      </div>
      <div class="text-xs text-gray-500 mb-3">${esc(desc)}</div>
      <div class="flex gap-2 flex-wrap">
        <button class="run-device-preset-btn bg-blue-600 hover:bg-blue-700 text-white text-xs rounded-lg px-3 py-1.5" data-preset-name="${esc(preset.name)}">Run</button>
        <button class="edit-device-preset-btn bg-gray-700 hover:bg-gray-600 text-white text-xs rounded-lg px-3 py-1.5" data-preset-name="${esc(preset.name)}">Edit</button>
        <button class="move-preset-btn bg-gray-700 hover:bg-gray-600 text-white text-xs rounded-lg px-2 py-1.5 ${index === 0 ? "opacity-30 cursor-not-allowed" : ""}" data-preset-name="${esc(preset.name)}" data-dir="-1" ${index === 0 ? "disabled" : ""}>Up</button>
        <button class="move-preset-btn bg-gray-700 hover:bg-gray-600 text-white text-xs rounded-lg px-2 py-1.5 ${index === total - 1 ? "opacity-30 cursor-not-allowed" : ""}" data-preset-name="${esc(preset.name)}" data-dir="1" ${index === total - 1 ? "disabled" : ""}>Dn</button>
        <button class="delete-device-preset-btn bg-red-700 hover:bg-red-600 text-white text-xs rounded-lg px-3 py-1.5" data-preset-name="${esc(preset.name)}">Del</button>
      </div>
    </div>`;
}

function renderInviteePresets(snap: DeviceSnapshot): string {
  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <h3 class="font-semibold">Owner's Presets</h3>
        <button id="stop-preset-btn" class="text-sm text-gray-400 hover:text-white transition-colors">Stop Preset</button>
      </div>
      ${pageState.ownerPresets.length === 0
        ? `<p class="text-sm text-gray-500">No presets available</p>`
        : `<div class="space-y-2">${pageState.ownerPresets.map((op) => renderInviteePresetCard(op.preset, op.withinLimits)).join("")}</div>`}
    </div>
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <h3 class="font-semibold">My Presets</h3>
        <button id="new-user-preset-btn" class="bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">
          + New Preset
        </button>
      </div>
      ${pageState.userPresets.length === 0
        ? `<p class="text-sm text-gray-500">No custom presets yet</p>`
        : `<div class="space-y-2">${pageState.userPresets.map(renderUserPresetCard).join("")}</div>`}
    </div>`;
}

function renderInviteePresetCard(preset: Preset, canRun: boolean): string {
  const desc = describePreset(preset);
  return `
    <div class="bg-gray-800 border border-gray-700 rounded-xl p-4">
      <div class="flex items-center justify-between mb-1">
        <span class="font-medium">${esc(preset.name)}</span>
        <span class="text-xs text-gray-500">${preset.tracks.length} track${preset.tracks.length === 1 ? "" : "s"}</span>
      </div>
      <div class="text-xs text-gray-500 mb-3">${esc(desc)}</div>
      <div class="flex gap-2 items-center">
        <button class="run-device-preset-btn bg-blue-600 hover:bg-blue-700 text-white text-xs rounded-lg px-3 py-1.5 ${canRun ? "" : "opacity-40 cursor-not-allowed"}" data-preset-name="${esc(preset.name)}" ${canRun ? "" : "disabled"}>Run</button>
        ${!canRun ? `<span class="text-xs text-red-400">Exceeds your limits</span>` : ""}
      </div>
    </div>`;
}

function renderUserPresetCard(up: UserPreset): string {
  const desc = describePreset(up.preset);
  return `
    <div class="bg-gray-800 border border-gray-700 rounded-xl p-4">
      <div class="flex items-center justify-between mb-1">
        <span class="font-medium">${esc(up.preset.name)}</span>
        <span class="text-xs text-gray-500">${up.preset.tracks.length} track${up.preset.tracks.length === 1 ? "" : "s"}</span>
      </div>
      <div class="text-xs text-gray-500 mb-3">${esc(desc)}</div>
      <div class="flex gap-2">
        <button class="run-user-preset-btn bg-blue-600 hover:bg-blue-700 text-white text-xs rounded-lg px-3 py-1.5" data-preset-id="${esc(up.id)}">Run</button>
        <button class="edit-user-preset-btn bg-gray-700 hover:bg-gray-600 text-white text-xs rounded-lg px-3 py-1.5" data-preset-id="${esc(up.id)}">Edit</button>
        <button class="delete-user-preset-btn bg-red-700 hover:bg-red-600 text-white text-xs rounded-lg px-3 py-1.5" data-preset-id="${esc(up.id)}">Del</button>
      </div>
    </div>`;
}

function describePreset(preset: Preset): string {
  return preset.tracks.map((t) =>
    `${t.collar_name}: ${t.steps.map((s) => `${s.mode} ${formatDuration(s.duration_ms)}`).join(" > ")}`
  ).join(" | ");
}

// ── User management (owner only) ──

function renderUserManagement(): string {
  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <h3 class="font-semibold mb-4">User Access</h3>
      ${pageState.deviceUsers.length === 0
        ? `<p class="text-sm text-gray-500">No users have accepted invitations yet. Create an invitation from the dashboard.</p>`
        : `<div class="space-y-3">${pageState.deviceUsers.map(renderUserRow).join("")}</div>`}
    </div>`;
}

function renderUserRow(u: { userId: string; login: string; hasPermission: boolean; permission: DevicePermission | null }): string {
  const isEditing = pageState.editingPermUserId === u.userId;
  return `
    <div class="bg-gray-800 border border-gray-700 rounded-xl p-4">
      <div class="flex items-center justify-between mb-2">
        <span class="font-medium">${esc(u.login)}</span>
        <div class="flex gap-2">
          <button class="edit-perm-btn bg-blue-600 hover:bg-blue-700 text-white text-xs rounded-lg px-3 py-1.5" data-user-id="${esc(u.userId)}">
            ${u.hasPermission ? "Edit Permissions" : "Set Permissions"}
          </button>
          ${u.hasPermission ? `<button class="revoke-perm-btn bg-red-700 hover:bg-red-600 text-white text-xs rounded-lg px-3 py-1.5" data-user-id="${esc(u.userId)}">Revoke</button>` : ""}
        </div>
      </div>
      ${u.hasPermission && u.permission ? renderPermSummary(u.permission) : `<p class="text-xs text-gray-500">No permissions set</p>`}
      ${isEditing ? renderPermEditor(u.userId) : ""}
    </div>`;
}

function renderPermSummary(perm: DevicePermission): string {
  if (perm.collars.length === 0) return `<p class="text-xs text-gray-500">No collars configured</p>`;
  return `<div class="space-y-1">${perm.collars.map((c) => {
    const parts: string[] = [];
    if (c.shock) parts.push(`shock(\u2264${c.shock.maxIntensity}, ${formatDuration(c.shock.maxDurationMs)})`);
    if (c.vibrate) parts.push(`vibrate(\u2264${c.vibrate.maxIntensity}, ${formatDuration(c.vibrate.maxDurationMs)})`);
    if (c.beep) parts.push(`beep(${formatDuration(c.beep.maxDurationMs)})`);
    return `<div class="text-xs text-gray-400">${esc(c.collarName)}: ${parts.length > 0 ? parts.join(", ") : "no modes"}</div>`;
  }).join("")}</div>`;
}

function renderPermEditor(userId: string): string {
  const snap = getSnap();
  const collars = snap?.state?.collars ?? [];
  return `
    <div class="mt-3 bg-gray-900 border border-gray-700 rounded-lg p-4" id="perm-editor">
      <h4 class="text-sm font-semibold mb-3">Edit Permissions</h4>
      ${collars.map((collar) => {
        const existing = pageState.editingPermData.find((p) => p.collarName === collar.name);
        const has = existing !== undefined;
        return `
          <div class="mb-3 bg-gray-800 rounded-lg p-3 border border-gray-700">
            <label class="flex items-center gap-2 mb-2">
              <input type="checkbox" class="perm-collar-toggle" data-collar="${esc(collar.name)}" ${has ? "checked" : ""}>
              <span class="font-medium text-sm">${esc(collar.name)}</span>
            </label>
            <div class="perm-collar-modes ${has ? "" : "hidden"}" data-collar-modes="${esc(collar.name)}">
              ${renderModePerm("shock", collar.name, existing?.shock)}
              ${renderModePerm("vibrate", collar.name, existing?.vibrate)}
              ${renderBeepPerm(collar.name, existing?.beep)}
            </div>
          </div>`;
      }).join("")}
      <div class="flex gap-2 mt-3">
        <button id="save-perm-btn" class="bg-green-600 hover:bg-green-700 text-white text-sm rounded-lg px-4 py-2" data-user-id="${esc(userId)}">Save</button>
        <button id="cancel-perm-btn" class="bg-gray-700 hover:bg-gray-600 text-white text-sm rounded-lg px-4 py-2">Cancel</button>
      </div>
    </div>`;
}

function renderModePerm(mode: string, collarName: string, limit: ModeLimit | null | undefined): string {
  const on = limit !== undefined && limit !== null;
  return `
    <div class="flex items-center gap-3 mb-1">
      <label class="flex items-center gap-1 w-20"><input type="checkbox" class="perm-mode-toggle" data-collar="${esc(collarName)}" data-mode="${mode}" ${on ? "checked" : ""}><span class="text-xs text-gray-400">${mode}</span></label>
      <label class="flex items-center gap-1"><span class="text-xs text-gray-500">max lvl</span><input type="number" min="0" max="99" value="${on ? limit!.maxIntensity : 50}" class="perm-intensity bg-gray-700 border border-gray-600 rounded px-2 py-1 text-xs w-16" data-collar="${esc(collarName)}" data-mode="${mode}" ${on ? "" : "disabled"}></label>
      <label class="flex items-center gap-1"><span class="text-xs text-gray-500">max ms</span><input type="number" min="100" max="30000" step="100" value="${on ? limit!.maxDurationMs : 5000}" class="perm-duration bg-gray-700 border border-gray-600 rounded px-2 py-1 text-xs w-20" data-collar="${esc(collarName)}" data-mode="${mode}" ${on ? "" : "disabled"}></label>
    </div>`;
}

function renderBeepPerm(collarName: string, limit: { maxDurationMs: number } | null | undefined): string {
  const on = limit !== undefined && limit !== null;
  return `
    <div class="flex items-center gap-3 mb-1">
      <label class="flex items-center gap-1 w-20"><input type="checkbox" class="perm-mode-toggle" data-collar="${esc(collarName)}" data-mode="beep" ${on ? "checked" : ""}><span class="text-xs text-gray-400">beep</span></label>
      <label class="flex items-center gap-1"><span class="text-xs text-gray-500">max ms</span><input type="number" min="100" max="30000" step="100" value="${on ? limit!.maxDurationMs : 5000}" class="perm-duration bg-gray-700 border border-gray-600 rounded px-2 py-1 text-xs w-20" data-collar="${esc(collarName)}" data-mode="beep" ${on ? "" : "disabled"}></label>
    </div>`;
}

// ── Event binding ──

function bindHoldButton(btn: HTMLElement, onStart: () => void, onStop: () => void): void {
  let active = false;

  function start(e: Event) {
    e.preventDefault();
    if (active) return;
    active = true;
    onStart();
  }

  function stop() {
    if (!active) return;
    active = false;
    onStop();
  }

  btn.addEventListener("mousedown", start);
  btn.addEventListener("mouseup", stop);
  btn.addEventListener("mouseleave", stop);
  btn.addEventListener("touchstart", start, { passive: false });
  btn.addEventListener("touchend", stop);
  btn.addEventListener("touchcancel", stop);
  btn.addEventListener("contextmenu", (e) => e.preventDefault());
}

export function bindDeviceEvents(root: HTMLElement): void {
  // Stop all
  root.querySelector("#stop-all-btn")?.addEventListener("click", () => {
    ws.sendDeviceCommand(pageState.uuid, { type: "stop_all" });
    pageState.activeActions.clear();
  });

  // Stop preset
  root.querySelector("#stop-preset-btn")?.addEventListener("click", () => {
    ws.sendDeviceCommand(pageState.uuid, { type: "stop_preset" });
  });

  // Intensity mode selects
  root.querySelectorAll(".collar-intensity-mode").forEach((sel) => {
    sel.addEventListener("change", (e) => {
      const select = e.target as HTMLSelectElement;
      const collarName = select.dataset.collar!;
      const cs = getCollarState(collarName);
      cs.intensityMode = select.value as "fixed" | "random";
      triggerRender();
    });
  });

  // Per-collar intensity sliders (fixed mode)
  root.querySelectorAll(".collar-intensity").forEach((slider) => {
    slider.addEventListener("input", (e) => {
      const input = e.target as HTMLInputElement;
      const collarName = input.dataset.collar!;
      const cs = getCollarState(collarName);
      cs.intensity = parseInt(input.value, 10);
      const valEl = root.querySelector(`.collar-intensity-val[data-collar="${collarName}"]`);
      if (valEl) valEl.textContent = String(cs.intensity);
    });
  });

  // Intensity range sliders (random mode, dual-thumb)
  root.querySelectorAll('.cc-range-slider[data-field="intensity"]').forEach((div) => {
    const collarName = (div as HTMLElement).dataset.collar!;
    const cs = getCollarState(collarName);
    const minInput = div.querySelector(".range-min") as HTMLInputElement;
    const maxInput = div.querySelector(".range-max") as HTMLInputElement;
    const valEl = root.querySelector(`.collar-intensity-val[data-collar="${collarName}"]`);
    minInput.addEventListener("input", () => {
      let v = parseInt(minInput.value, 10);
      if (v > parseInt(maxInput.value, 10)) { v = parseInt(maxInput.value, 10); minInput.value = String(v); }
      cs.intensity = v;
      if (valEl) valEl.textContent = `${cs.intensity}-${cs.intensityMax}`;
    });
    maxInput.addEventListener("input", () => {
      let v = parseInt(maxInput.value, 10);
      if (v < parseInt(minInput.value, 10)) { v = parseInt(minInput.value, 10); maxInput.value = String(v); }
      cs.intensityMax = v;
      if (valEl) valEl.textContent = `${cs.intensity}-${cs.intensityMax}`;
    });
  });

  // Duration mode selects
  root.querySelectorAll(".collar-duration-mode").forEach((sel) => {
    sel.addEventListener("change", (e) => {
      const select = e.target as HTMLSelectElement;
      const collarName = select.dataset.collar!;
      const cs = getCollarState(collarName);
      cs.durationMode = select.value as "fixed" | "held" | "random";
      triggerRender();
    });
  });

  // Per-collar duration sliders (fixed mode)
  root.querySelectorAll("input.collar-duration").forEach((slider) => {
    slider.addEventListener("input", (e) => {
      const input = e.target as HTMLInputElement;
      const collarName = input.dataset.collar!;
      const cs = getCollarState(collarName);
      cs.durationMs = Math.round(parseFloat(input.value) * 1000);
      const valEl = root.querySelector(`.collar-duration-val[data-collar="${collarName}"]`);
      if (valEl) valEl.textContent = `${(cs.durationMs / 1000).toFixed(1)}s`;
    });
  });

  // Duration range sliders (random mode, dual-thumb)
  root.querySelectorAll('.cc-range-slider[data-field="duration"]').forEach((div) => {
    const collarName = (div as HTMLElement).dataset.collar!;
    const cs = getCollarState(collarName);
    const minInput = div.querySelector(".range-min") as HTMLInputElement;
    const maxInput = div.querySelector(".range-max") as HTMLInputElement;
    const valEl = root.querySelector(`.collar-duration-val[data-collar="${collarName}"]`);
    minInput.addEventListener("input", () => {
      let v = parseFloat(minInput.value);
      if (v > parseFloat(maxInput.value)) { v = parseFloat(maxInput.value); minInput.value = String(v); }
      cs.durationMs = Math.round(v * 1000);
      if (valEl) valEl.textContent = `${(cs.durationMs / 1000).toFixed(1)}s-${(cs.durationMaxMs / 1000).toFixed(1)}s`;
    });
    maxInput.addEventListener("input", () => {
      let v = parseFloat(maxInput.value);
      if (v < parseFloat(minInput.value)) { v = parseFloat(minInput.value); maxInput.value = String(v); }
      cs.durationMaxMs = Math.round(v * 1000);
      if (valEl) valEl.textContent = `${(cs.durationMs / 1000).toFixed(1)}s-${(cs.durationMaxMs / 1000).toFixed(1)}s`;
    });
  });

  // Action buttons — timed click or hold-to-activate depending on durationMode
  root.querySelectorAll(".action-btn").forEach((btn) => {
    const el = btn as HTMLElement;
    const collarName = el.dataset.collar!;
    const mode = el.dataset.mode! as CommandMode;
    const cs = getCollarState(collarName);

    if (cs.durationMode === "held") {
      bindHoldButton(
        el,
        () => {
          const intensity = mode === "beep" ? 0 : cs.intensity;
          const cmd: StartActionCommand = { type: "start_action", collar_name: collarName, mode, intensity };
          if (mode !== "beep" && cs.intensityMode === "random") {
            cmd.intensity_max = cs.intensityMax;
          }
          pageState.activeActions.add(`${collarName}:${mode}`);
          try {
            ws.sendDeviceCommand(pageState.uuid, cmd);
          } catch {
            showBanner("error", "Not connected");
          }
          triggerRender();
        },
        () => {
          pageState.activeActions.delete(`${collarName}:${mode}`);
          try {
            ws.sendDeviceCommand(pageState.uuid, { type: "stop_action", collar_name: collarName, mode });
          } catch {
            // best effort stop
          }
          triggerRender();
        },
      );
    } else {
      el.addEventListener("click", () => {
        const intensity = mode === "beep" ? 0 : cs.intensity;
        const cmd: RunActionCommand = {
          type: "run_action", collar_name: collarName, mode, intensity, duration_ms: cs.durationMs,
        };
        if (mode !== "beep" && cs.intensityMode === "random") {
          cmd.intensity_max = cs.intensityMax;
        }
        if (cs.durationMode === "random") {
          cmd.duration_max_ms = cs.durationMaxMs;
        }
        try {
          ws.sendDeviceCommand(pageState.uuid, cmd);
        } catch {
          showBanner("error", "Not connected");
        }
      });
    }
  });

  // ── Owner preset buttons ──
  const snap = getSnap();
  const collars = snap?.state?.collars ?? [];

  root.querySelector("#new-device-preset-btn")?.addEventListener("click", () => {
    openPresetEditor(null, null, collars, async (preset) => {
      ws.sendDeviceCommand(pageState.uuid, { type: "save_preset", original_name: null, preset });
    }, undefined, pageState.uuid);
  });

  root.querySelectorAll(".edit-device-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const name = (btn as HTMLElement).dataset.presetName!;
      const preset = snap?.state?.presets.find((p) => p.name === name);
      if (!preset) return;
      openPresetEditor(preset, name, collars, async (edited) => {
        ws.sendDeviceCommand(pageState.uuid, { type: "save_preset", original_name: name, preset: edited });
      }, undefined, pageState.uuid);
    });
  });

  root.querySelectorAll(".delete-device-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const name = (btn as HTMLElement).dataset.presetName!;
      ws.sendDeviceCommand(pageState.uuid, { type: "delete_preset", name });
    });
  });

  root.querySelectorAll(".move-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const name = (btn as HTMLElement).dataset.presetName!;
      const dir = parseInt((btn as HTMLElement).dataset.dir!, 10);
      const presets = snap?.state?.presets ?? [];
      const names = presets.map((p) => p.name);
      const i = names.indexOf(name);
      if (i < 0) return;
      const j = i + dir;
      if (j < 0 || j >= names.length) return;
      [names[i], names[j]] = [names[j]!, names[i]!];
      ws.sendDeviceCommand(pageState.uuid, { type: "reorder_presets", names });
    });
  });

  // Run device preset
  root.querySelectorAll(".run-device-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const name = (btn as HTMLElement).dataset.presetName;
      if (name) ws.sendDeviceCommand(pageState.uuid, { type: "run_preset", name });
    });
  });

  // ── Invitee user preset buttons ──
  root.querySelector("#new-user-preset-btn")?.addEventListener("click", () => {
    openPresetEditor(null, null, collars, async (preset) => {
      await api.createUserPreset(pageState.uuid, preset);
      await loadDeviceData(pageState.uuid);
      triggerRender();
    }, snap?.permissions ?? undefined, pageState.uuid);
  });

  root.querySelectorAll(".run-user-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = (btn as HTMLElement).dataset.presetId;
      const up = pageState.userPresets.find((p) => p.id === id);
      if (up) {
        ws.sendDeviceCommand(pageState.uuid, { type: "save_preset", original_name: null, preset: up.preset });
        setTimeout(() => ws.sendDeviceCommand(pageState.uuid, { type: "run_preset", name: up.preset.name }), 200);
      }
    });
  });

  root.querySelectorAll(".edit-user-preset-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = (btn as HTMLElement).dataset.presetId;
      const up = pageState.userPresets.find((p) => p.id === id);
      if (!up) return;
      openPresetEditor(up.preset, up.preset.name, collars, async (preset) => {
        await api.updateUserPreset(pageState.uuid, up.id, preset);
        await loadDeviceData(pageState.uuid);
        triggerRender();
      }, snap?.permissions ?? undefined, pageState.uuid);
    });
  });

  root.querySelectorAll(".delete-user-preset-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const id = (btn as HTMLElement).dataset.presetId;
      if (!id) return;
      try {
        await api.deleteUserPreset(pageState.uuid, id);
        await loadDeviceData(pageState.uuid);
        triggerRender();
      } catch (err) {
        showBanner("error", err instanceof Error ? err.message : "Failed to delete");
      }
    });
  });

  // ── Permission management ──
  root.querySelectorAll(".edit-perm-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const userId = (btn as HTMLElement).dataset.userId!;
      const user = pageState.deviceUsers.find((u) => u.userId === userId);
      pageState.editingPermUserId = userId;
      pageState.editingPermData = user?.permission?.collars ? JSON.parse(JSON.stringify(user.permission.collars)) : [];
      triggerRender();
    });
  });

  root.querySelectorAll(".revoke-perm-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const userId = (btn as HTMLElement).dataset.userId!;
      try {
        await api.deletePermission(pageState.uuid, userId);
        await loadDeviceData(pageState.uuid);
        triggerRender();
      } catch (err) {
        showBanner("error", err instanceof Error ? err.message : "Failed to revoke");
      }
    });
  });

  bindPermEditorEvents(root);
}

function bindPermEditorEvents(root: HTMLElement): void {
  root.querySelectorAll(".perm-collar-toggle").forEach((checkbox) => {
    checkbox.addEventListener("change", (e) => {
      const target = e.target as HTMLInputElement;
      const cn = target.dataset.collar!;
      const modesDiv = root.querySelector(`[data-collar-modes="${cn}"]`);
      if (target.checked) {
        modesDiv?.classList.remove("hidden");
        if (!pageState.editingPermData.find((p) => p.collarName === cn)) {
          pageState.editingPermData.push({ collarName: cn, shock: null, vibrate: null, beep: null });
        }
      } else {
        modesDiv?.classList.add("hidden");
        pageState.editingPermData = pageState.editingPermData.filter((p) => p.collarName !== cn);
      }
    });
  });

  root.querySelectorAll(".perm-mode-toggle").forEach((checkbox) => {
    checkbox.addEventListener("change", (e) => {
      const target = e.target as HTMLInputElement;
      const cn = target.dataset.collar!;
      const mode = target.dataset.mode!;
      const cp = pageState.editingPermData.find((p) => p.collarName === cn);
      if (!cp) return;

      const intInput = root.querySelector(`.perm-intensity[data-collar="${cn}"][data-mode="${mode}"]`) as HTMLInputElement | null;
      const durInput = root.querySelector(`.perm-duration[data-collar="${cn}"][data-mode="${mode}"]`) as HTMLInputElement | null;

      if (target.checked) {
        if (intInput) intInput.disabled = false;
        if (durInput) durInput.disabled = false;
        if (mode === "beep") cp.beep = { maxDurationMs: parseInt(durInput?.value ?? "5000", 10) };
        else {
          const limit: ModeLimit = { maxIntensity: parseInt(intInput?.value ?? "50", 10), maxDurationMs: parseInt(durInput?.value ?? "5000", 10) };
          if (mode === "shock") cp.shock = limit; else cp.vibrate = limit;
        }
      } else {
        if (intInput) intInput.disabled = true;
        if (durInput) durInput.disabled = true;
        if (mode === "shock") cp.shock = null;
        else if (mode === "vibrate") cp.vibrate = null;
        else cp.beep = null;
      }
    });
  });

  root.querySelector("#save-perm-btn")?.addEventListener("click", async () => {
    const userId = (root.querySelector("#save-perm-btn") as HTMLElement)?.dataset.userId;
    if (!userId) return;

    const collars: CollarPermission[] = [];
    for (const cp of pageState.editingPermData) {
      const collarPerm: CollarPermission = { collarName: cp.collarName, shock: null, vibrate: null, beep: null };
      for (const mode of ["shock", "vibrate"] as const) {
        const toggle = root.querySelector(`.perm-mode-toggle[data-collar="${cp.collarName}"][data-mode="${mode}"]`) as HTMLInputElement | null;
        if (toggle?.checked) {
          const intEl = root.querySelector(`.perm-intensity[data-collar="${cp.collarName}"][data-mode="${mode}"]`) as HTMLInputElement | null;
          const durEl = root.querySelector(`.perm-duration[data-collar="${cp.collarName}"][data-mode="${mode}"]`) as HTMLInputElement | null;
          collarPerm[mode] = { maxIntensity: parseInt(intEl?.value ?? "50", 10), maxDurationMs: parseInt(durEl?.value ?? "5000", 10) };
        }
      }
      const beepToggle = root.querySelector(`.perm-mode-toggle[data-collar="${cp.collarName}"][data-mode="beep"]`) as HTMLInputElement | null;
      if (beepToggle?.checked) {
        const durEl = root.querySelector(`.perm-duration[data-collar="${cp.collarName}"][data-mode="beep"]`) as HTMLInputElement | null;
        collarPerm.beep = { maxDurationMs: parseInt(durEl?.value ?? "5000", 10) };
      }
      collars.push(collarPerm);
    }

    try {
      await api.setPermission(pageState.uuid, userId, collars);
      pageState.editingPermUserId = null;
      pageState.editingPermData = [];
      await loadDeviceData(pageState.uuid);
      showBanner("info", "Permissions saved");
      triggerRender();
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Failed to save");
    }
  });

  root.querySelector("#cancel-perm-btn")?.addEventListener("click", () => {
    pageState.editingPermUserId = null;
    pageState.editingPermData = [];
    triggerRender();
  });
}
