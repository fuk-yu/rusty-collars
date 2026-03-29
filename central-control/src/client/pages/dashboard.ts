import * as api from "../api.js";
import { state, showBanner, triggerRender } from "../state.js";
import { navigate } from "../router.js";
import * as ws from "../ws.js";
import { esc } from "../utils.js";
import type { ApiDevice, ApiInvitation } from "../../shared/types.js";

interface DashboardData {
  ownedDevices: ApiDevice[];
  sharedDevices: ApiDevice[];
  invitations: ApiInvitation[];
  newInviteLink: string | null;
}

let dashData: DashboardData = {
  ownedDevices: [],
  sharedDevices: [],
  invitations: [],
  newInviteLink: null,
};

export function renderDashboard(): string {
  const user = state.user;
  if (!user) return "";

  return `
    <div class="max-w-6xl mx-auto p-6 space-y-6">
      ${renderNav(user.login, user.totpEnabled)}

      <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
        ${renderOwnedDevices(dashData.ownedDevices)}
        ${renderSharedDevices(dashData.sharedDevices)}
      </div>

      ${renderInvitations(dashData.invitations, dashData.newInviteLink)}
    </div>`;
}

function renderNav(login: string, totpEnabled: boolean): string {
  return `
    <div class="flex items-center justify-between">
      <h1 class="text-2xl font-bold">Central Control</h1>
      <div class="flex items-center gap-4">
        ${!totpEnabled ? `<a href="#/setup-2fa" class="text-sm text-yellow-400 hover:underline">Enable 2FA</a>` : ""}
        <span class="text-sm text-gray-400">${esc(login)}</span>
        <button id="logout-btn" class="text-sm text-gray-400 hover:text-white transition-colors">Logout</button>
      </div>
    </div>`;
}

function renderOwnedDevices(devices: ApiDevice[]): string {
  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-lg font-semibold">My Devices</h2>
        <button id="add-device-btn" class="bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">
          + Add Controller
        </button>
      </div>
      <div id="add-device-form" class="hidden mb-4">
        <form id="new-device-form" class="flex gap-2">
          <input type="text" name="nickname" placeholder="Device nickname" required
            class="flex-1 bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-gray-100 text-sm focus:outline-none focus:border-blue-500">
          <button type="submit" class="bg-green-600 hover:bg-green-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">Create</button>
          <button type="button" id="cancel-add-device" class="bg-gray-700 hover:bg-gray-600 text-white text-sm rounded-lg px-4 py-2 transition-colors">Cancel</button>
        </form>
      </div>
      <div id="new-device-info" class="hidden mb-4 bg-gray-800 border border-gray-700 rounded-lg p-4">
        <p class="text-sm text-gray-400 mb-2">Device created! Configure your firmware with:</p>
        <div class="space-y-2">
          <div><span class="text-xs text-gray-500">UUID:</span> <code id="new-device-uuid" class="text-sm font-mono select-all text-blue-400"></code></div>
          <div><span class="text-xs text-gray-500">URL:</span> <code id="new-device-url" class="text-sm font-mono select-all text-blue-400 break-all"></code></div>
        </div>
        <button id="dismiss-device-info" class="mt-3 text-sm text-gray-400 hover:text-white">Dismiss</button>
      </div>
      ${devices.length === 0
        ? `<p class="text-gray-500 text-sm">No devices registered. Add a collar controller to get started.</p>`
        : `<div class="space-y-2">${devices.map((d) => renderDeviceCard(d, true)).join("")}</div>`}
    </div>`;
}

function renderSharedDevices(devices: ApiDevice[]): string {
  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <h2 class="text-lg font-semibold mb-4">Shared With Me</h2>
      ${devices.length === 0
        ? `<p class="text-gray-500 text-sm">No devices shared with you yet. Ask a device owner for an invitation link.</p>`
        : `<div class="space-y-2">${devices.map((d) => renderDeviceCard(d, false)).join("")}</div>`}
    </div>`;
}

