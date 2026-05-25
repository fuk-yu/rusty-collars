import {
  type ConnectionState,
  type ConnectionStats,
  type PingSample,
  WS_CONNECT_TIMEOUT_MS,
  WS_DEAD_LINGER_MS,
  WS_PING_INTERVAL_MS,
  WS_PONG_TIMEOUT_MS,
  WS_STALE_GRACE_MS,
} from "./types.js";

export interface ConnectionCallbacks {
  onStateChange: (conn: Connection, prev: ConnectionState) => void;
  onMessage: (conn: Connection, raw: any) => void;
  onRtt: (conn: Connection, rttMs: number) => void;
  onPingSent: (conn: Connection) => void;
  onPingLost: (conn: Connection) => void;
  log: (level: "info" | "warn" | "error", text: string) => void;
  now: () => number;
}

let nextConnId = 1;

export class Connection {
  readonly id = nextConnId++;
  private socket: WebSocket | null = null;
  private state: ConnectionState = "NEW";
  private openedAt: number | null = null;
  private aliveSince: number | null = null;
  private staleSince: number | null = null;
  private deadline: number | null = null;
  private lastPingAt: number | null = null;
  private lastPongAt: number | null = null;
  private lastRttMs: number | null = null;
  private closeCode: number | null = null;
  private closeReason: string | null = null;
  private nonceCounter = 0;
  private pending = new Map<number, PingSample>();
  private connectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private graceTimer: ReturnType<typeof setTimeout> | null = null;
  private lingerTimer: ReturnType<typeof setTimeout> | null = null;
  private destroyed = false;

  constructor(
    readonly url: string,
    private readonly cb: ConnectionCallbacks,
  ) {
    const now = cb.now();
    this.deadline = now + WS_CONNECT_TIMEOUT_MS;
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch (err) {
      cb.log("error", `[#${this.id}] new WebSocket threw: ${String(err)}`);
      this.markDead(1006, "construction failed");
      return;
    }
    this.socket = socket;
    socket.onopen = this.handleOpen;
    socket.onmessage = this.handleMessage;
    socket.onerror = this.handleError;
    socket.onclose = this.handleClose;
    this.connectTimer = setTimeout(this.handleConnectTimeout, WS_CONNECT_TIMEOUT_MS);
    cb.log("info", `[#${this.id}] connecting to ${url}`);
  }

  getState(): ConnectionState {
    return this.state;
  }

  getStats(now: number): ConnectionStats {
    return {
      id: this.id,
      state: this.state,
      openedAt: this.openedAt,
      aliveSince: this.aliveSince,
      staleSince: this.staleSince,
      deadline: this.deadline,
      lastPingAt: this.lastPingAt,
      lastPongAt: this.lastPongAt,
      pendingPings: this.pending.size,
      lastRttMs: this.lastRttMs,
      closeCode: this.closeCode,
      closeReason: this.closeReason,
    };
    void now;
  }

  isUsable(): boolean {
    return (
      !this.destroyed &&
      this.socket !== null &&
      this.socket.readyState === WebSocket.OPEN &&
      (this.state === "ALIVE" || this.state === "STALE")
    );
  }

  send(json: string): boolean {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return false;
    try {
      this.socket.send(json);
      return true;
    } catch (err) {
      this.cb.log("warn", `[#${this.id}] send failed: ${String(err)}`);
      return false;
    }
  }

  pingNow(): void {
    this.sendPing();
  }

  close(reason: string): void {
    if (this.destroyed) return;
    this.cb.log("info", `[#${this.id}] closing: ${reason}`);
    this.closeReason = this.closeReason ?? reason;
    if (this.socket) {
      try {
        this.socket.close(1000, reason.slice(0, 120));
      } catch {
        // ignore
      }
    }
    if (this.state !== "DEAD") {
      this.markDead(this.closeCode ?? 1000, reason);
    }
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.clearAllTimers();
    if (this.socket) {
      try {
        this.socket.onopen = null;
        this.socket.onmessage = null;
        this.socket.onerror = null;
        this.socket.onclose = null;
        if (
          this.socket.readyState === WebSocket.OPEN ||
          this.socket.readyState === WebSocket.CONNECTING
        ) {
          this.socket.close();
        }
      } catch {
        // ignore
      }
      this.socket = null;
    }
  }

  // ── lifecycle handlers ────────────────────────────────────────────────

  private handleOpen = (): void => {
    if (this.destroyed) return;
    if (this.connectTimer !== null) {
      clearTimeout(this.connectTimer);
      this.connectTimer = null;
    }
    const now = this.cb.now();
    this.openedAt = now;
    this.aliveSince = now;
    this.deadline = now + WS_PONG_TIMEOUT_MS;
    this.transition("ALIVE");
    this.startPingLoop();
    this.sendPing();
  };

  private handleMessage = (e: MessageEvent): void => {
    if (this.destroyed) return;
    let msg: any;
    try {
      msg = JSON.parse(e.data);
    } catch (err) {
      this.cb.log("warn", `[#${this.id}] malformed JSON: ${String(err)}`);
      return;
    }
    if (msg && msg.type === "pong" && typeof msg.nonce === "number") {
      this.handlePong(msg.nonce);
      this.cb.onMessage(this, msg);
      return;
    }
    if (this.state === "STALE") {
      const now = this.cb.now();
      this.cb.log("info", `[#${this.id}] recovered from STALE on data message`);
      this.staleSince = null;
      this.deadline = now + WS_PONG_TIMEOUT_MS;
      if (this.graceTimer !== null) {
        clearTimeout(this.graceTimer);
        this.graceTimer = null;
      }
      this.transition("ALIVE");
    }
    this.cb.onMessage(this, msg);
  };

