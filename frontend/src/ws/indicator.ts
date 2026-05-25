import { WsManager } from "./manager.js";
import {
  type ManagerStats,
  type WidgetState,
  WS_CONNECT_TIMEOUT_MS,
  WS_PONG_TIMEOUT_MS,
  WS_STALE_GRACE_MS,
} from "./types.js";

const RENDER_INTERVAL_MS = 100;
const LOG_MAX_RENDERED = 80;

export interface IndicatorElements {
  readonly dot: HTMLElement;
  readonly text: HTMLElement;
  readonly tooltip: HTMLElement | null;
  readonly retryButton: HTMLButtonElement | null;
  readonly titleBase: string;
}

export function deriveWidgetState(stats: ManagerStats): WidgetState {
  if (stats.isTerminal) return "terminal";
  let hasAlive = false;
  let hasStale = false;
  let hasConnecting = false;
  for (const c of stats.connections) {
    if (c.state === "ALIVE") hasAlive = true;
    else if (c.state === "STALE") hasStale = true;
    else if (c.state === "NEW") hasConnecting = true;
  }
  if (hasAlive && !hasStale) return "alive";
  if (hasStale) return "stale";
  if (hasConnecting) return "connecting";
  if (stats.reconnectDeferredOnHidden) return "deferred";
  if (stats.reconnectAt !== null) return "connecting";
  return "dead";
}

const WIDGET_LABEL: Record<WidgetState, string> = {
  alive: "Connected",
  stale: "Connection stale",
  connecting: "Connecting",
  deferred: "Deferred (tab hidden)",
  terminal: "Disconnected",
  dead: "Disconnected",
};

const WIDGET_GLYPH: Record<WidgetState, string> = {
  alive: "●", // ●
  stale: "◐", // ◐
  connecting: "○", // ○
  deferred: "⏸", // ⏸
  terminal: "✕", // ✕
  dead: "✕", // ✕
};

interface CountdownView {
  readonly remainingMs: number;
  readonly totalMs: number;
  readonly label: string;
}

function computeCountdown(stats: ManagerStats, widget: WidgetState): CountdownView | null {
  const now = stats.now;
  if (widget === "connecting" && stats.reconnectAt !== null) {
    const remaining = Math.max(0, stats.reconnectAt - now);
    return {
      remainingMs: remaining,
      totalMs: WS_CONNECT_TIMEOUT_MS,
      label: `retry in ${(remaining / 1000).toFixed(1)}s`,
    };
  }
  const active = stats.connections.find((c) => c.id === stats.activeId) ?? null;
  if (widget === "stale" && active && active.deadline !== null) {
    const remaining = Math.max(0, active.deadline - now);
    return {
      remainingMs: remaining,
      totalMs: WS_STALE_GRACE_MS,
      label: `stale, ${(remaining / 1000).toFixed(1)}s to recover`,
    };
  }
  if (widget === "connecting") {
    const newest = stats.connections.find((c) => c.state === "NEW");
    if (newest && newest.deadline !== null) {
      const remaining = Math.max(0, newest.deadline - now);
      return {
        remainingMs: remaining,
        totalMs: WS_CONNECT_TIMEOUT_MS,
        label: `connecting, ${(remaining / 1000).toFixed(1)}s`,
      };
    }
  }
  if (widget === "alive" && active && active.deadline !== null) {
    const remaining = Math.max(0, active.deadline - now);
    return {
      remainingMs: remaining,
      totalMs: WS_PONG_TIMEOUT_MS,
      label: `pong ${(remaining / 1000).toFixed(1)}s`,
    };
  }
  return null;
}

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function formatRttWindow(label: string, w: ManagerStats["rtt30s"]): string {
  if (w.count === 0) return `<div>${label}: <span class="ws-dim">no data</span></div>`;
  return `<div>${label}: min ${w.min}ms / med ${w.median}ms / max ${w.max}ms (n=${w.count})</div>`;
}

