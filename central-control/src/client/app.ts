import * as api from "./api.js";
import { ApiError } from "./api.js";
import * as wsClient from "./ws.js";
import { defineRoute, onNotFound, startRouter, navigate, currentPath } from "./router.js";
import { state, onRender, triggerRender, showBanner } from "./state.js";
import {
  renderLoginPage, bindLoginEvents,
  renderSignupPage, bindSignupEvents,
  renderTotpLoginPage, bindTotpLoginEvents,
  renderTotpSetupPage, bindTotpSetupEvents, getTotpSetupData,
} from "./pages/auth.js";
import { renderDashboard, loadDashboardData, bindDashboardEvents } from "./pages/dashboard.js";
import { renderDevicePage, loadDeviceData, bindDeviceEvents } from "./pages/device.js";
import { renderInvitePage, bindInviteEvents } from "./pages/invite.js";
import { handlePresetPreview } from "./components/preset-editor.js";
import type { WsServerMessage, DeviceSnapshot } from "../shared/types.js";
import { esc } from "./utils.js";

let root: HTMLElement;

export function initApp(el: HTMLElement): void {
  root = el;

  onRender(render);

  wsClient.setHandlers(handleWsMessage, handleWsStatus);

  defineRoute("/login", () => { state.currentPage = "login"; state.pageParams = {}; render(); });
  defineRoute("/signup", () => { state.currentPage = "signup"; state.pageParams = {}; render(); });
  defineRoute("/totp-login", () => { state.currentPage = "totp-login"; state.pageParams = {}; render(); });
  defineRoute("/setup-2fa", () => {
    if (!state.user) { navigate("/login"); return; }
    state.currentPage = "setup-2fa"; state.pageParams = {}; render();
  });
  defineRoute("/", async () => {
    if (!state.user) { navigate("/login"); return; }
    state.currentPage = "dashboard"; state.pageParams = {};
    await loadDashboardData();
    render();
  });
  defineRoute("/device/:uuid", async (params) => {
    if (!state.user) { navigate("/login"); return; }
    state.currentPage = "device"; state.pageParams = params;
    await loadDeviceData(params.uuid!);
    render();
  });
  defineRoute("/invite/:token", async (params) => {
    state.currentPage = "invite"; state.pageParams = params;
    render();
  });

  onNotFound(() => {
    navigate("/");
  });

  // Try to restore session
  checkSession();
}

async function checkSession(): Promise<void> {
  try {
    const user = await api.getMe();
    state.user = user;
    // We don't know the session token from cookie, but the cookie is sent automatically
    state.sessionToken = "cookie";
    wsClient.connect();
    startRouter();
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      state.user = null;
      state.sessionToken = null;
      startRouter();
      // Let the router handle redirecting to login
      const path = currentPath();
      if (!path.startsWith("/login") && !path.startsWith("/signup") && !path.startsWith("/invite/")) {
        navigate("/login");
      }
    } else {
      showBanner("error", "Failed to connect to server");
      startRouter();
    }
  }
}

function render(): void {
  const bannerHtml = state.banner
    ? `<div class="fixed top-4 right-4 z-40 max-w-md px-4 py-3 rounded-xl text-sm shadow-lg ${
        state.banner.kind === "error"
          ? "bg-red-900/90 border border-red-700 text-red-200"
          : "bg-blue-900/90 border border-blue-700 text-blue-200"
      }">${esc(state.banner.text)}</div>`
    : "";

  let pageHtml = "";
  switch (state.currentPage) {
    case "login":
      pageHtml = renderLoginPage();
      break;
    case "signup":
      pageHtml = renderSignupPage();
      break;
    case "totp-login":
      pageHtml = renderTotpLoginPage();
      break;
    case "setup-2fa":
      pageHtml = renderTotpSetupPage(getTotpSetupData());
      break;
    case "dashboard":
      pageHtml = renderDashboard();
      break;
    case "device":
      pageHtml = renderDevicePage();
      break;
    case "invite":
      pageHtml = renderInvitePage(state.pageParams.token ?? "");
      break;
    case "loading":
      pageHtml = `<div class="min-h-screen flex items-center justify-center"><p class="text-gray-400">Loading...</p></div>`;
      break;
  }

  root.innerHTML = bannerHtml + pageHtml;
  bindPageEvents();
}

function bindPageEvents(): void {
  switch (state.currentPage) {
    case "login":
      bindLoginEvents(root);
      break;
    case "signup":
      bindSignupEvents(root);
      break;
    case "totp-login":
      bindTotpLoginEvents(root);
      break;
    case "setup-2fa":
      bindTotpSetupEvents(root);
      break;
    case "dashboard":
      bindDashboardEvents(root);
      break;
    case "device":
      bindDeviceEvents(root);
      break;
    case "invite":
      bindInviteEvents(root, state.pageParams.token ?? "");
      break;
  }
}

function handleWsMessage(msg: WsServerMessage): void {
  switch (msg.type) {
    case "authenticated":
      break;
    case "devices_list":
      state.devices.clear();
      for (const device of msg.devices) {
        state.devices.set(device.uuid, device);
      }
      // Only re-render if on device page to update live state
      if (state.currentPage === "device") {
        render();
      }
      break;
    case "device_update": {
      state.devices.set(msg.device.uuid, msg.device);
      if (state.currentPage === "device" && state.pageParams.uuid === msg.device.uuid) {
        render();
      }
      break;
    }
    case "device_disconnected":
      state.devices.delete(msg.deviceUuid);
      if (state.currentPage === "device" && state.pageParams.uuid === msg.deviceUuid) {
        render();
      }
      break;
    case "error":
      showBanner("error", msg.message);
      break;
    case "info":
      showBanner("info", msg.message);
      break;
    case "preset_preview":
      handlePresetPreview(msg.nonce, msg.preview, msg.error);
      break;
  }
}

function handleWsStatus(connected: boolean): void {
  state.wsConnected = connected;
  if (!connected && state.user) {
    showBanner("error", "WebSocket disconnected, reconnecting...");
  }
}