  private handleError = (): void => {
    if (this.destroyed) return;
    this.cb.log("warn", `[#${this.id}] socket error`);
  };

  private handleClose = (ev: CloseEvent): void => {
    if (this.destroyed) return;
    this.markDead(ev.code, ev.reason || `close ${ev.code}`);
  };

  private handleConnectTimeout = (): void => {
    if (this.destroyed) return;
    this.connectTimer = null;
    if (
      this.state === "NEW" &&
      this.socket &&
      this.socket.readyState === WebSocket.CONNECTING
    ) {
      this.cb.log("warn", `[#${this.id}] connect timeout after ${WS_CONNECT_TIMEOUT_MS}ms`);
      this.closeReason = "connect timeout";
      try {
        this.socket.close();
      } catch {
        // ignore
      }
      this.markDead(1006, "connect timeout");
    }
  };

  // ── heartbeat ────────────────────────────────────────────────────────

  private startPingLoop(): void {
    if (this.pingTimer !== null) clearInterval(this.pingTimer);
    this.pingTimer = setInterval(() => this.sendPing(), WS_PING_INTERVAL_MS);
  }

  private sendPing(): void {
    if (this.destroyed) return;
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    const now = this.cb.now();
    // Reap any pending pings older than the pong timeout — promotes us to STALE.
    let staled = false;
    for (const [nonce, sample] of this.pending) {
      if (now - sample.sentAt >= WS_PONG_TIMEOUT_MS) {
        this.pending.delete(nonce);
        this.cb.onPingLost(this);
        if (this.state === "ALIVE" && !staled) {
          this.cb.log("warn", `[#${this.id}] pong timeout for nonce=${nonce}`);
          this.enterStale();
          staled = true;
        }
      }
    }
    if (staled) return;
    const nonce = ++this.nonceCounter;
    const sample: PingSample = { nonce, sentAt: now };
    this.pending.set(nonce, sample);
    this.lastPingAt = now;
    const ok = this.send(JSON.stringify({ type: "ping", nonce }));
    if (!ok) {
      this.pending.delete(nonce);
      return;
    }
    this.cb.onPingSent(this);
  }

  private handlePong(nonce: number): void {
    const sample = this.pending.get(nonce);
    if (!sample) return;
    this.pending.delete(nonce);
    const now = this.cb.now();
    const rtt = Math.max(1, Math.round(now - sample.sentAt));
    this.lastPongAt = now;
    this.lastRttMs = rtt;
    this.cb.onRtt(this, rtt);
    if (this.state === "STALE") {
      this.cb.log("info", `[#${this.id}] recovered from STALE (rtt=${rtt}ms)`);
      this.staleSince = null;
      if (this.graceTimer !== null) {
        clearTimeout(this.graceTimer);
        this.graceTimer = null;
      }
      this.transition("ALIVE");
    }
    if (this.state === "ALIVE") {
      this.deadline = now + WS_PONG_TIMEOUT_MS;
    }
  }

  private enterStale(): void {
    if (this.state !== "ALIVE") return;
    const now = this.cb.now();
    this.staleSince = now;
    this.deadline = now + WS_STALE_GRACE_MS;
    this.transition("STALE");
    if (this.graceTimer !== null) clearTimeout(this.graceTimer);
    this.graceTimer = setTimeout(() => {
      this.graceTimer = null;
      if (!this.destroyed && this.state === "STALE") {
        this.cb.log("warn", `[#${this.id}] STALE grace expired`);
        this.markDead(1006, "stale grace expired");
      }
    }, WS_STALE_GRACE_MS);
  }

  // ── teardown ─────────────────────────────────────────────────────────

  private markDead(code: number, reason: string): void {
    if (this.state === "DEAD" || this.destroyed) return;
    this.closeCode = code;
    this.closeReason = reason;
    this.deadline = this.cb.now() + WS_DEAD_LINGER_MS;
    this.clearOpTimers();
    this.transition("DEAD");
    // Linger briefly so the UI can show the dead state, then drop everything.
    if (this.lingerTimer !== null) clearTimeout(this.lingerTimer);
    this.lingerTimer = setTimeout(() => {
      this.lingerTimer = null;
      this.destroy();
    }, WS_DEAD_LINGER_MS);
  }

  private clearOpTimers(): void {
    if (this.connectTimer !== null) {
      clearTimeout(this.connectTimer);
      this.connectTimer = null;
    }
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
    if (this.graceTimer !== null) {
      clearTimeout(this.graceTimer);
      this.graceTimer = null;
    }
  }

  private clearAllTimers(): void {
    this.clearOpTimers();
    if (this.lingerTimer !== null) {
      clearTimeout(this.lingerTimer);
      this.lingerTimer = null;
    }
  }

  private transition(to: ConnectionState): void {
    if (this.state === to) return;
    const prev = this.state;
    this.state = to;
    this.cb.log("info", `[#${this.id}] ${prev} → ${to}`);
    this.cb.onStateChange(this, prev);
  }
}
