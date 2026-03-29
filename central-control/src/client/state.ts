import type { ApiUser, DeviceSnapshot } from "../shared/types.js";
import type { PresetPreview } from "../shared/protocol.js";

export interface AppState {
  user: ApiUser | null;
  sessionToken: string | null;
  wsConnected: boolean;
  devices: Map<string, DeviceSnapshot>;
  banner: { kind: "info" | "error"; text: string } | null;
  currentPage: string;
  pageParams: Record<string, string>;
}

export const state: AppState = {
  user: null,
  sessionToken: null,
  wsConnected: false,
  devices: new Map(),
  banner: null,
  currentPage: "loading",
  pageParams: {},
};

let renderCallback: (() => void) | null = null;

export function onRender(cb: () => void): void {
  renderCallback = cb;
}

export function triggerRender(): void {
  renderCallback?.();
}

export function showBanner(kind: "info" | "error", text: string): void {
  state.banner = { kind, text };
  triggerRender();
  if (kind === "info") {
    setTimeout(() => {
      if (state.banner?.text === text) {
        state.banner = null;
        triggerRender();
      }
    }, 4000);
  }
}
