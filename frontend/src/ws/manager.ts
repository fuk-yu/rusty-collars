import { Connection, type ConnectionCallbacks } from "./connection.js";
import {
  type LogEntry,
  type ManagerStats,
  type RttSample,
  type RttWindow,
  type ServerMessageHandler,
  type StatsListener,
  NON_RETRIABLE_CLOSE_CODES,
  WS_BACKOFF_BASE_MS,
  WS_BACKOFF_CAP_MS,
  WS_EVENT_LOG_CAP,
  WS_MAX_LIVE_CONNECTIONS,
  WS_MAX_RECONNECT_ATTEMPTS,
  WS_PONG_TIMEOUT_MS,
  WS_RTT_HISTORY_CAP,
  WS_RTT_WINDOW_1M_MS,
  WS_RTT_WINDOW_30S_MS,
  WS_RTT_WINDOW_5M_MS,
  WS_TICK_MS,
  WS_TIME_JUMP_THRESHOLD_MS,
} from "./types.js";

export interface ManagerOptions {
  readonly url: string;
  readonly onMessage: ServerMessageHandler;
  readonly onActiveOpen?: () => void;
  readonly onActiveLost?: () => void;
}

export class WsManager {
  private readonly connections = new Map<number, Connection>();
  private activeId: number | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectAt: number | null = null;
  private reconnectDeferredOnHidden = false;
  private isTerminal = false;
  private terminalReason: string | null = null;
  private totalPings = 0;
  private lostPings = 0;
  private rttHistory: RttSample[] = [];
  private logEntries: LogEntry[] = [];
  private listeners = new Set<StatsListener>();
  private tickTimer: ReturnType<typeof setInterval> | null = null;
  private lastTickAt: number;
  private destroyed = false;

  constructor(private readonly opts: ManagerOptions) {
    this.lastTickAt = Date.now();
    this.installLifecycleListeners();
    this.startTimeJumpDetector();
    this.connect();
  }

  // ── public surface ───────────────────────────────────────────────────

  send(payload: any): boolean {
    const active = this.getActiveConnection();
    if (!active) return false;
    return active.send(JSON.stringify(payload));
  }

  isActiveAlive(): boolean {
    const c = this.getActiveConnection();
    return c !== null && c.getState() === "ALIVE";
  }

  isActiveOpen(): boolean {
    return this.getActiveConnection() !== null;
  }

  /** Manual user trigger: clear terminal state and try connecting again. */
  retry(): void {
    if (this.destroyed) return;
    this.log("info", "manual retry requested");
    this.isTerminal = false;
    this.terminalReason = null;
    this.reconnectAttempt = 0;
    this.clearReconnectTimer();
    this.connect();
    this.notify();
  }

  subscribe(listener: StatsListener): () => void {
    this.listeners.add(listener);
    listener(this.snapshot());
    return () => this.listeners.delete(listener);
  }

  snapshot(): ManagerStats {
    const now = Date.now();
    const stats: ManagerStats = {
      now,
      connections: Array.from(this.connections.values()).map((c) => c.getStats(now)),
      activeId: this.activeId,
      reconnectAttempt: this.reconnectAttempt,
      reconnectMaxAttempts: WS_MAX_RECONNECT_ATTEMPTS,
      reconnectAt: this.reconnectAt,
      reconnectDeferredOnHidden: this.reconnectDeferredOnHidden,
      isTerminal: this.isTerminal,
      terminalReason: this.terminalReason,
      lostPings: this.lostPings,
      totalPings: this.totalPings,
      rtt30s: this.rttWindow(now, WS_RTT_WINDOW_30S_MS),
      rtt1m: this.rttWindow(now, WS_RTT_WINDOW_1M_MS),
      rtt5m: this.rttWindow(now, WS_RTT_WINDOW_5M_MS),
    };
    return stats;
  }

  log(level: LogEntry["level"], text: string): void {
    const entry: LogEntry = { at: Date.now(), level, text };
    this.logEntries.push(entry);
    if (this.logEntries.length > WS_EVENT_LOG_CAP) {
      this.logEntries.splice(0, this.logEntries.length - WS_EVENT_LOG_CAP);
    }
    if (level !== "info") console.warn(`[ws] ${text}`);
  }

  getLog(): readonly LogEntry[] {
    return this.logEntries;
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.clearReconnectTimer();
    if (this.tickTimer !== null) {
      clearInterval(this.tickTimer);
      this.tickTimer = null;
    }
    for (const c of this.connections.values()) c.destroy();
    this.connections.clear();
    this.activeId = null;
  }

  // ── connection lifecycle ─────────────────────────────────────────────

  private getActiveConnection(): Connection | null {
    if (this.activeId === null) return null;
    return this.connections.get(this.activeId) ?? null;
  }

