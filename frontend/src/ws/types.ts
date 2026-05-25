export const WS_CONNECT_TIMEOUT_MS = 5000;
export const WS_PING_INTERVAL_MS = 1000;
export const WS_PONG_TIMEOUT_MS = 3000;
export const WS_STALE_GRACE_MS = 3000;
export const WS_DEAD_LINGER_MS = 1500;

export const WS_BACKOFF_BASE_MS = 500;
export const WS_BACKOFF_CAP_MS = 30000;
export const WS_MAX_RECONNECT_ATTEMPTS = 15;

export const WS_MAX_LIVE_CONNECTIONS = 3;

export const WS_TICK_MS = 1000;
export const WS_TIME_JUMP_THRESHOLD_MS = 2000;

export const WS_RTT_WINDOW_30S_MS = 30_000;
export const WS_RTT_WINDOW_1M_MS = 60_000;
export const WS_RTT_WINDOW_5M_MS = 300_000;
export const WS_RTT_HISTORY_CAP = 1024;

export const WS_EVENT_LOG_CAP = 500;

// RFC 6455 close codes that mean: do not retry, the peer is permanently
// unhappy with our protocol — reconnecting will hit the same defect forever.
export const NON_RETRIABLE_CLOSE_CODES = new Set<number>([
  1002, 1003, 1007, 1008, 1009, 1010, 1011, 1015,
]);

export type ConnectionState = "NEW" | "ALIVE" | "STALE" | "DEAD";

export type WidgetState =
  | "alive"
  | "stale"
  | "connecting"
  | "deferred"
  | "terminal"
  | "dead";

export interface PingSample {
  readonly nonce: number;
  readonly sentAt: number;
}

export interface RttSample {
  readonly at: number;
  readonly rttMs: number;
}

export interface ConnectionStats {
  readonly id: number;
  readonly state: ConnectionState;
  readonly openedAt: number | null;
  readonly aliveSince: number | null;
  readonly staleSince: number | null;
  readonly deadline: number | null;
  readonly lastPingAt: number | null;
  readonly lastPongAt: number | null;
  readonly pendingPings: number;
  readonly lastRttMs: number | null;
  readonly closeCode: number | null;
  readonly closeReason: string | null;
}

export interface RttWindow {
  readonly count: number;
  readonly min: number;
  readonly median: number;
  readonly max: number;
}

export interface ManagerStats {
  readonly now: number;
  readonly connections: readonly ConnectionStats[];
  readonly activeId: number | null;
  readonly reconnectAttempt: number;
  readonly reconnectMaxAttempts: number;
  readonly reconnectAt: number | null;
  readonly reconnectDeferredOnHidden: boolean;
  readonly isTerminal: boolean;
  readonly terminalReason: string | null;
  readonly lostPings: number;
  readonly totalPings: number;
  readonly rtt30s: RttWindow;
  readonly rtt1m: RttWindow;
  readonly rtt5m: RttWindow;
}

export interface LogEntry {
  readonly at: number;
  readonly level: "info" | "warn" | "error";
  readonly text: string;
}

export type ServerMessageHandler = (msg: any) => void;
export type StatsListener = (stats: ManagerStats) => void;