function renderDeviceCard(device: ApiDevice, isOwner: boolean): string {
  const statusDot = device.connected
    ? `<span class="w-2 h-2 rounded-full bg-green-500 inline-block"></span>`
    : `<span class="w-2 h-2 rounded-full bg-gray-600 inline-block"></span>`;

  return `
    <a href="#/device/${esc(device.uuid)}" class="block bg-gray-800 border border-gray-700 rounded-xl p-4 hover:border-gray-600 transition-colors">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          ${statusDot}
          <span class="font-medium">${esc(device.nickname)}</span>
        </div>
        <span class="text-xs text-gray-500">${isOwner ? "owned" : `by ${esc(device.ownerLogin)}`}</span>
      </div>
      <div class="text-xs text-gray-500 mt-1 font-mono">${esc(device.uuid)}</div>
    </a>`;
}

function renderInvitations(invitations: ApiInvitation[], newLink: string | null): string {
  const sent = invitations.filter((i) => i.fromLogin === state.user?.login);
  const received = invitations.filter((i) => i.toLogin === state.user?.login);

  return `
    <div class="bg-gray-900 border border-gray-800 rounded-2xl p-6 shadow-xl">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-lg font-semibold">Invitations</h2>
        <button id="show-invite-form-btn" class="bg-blue-600 hover:bg-blue-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">
          Create Invitation
        </button>
      </div>
      <div id="invite-form" class="hidden mb-4">
        <form id="new-invite-form" class="flex gap-2">
          <input type="text" name="invite-name" placeholder="Invitation name (e.g. for Alice)" required
            class="flex-1 bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-gray-100 text-sm focus:outline-none focus:border-blue-500">
          <button type="submit" class="bg-green-600 hover:bg-green-700 text-white text-sm font-semibold rounded-lg px-4 py-2 transition-colors">Send</button>
          <button type="button" id="cancel-invite-form" class="bg-gray-700 hover:bg-gray-600 text-white text-sm rounded-lg px-4 py-2 transition-colors">Cancel</button>
        </form>
      </div>

      ${newLink ? `
        <div class="mb-4 bg-gray-800 border border-blue-700 rounded-lg p-4">
          <p class="text-sm text-gray-300 mb-2">Share this single-use invitation link:</p>
          <code class="text-sm font-mono select-all text-blue-400 break-all">${esc(newLink)}</code>
          <button id="dismiss-invite-link" class="block mt-2 text-sm text-gray-400 hover:text-white">Dismiss</button>
        </div>` : ""}

      <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div>
          <h3 class="text-sm font-semibold text-gray-400 mb-2">Sent (${sent.length})</h3>
          ${sent.length === 0
            ? `<p class="text-xs text-gray-600">No invitations sent</p>`
            : sent.map((i) => `
              <div class="bg-gray-800 border border-gray-700 rounded-lg p-3 mb-2">
                <div class="flex justify-between items-center text-sm">
                  <div>
                    <span class="font-medium">${esc(i.name)}</span>
                    <span class="text-xs text-gray-500 ml-2">${i.toLogin ? esc(i.toLogin) : "pending..."}</span>
                  </div>
                  <div class="flex items-center gap-2">
                    <span class="text-xs ${i.status === "accepted" ? "text-green-400" : i.status === "rejected" ? "text-red-400" : "text-yellow-400"}">${esc(i.status)}</span>
                    ${i.status === "pending" ? `<button class="cancel-invite-btn text-xs text-red-400 hover:text-red-300" data-invite-token="${esc(i.token)}">Cancel</button>` : ""}
                  </div>
                </div>
              </div>`).join("")}
        </div>
        <div>
          <h3 class="text-sm font-semibold text-gray-400 mb-2">Received (${received.length})</h3>
          ${received.length === 0
            ? `<p class="text-xs text-gray-600">No invitations received</p>`
            : received.map((i) => `
              <div class="bg-gray-800 border border-gray-700 rounded-lg p-3 mb-2">
                <div class="flex justify-between text-sm">
                  <div>
                    <span class="font-medium">${esc(i.name)}</span>
                    <span class="text-xs text-gray-500 ml-2">from ${esc(i.fromLogin)}</span>
                  </div>
                  <span class="text-xs ${i.status === "accepted" ? "text-green-400" : i.status === "rejected" ? "text-red-400" : "text-yellow-400"}">${esc(i.status)}</span>
                </div>
              </div>`).join("")}
        </div>
      </div>
    </div>`;
}