  private connect(): void {
    if (this.destroyed || this.isTerminal) return;
    if (this.countLiveConnections() >= WS_MAX_LIVE_CONNECTIONS) {
      this.log("warn", "max live connections reached, skipping spawn");
      return;
    }
    const cb: ConnectionCallbacks = {
      onStateChange: (conn, prev) => this.handleStateChange(conn, prev),
      onMessage: (_conn, msg) => this.opts.onMessage(msg),
      onRtt: (_conn, rtt) => this.recordRtt(rtt),
      onPingSent: () => {
        this.totalPings++;
      },
      onPingLost: () => {
        this.lostPings++;
      },
      log: (level, text) => this.log(level, text),
      now: () => Date.now(),
    };
    const conn = new Connection(this.opts.url, cb);
    this.connections.set(conn.id, conn);
    this.notify();
  }

  private handleStateChange(conn: Connection, prev: import("./types.js").ConnectionState): void {
    const next = conn.getState();
    switch (next) {
      case "ALIVE": {
        this.reconnectAttempt = 0;
        this.clearReconnectTimer();
        if (this.activeId === null) {
          this.activeId = conn.id;
          this.opts.onActiveOpen?.();
        } else if (this.activeId !== conn.id) {
          const active = this.connections.get(this.activeId);
          if (!active || active.getState() !== "ALIVE") {
            this.activeId = conn.id;
            this.opts.onActiveOpen?.();
          } else {
            conn.close("superseded by existing active");
          }
        }
        // Cull any redundant non-active peers — we only need one live channel.
        if (this.activeId === conn.id) {
          for (const other of this.connections.values()) {
            if (other.id === conn.id) continue;
            const s = other.getState();
            if (s === "NEW" || s === "ALIVE" || s === "STALE") {
              other.close("active recovered, no longer needed");
            }
          }
        }
        break;
      }
      case "STALE": {
        if (this.activeId === conn.id) {
          this.ensureReplacement();
        }
        break;
      }
      case "DEAD": {
        if (this.activeId === conn.id) {
          this.activeId = null;
          this.opts.onActiveLost?.();
          this.handleActiveLost(conn);
        }
        // Drop from pool once it linger-cleans itself; we erase here too so
        // counts stay tight.
        this.connections.delete(conn.id);
        if (this.activeId === null && !this.hasUsableConnection()) {
          this.scheduleReconnect(conn.getStats(Date.now()).closeCode);
        }
        break;
      }
      case "NEW":
        break;
    }
    void prev;
    this.notify();
  }

  private handleActiveLost(deadConn: Connection): void {
    const stats = deadConn.getStats(Date.now());
    const code = stats.closeCode ?? 1006;
    if (NON_RETRIABLE_CLOSE_CODES.has(code)) {
      this.isTerminal = true;
      this.terminalReason = `non-retriable close ${code}: ${stats.closeReason ?? "no reason"}`;
      this.log("error", `terminal: ${this.terminalReason}`);
    }
  }

  private ensureReplacement(): void {
    // Already a NEW/ALIVE peer in the pool? Don't spawn another.
    for (const c of this.connections.values()) {
      const s = c.getState();
      if (s === "NEW" || s === "ALIVE") {
        if (c.id !== this.activeId) return;
      }
    }
    this.log("info", "spawning replacement for STALE active");
    this.connect();
  }

  private hasUsableConnection(): boolean {
    for (const c of this.connections.values()) {
      const s = c.getState();
      if (s === "NEW" || s === "ALIVE" || s === "STALE") return true;
    }
    return false;
  }

  private countLiveConnections(): number {
    let n = 0;
    for (const c of this.connections.values()) {
      if (c.getState() !== "DEAD") n++;
    }
    return n;
  }

  // ── reconnect / backoff ──────────────────────────────────────────────