function renderTooltip(stats: ManagerStats, widget: WidgetState): string {
  const active = stats.connections.find((c) => c.id === stats.activeId) ?? null;
  const lossPct =
    stats.totalPings > 0
      ? ((stats.lostPings / stats.totalPings) * 100).toFixed(1)
      : "0.0";
  const lines: string[] = [];
  lines.push(`<div><b>${esc(WIDGET_LABEL[widget])}</b></div>`);
  if (stats.terminalReason) {
    lines.push(`<div class="ws-err">${esc(stats.terminalReason)}</div>`);
  }
  lines.push(
    `<div>pool: ${stats.connections.length} (alive ${stats.connections.filter((c) => c.state === "ALIVE").length})</div>`,
  );
  if (active) {
    const uptime = active.aliveSince !== null ? Math.round((stats.now - active.aliveSince) / 1000) : 0;
    lines.push(
      `<div>active: #${active.id} (${esc(active.state)}, up ${uptime}s, pending pings ${active.pendingPings}, last rtt ${active.lastRttMs ?? "—"}ms)</div>`,
    );
  } else {
    lines.push(`<div>active: <span class="ws-dim">none</span></div>`);
  }
  lines.push(`<div>loss: ${lossPct}% (${stats.lostPings}/${stats.totalPings})</div>`);
  lines.push(formatRttWindow("rtt 30s", stats.rtt30s));
  lines.push(formatRttWindow("rtt 1m", stats.rtt1m));
  lines.push(formatRttWindow("rtt 5m", stats.rtt5m));
  if (stats.reconnectDeferredOnHidden) {
    lines.push(`<div>reconnect: deferred until tab is visible</div>`);
  } else if (stats.reconnectAt !== null) {
    const remaining = Math.max(0, stats.reconnectAt - stats.now);
    lines.push(
      `<div>reconnect: attempt ${stats.reconnectAttempt}/${stats.reconnectMaxAttempts} in ${(remaining / 1000).toFixed(1)}s</div>`,
    );
  } else if (stats.isTerminal) {
    lines.push(`<div>reconnect: stopped (click retry)</div>`);
  } else if (stats.reconnectAttempt > 0) {
    lines.push(`<div>reconnect: idle (attempts so far ${stats.reconnectAttempt})</div>`);
  }
  // Per-connection cards
  const conns = stats.connections
    .map((c) => {
      const cls = c.id === stats.activeId ? "ws-card ws-card-active" : "ws-card";
      const closeBits = c.closeCode !== null ? ` close ${c.closeCode}${c.closeReason ? ` (${esc(c.closeReason)})` : ""}` : "";
      return `<div class="${cls}">#${c.id} ${esc(c.state)} rtt ${c.lastRttMs ?? "—"}ms pend ${c.pendingPings}${closeBits}</div>`;
    })
    .join("");
  if (conns) lines.push(`<div class="ws-cards">${conns}</div>`);
  return lines.join("");
}

export class Indicator {
  private rafHandle: number | null = null;
  private pendingRender = false;
  private lastRenderAt = 0;
  private unsubscribe: (() => void) | null = null;
  private lastStats: ManagerStats | null = null;

  constructor(
    private readonly mgr: WsManager,
    private readonly el: IndicatorElements,
  ) {
    this.el.dot.setAttribute("role", "status");
    this.el.dot.setAttribute("aria-live", "polite");
    if (this.el.retryButton) {
      this.el.retryButton.addEventListener("click", () => this.mgr.retry());
    }
    this.unsubscribe = this.mgr.subscribe((stats) => {
      this.lastStats = stats;
      this.scheduleRender();
    });
    this.startRafLoop();
  }

  destroy(): void {
    if (this.unsubscribe) this.unsubscribe();
    if (this.rafHandle !== null) cancelAnimationFrame(this.rafHandle);
  }

  private scheduleRender(): void {
    this.pendingRender = true;
  }

  private startRafLoop(): void {
    const tick = () => {
      if (this.pendingRender || Date.now() - this.lastRenderAt >= RENDER_INTERVAL_MS) {
        this.render();
      }
      this.rafHandle = requestAnimationFrame(tick);
    };
    this.rafHandle = requestAnimationFrame(tick);
  }

  private render(): void {
    this.pendingRender = false;
    this.lastRenderAt = Date.now();
    const stats = this.lastStats ?? this.mgr.snapshot();
    const widget = deriveWidgetState(stats);
    const cd = computeCountdown(stats, widget);

    this.el.dot.dataset.state = widget;
    this.el.dot.setAttribute("aria-label", WIDGET_LABEL[widget]);
    this.el.dot.title = WIDGET_LABEL[widget];

    const glyph = WIDGET_GLYPH[widget];
    const active = stats.connections.find((c) => c.id === stats.activeId) ?? null;
    let text: string;
    if (widget === "alive") {
      const rtt = active?.lastRttMs ?? null;
      text = rtt === null ? `${glyph} ...` : `${glyph} ${rtt}ms`;
    } else if (widget === "stale") {
      text = `${glyph} ${cd?.label ?? "stale"}`;
    } else if (widget === "connecting") {
      text = `${glyph} ${cd?.label ?? "connecting"}`;
    } else if (widget === "deferred") {
      text = `${glyph} deferred (hidden)`;
    } else if (widget === "terminal") {
      text = `${glyph} stopped`;
    } else {
      text = `${glyph} disconnected`;
    }
    this.el.text.textContent = text;

    if (this.el.tooltip) {
      this.el.tooltip.innerHTML = renderTooltip(stats, widget);
      const logHtml = this.renderLog();
      if (logHtml) this.el.tooltip.innerHTML += `<div class="ws-log">${logHtml}</div>`;
    }

    if (this.el.retryButton) {
      this.el.retryButton.style.display = stats.isTerminal ? "" : "none";
    }

    // R-V7: mirror state in document.title so hidden tabs surface it.
    const aliveCount = stats.connections.filter((c) => c.state === "ALIVE").length;
    const total = stats.connections.length;
    const prefix = widget === "alive" ? `(${aliveCount}/${total})` : `[${WIDGET_LABEL[widget].toUpperCase()}]`;
    document.title = `${prefix} ${this.el.titleBase}`;
  }

  private renderLog(): string {
    const entries = this.mgr.getLog();
    if (entries.length === 0) return "";
    const slice = entries.slice(-LOG_MAX_RENDERED).reverse();
    return slice
      .map((e) => {
        const t = new Date(e.at).toLocaleTimeString();
        const cls = e.level === "error" ? "ws-log-err" : e.level === "warn" ? "ws-log-warn" : "";
        return `<div class="${cls}">${t} ${esc(e.text)}</div>`;
      })
      .join("");
  }
}