// ── Data loading ──

export async function loadDashboardData(): Promise<void> {
  try {
    const [owned, shared, invitations] = await Promise.all([
      api.listDevices(),
      api.listSharedDevices(),
      api.listInvitations(),
    ]);
    dashData = { ownedDevices: owned, sharedDevices: shared, invitations, newInviteLink: dashData.newInviteLink };
  } catch (err) {
    showBanner("error", err instanceof Error ? err.message : "Failed to load data");
  }
}

// ── Event binding ──

export function bindDashboardEvents(root: HTMLElement): void {
  root.querySelector("#logout-btn")?.addEventListener("click", async () => {
    try {
      await api.logout();
    } catch {
      // ignore
    }
    ws.disconnect();
    state.user = null;
    state.sessionToken = null;
    state.devices.clear();
    navigate("/login");
  });

  const addBtn = root.querySelector("#add-device-btn");
  const addForm = root.querySelector("#add-device-form");
  const cancelBtn = root.querySelector("#cancel-add-device");

  addBtn?.addEventListener("click", () => {
    addForm?.classList.toggle("hidden");
  });

  cancelBtn?.addEventListener("click", () => {
    addForm?.classList.add("hidden");
  });

  const newDeviceForm = root.querySelector("#new-device-form") as HTMLFormElement | null;
  newDeviceForm?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(newDeviceForm);
    const nickname = (formData.get("nickname") as string).trim();

    try {
      const result = await api.addDevice(nickname);
      addForm?.classList.add("hidden");
      const infoDiv = root.querySelector("#new-device-info");
      const uuidEl = root.querySelector("#new-device-uuid");
      const urlEl = root.querySelector("#new-device-url");
      if (infoDiv && uuidEl && urlEl) {
        uuidEl.textContent = result.device.uuid;
        urlEl.textContent = result.connectionUrl;
        infoDiv.classList.remove("hidden");
      }
      await loadDashboardData();
      triggerRender();
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Failed to add device");
    }
  });

  root.querySelector("#dismiss-device-info")?.addEventListener("click", () => {
    root.querySelector("#new-device-info")?.classList.add("hidden");
  });

  root.querySelector("#show-invite-form-btn")?.addEventListener("click", () => {
    root.querySelector("#invite-form")?.classList.toggle("hidden");
  });

  root.querySelector("#cancel-invite-form")?.addEventListener("click", () => {
    root.querySelector("#invite-form")?.classList.add("hidden");
  });

  const newInviteForm = root.querySelector("#new-invite-form") as HTMLFormElement | null;
  newInviteForm?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(newInviteForm);
    const name = (formData.get("invite-name") as string).trim();
    try {
      const result = await api.createInvitation(name);
      root.querySelector("#invite-form")?.classList.add("hidden");
      dashData.newInviteLink = result.link;
      await loadDashboardData();
      triggerRender();
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Failed to create invitation");
    }
  });

  root.querySelector("#dismiss-invite-link")?.addEventListener("click", () => {
    dashData.newInviteLink = null;
    triggerRender();
  });

  root.querySelectorAll(".cancel-invite-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.preventDefault();
      const token = (btn as HTMLElement).dataset.inviteToken;
      if (!token) return;
      try {
        await api.cancelInvitation(token);
        await loadDashboardData();
        triggerRender();
      } catch (err) {
        showBanner("error", err instanceof Error ? err.message : "Failed to cancel invitation");
      }
    });
  });
}