  private scheduleReconnect(lastCloseCode: number | null): void {
    if (this.destroyed || this.isTerminal) return;
    if (this.reconnectTimer !== null) return;
    if (lastCloseCode !== null && NON_RETRIABLE_CLOSE_CODES.has(lastCloseCode)) {
      this.isTerminal = true;
      this.terminalReason = `non-retriable close ${lastCloseCode}`;
      this.notify();
      return;
    }
    if (this.reconnectAttempt >= WS_MAX_RECONNECT_ATTEMPTS) {
      this.isTerminal = true;
      this.terminalReason = `reconnect attempts exhausted (${WS_MAX_RECONNECT_ATTEMPTS})`;
      this.log("error", `terminal: ${this.terminalReason}`);
      this.notify();
      return;
    }
    if (typeof document !== "undefined" && document.visibilityState === "hidden") {
      this.reconnectDeferredOnHidden = true;
      this.reconnectAt = null;
      this.log("info", "reconnect deferred (tab hidden)");
      this.notify();
      return;
    }
    this.reconnectDeferredOnHidden = false;
    const delay = this.computeBackoffDelay(this.reconnectAttempt);
    this.reconnectAttempt++;
    const fireAt = Date.now() + delay;
    this.reconnectAt = fireAt;
    this.log("info", `reconnect in ${delay}ms (attempt ${this.reconnectAttempt}/${WS_MAX_RECONNECT_ATTEMPTS})`);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.reconnectAt = null;
      this.connect();
    }, delay);
    this.notify();
  }

  private computeBackoffDelay(attempt: number): number {
    const exp = Math.min(WS_BACKOFF_CAP_MS, WS_BACKOFF_BASE_MS * Math.pow(2, attempt));
    // Full jitter — random(0.5, 1.0) × computed delay.
    const min = exp * 0.5;
    return Math.floor(min + Math.random() * (exp - min));
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.reconnectAt = null;
    this.reconnectDeferredOnHidden = false;
  }

  // ── RTT / loss accounting ────────────────────────────────────────────

  private recordRtt(rttMs: number): void {
    const now = Date.now();
    this.rttHistory.push({ at: now, rttMs });
    if (this.rttHistory.length > WS_RTT_HISTORY_CAP) {
      this.rttHistory.splice(0, this.rttHistory.length - WS_RTT_HISTORY_CAP);
    }
  }

  private rttWindow(now: number, windowMs: number): RttWindow {
    const cutoff = now - windowMs;
    const samples: number[] = [];
    for (const s of this.rttHistory) {
      if (s.at >= cutoff) samples.push(s.rttMs);
    }
    if (samples.length === 0) return { count: 0, min: 0, median: 0, max: 0 };
    samples.sort((a, b) => a - b);
    const mid = samples.length >> 1;
    const median =
      samples.length % 2 === 0
        ? Math.round(((samples[mid - 1] ?? 0) + (samples[mid] ?? 0)) / 2)
        : (samples[mid] ?? 0);
    const min = samples[0] ?? 0;
    const max = samples[samples.length - 1] ?? 0;
    return { count: samples.length, min, median, max };
  }

  // ── time-jump detection ─────────────────────────────────────────────

  private startTimeJumpDetector(): void {
    this.tickTimer = setInterval(() => {
      const now = Date.now();
      const elapsed = now - this.lastTickAt;
      this.lastTickAt = now;
      if (elapsed > WS_TICK_MS + WS_TIME_JUMP_THRESHOLD_MS) {
        this.handleTimeJump(elapsed);
      }
    }, WS_TICK_MS);
  }

  private handleTimeJump(elapsedMs: number): void {
    this.log("warn", `time jump detected: ${elapsedMs}ms`);
    if (elapsedMs >= WS_PONG_TIMEOUT_MS) {
      // Long gap — NAT, TCP state, freezes. Open a replacement proactively
      // instead of waiting for the existing socket to time out a pong.
      this.log("info", "proactive reconnect after long gap");
      const active = this.getActiveConnection();
      if (active && active.getState() === "ALIVE") {
        // Demote it: try a fresh connection and let the manager promote whichever
        // confirms aliveness first.
        this.ensureReplacement();
      } else {
        this.connect();
      }
    } else {
      // Short gap — just nudge existing connections.
      for (const c of this.connections.values()) {
        if (c.isUsable()) c.pingNow();
      }
    }
  }

  // ── page lifecycle ──────────────────────────────────────────────────

  private installLifecycleListeners(): void {
    if (typeof document === "undefined" || typeof window === "undefined") return;

    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") {
        this.log("info", "tab visible");
        this.checkAllConnections();
        if (this.reconnectDeferredOnHidden) {
          this.reconnectDeferredOnHidden = false;
          this.scheduleReconnect(null);
        }
      } else {
        this.log("info", "tab hidden");
      }
    });

    window.addEventListener("freeze" as any, () => this.log("info", "page freeze"));
    window.addEventListener("resume" as any, () => {
      this.log("info", "page resume");
      this.handleTimeJump(WS_PONG_TIMEOUT_MS);
    });

    window.addEventListener("pagehide", (ev) => {
      if ((ev as PageTransitionEvent).persisted) {
        this.log("info", "pagehide persisted (BFCache) — closing sockets");
        for (const c of this.connections.values()) c.close("BFCache pagehide");
      }
    });
    window.addEventListener("pageshow", (ev) => {
      if ((ev as PageTransitionEvent).persisted) {
        this.log("info", "pageshow persisted (BFCache) — reconnecting");
        this.reconnectAttempt = 0;
        this.isTerminal = false;
        this.terminalReason = null;
        this.connect();
      }
    });

    window.addEventListener("online", () => {
      this.log("info", "online");
      this.checkAllConnections();
      if (this.isTerminal) this.retry();
      else if (!this.hasUsableConnection()) this.scheduleReconnect(null);
    });
    window.addEventListener("offline", () => this.log("warn", "offline"));

    const nav = (window.navigator as any).connection;
    if (nav && typeof nav.addEventListener === "function") {
      nav.addEventListener("change", () => {
        this.log("info", `NetworkInformation change (downlink=${nav.downlink} effectiveType=${nav.effectiveType})`);
        this.checkAllConnections();
      });
    }
  }

  private checkAllConnections(): void {
    for (const c of this.connections.values()) {
      if (c.isUsable()) c.pingNow();
    }
  }

  // ── notify ──────────────────────────────────────────────────────────

  private notify(): void {
    if (this.destroyed) return;
    const snap = this.snapshot();
    for (const listener of this.listeners) {
      try {
        listener(snap);
      } catch (err) {
        console.warn("[ws] listener threw", err);
      }
    }
  }
}
