import WebSocket from "ws";
import type { BoardCommand, BoardIncomingMessage, EventLogEntry, PingMessage, PongMessage, StateMessage } from "../shared/protocol.js";

const BOARD_PING_INTERVAL_MS = 5_000;
const BOARD_PING_TIMEOUT_MS = 15_000;

export interface BoardConnection {
  deviceId: string;
  socket: WebSocket;
  peer: string;
  connectedAtMs: number;
  lastSeenAtMs: number;
  nextPingNonce: number;
  pendingPings: Map<number, number>;
  rttMs: number | null;
  lastError: string | null;
  state: StateMessage | null;
  eventLogEvents: EventLogEntry[];
}

type BoardChangeListener = (deviceId: string) => void;

const boards = new Map<string, BoardConnection>();
const listeners = new Set<BoardChangeListener>();

export function onBoardChange(listener: BoardChangeListener): void {
  listeners.add(listener);
}

function notifyBoardChange(deviceId: string): void {
  for (const listener of listeners) {
    listener(deviceId);
  }
}

export function getBoard(deviceId: string): BoardConnection | undefined {
  return boards.get(deviceId);
}

export function isBoardConnected(deviceId: string): boolean {
  const board = boards.get(deviceId);
  return board !== undefined && board.socket.readyState === WebSocket.OPEN;
}

export function getAllBoards(): Map<string, BoardConnection> {
  return boards;
}

export function handleBoardConnection(socket: WebSocket, deviceId: string, peer: string): void {
  const existing = boards.get(deviceId);
  if (existing) {
    console.log(`[board ${deviceId}] replacing existing connection from ${existing.peer}`);
    existing.socket.close(1000, "Replaced by new connection");
    boards.delete(deviceId);
  }

  const board: BoardConnection = {
    deviceId,
    socket,
    peer,
    connectedAtMs: Date.now(),
    lastSeenAtMs: Date.now(),
    nextPingNonce: 0,
    pendingPings: new Map(),
    rttMs: null,
    lastError: null,
    state: null,
    eventLogEvents: [],
  };

  boards.set(deviceId, board);
  console.log(`[board ${deviceId}] connected from ${peer}`);
  notifyBoardChange(deviceId);

  socket.on("message", (data, isBinary) => {
    if (isBinary) {
      board.lastError = "Binary messages are not supported";
      notifyBoardChange(deviceId);
      return;
    }

    try {
      const message = JSON.parse(data.toString()) as BoardIncomingMessage;
      board.lastSeenAtMs = Date.now();
      handleBoardMessage(board, message);
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      board.lastError = `Invalid board message: ${msg}`;
      console.error(`[board ${deviceId}] invalid message: ${msg}`);
      notifyBoardChange(deviceId);
    }
  });

  socket.on("close", (code, reason) => {
    boards.delete(deviceId);
    const reasonStr = reason.length === 0 ? "<empty>" : reason.toString("utf8");
    console.log(`[board ${deviceId}] disconnected code=${code} reason=${reasonStr}`);
    notifyBoardChange(deviceId);
  });

  socket.on("error", (error) => {
    board.lastError = `Socket error: ${error.message}`;
    console.error(`[board ${deviceId}] socket error: ${error.message}`);
    notifyBoardChange(deviceId);
  });
}

function handleBoardMessage(board: BoardConnection, message: BoardIncomingMessage): void {
  switch (message.type) {
    case "state":
      board.state = message;
      notifyBoardChange(board.deviceId);
      return;
    case "event_log_state":
      board.eventLogEvents = message.events.slice(-100);
      notifyBoardChange(board.deviceId);
      return;
    case "event_log_event":
      board.eventLogEvents.push(message.event);
      if (board.eventLogEvents.length > 100) {
        board.eventLogEvents.splice(0, board.eventLogEvents.length - 100);
      }
      notifyBoardChange(board.deviceId);
      return;
    case "remote_control_status":
      notifyBoardChange(board.deviceId);
      return;
    case "preset_preview":
      notifyPresetPreview(board.deviceId, message);
      return;
    case "error":
      board.lastError = message.message;
      notifyBoardChange(board.deviceId);
      return;
    case "ping":
      sendToBoard(board, { type: "pong", nonce: message.nonce });
      return;
    case "pong":
      handleBoardPong(board, message);
      return;
  }
}

function handleBoardPong(board: BoardConnection, message: PongMessage): void {
  const startedAtMs = board.pendingPings.get(message.nonce);
  if (startedAtMs !== undefined) {
    board.pendingPings.delete(message.nonce);
    board.rttMs = Date.now() - startedAtMs;
  }
  if (message.server_uptime_s !== undefined && board.state) {
    board.state.server_uptime_s = message.server_uptime_s;
  }
  notifyBoardChange(board.deviceId);
}

export function sendToBoard(board: BoardConnection, command: BoardCommand): void {
  if (board.socket.readyState !== WebSocket.OPEN) {
    throw new Error(`Board ${board.deviceId} is not connected`);
  }
  board.socket.send(JSON.stringify(command));
}

export function sendCommandToDevice(deviceId: string, command: BoardCommand): void {
  const board = boards.get(deviceId);
  if (!board) throw new Error(`Device ${deviceId} is not connected`);
  sendToBoard(board, command);
}

// Preset preview forwarding
type PresetPreviewListener = (deviceId: string, message: BoardIncomingMessage) => void;
const presetPreviewListeners = new Set<PresetPreviewListener>();

export function onPresetPreview(listener: PresetPreviewListener): void {
  presetPreviewListeners.add(listener);
}

function notifyPresetPreview(deviceId: string, message: BoardIncomingMessage): void {
  for (const listener of presetPreviewListeners) {
    listener(deviceId, message);
  }
}

// Periodic ping
setInterval(() => {
  const now = Date.now();
  for (const board of boards.values()) {
    if (board.socket.readyState !== WebSocket.OPEN) continue;
    if (!board.state) continue;

    for (const [nonce, startedAtMs] of board.pendingPings) {
      if (now - startedAtMs > BOARD_PING_TIMEOUT_MS) {
        board.lastError = `Board ping timeout for nonce ${nonce}`;
        board.socket.close(1011, "Ping timeout");
        notifyBoardChange(board.deviceId);
        break;
      }
    }

    if (board.pendingPings.size > 0 || board.socket.readyState !== WebSocket.OPEN) continue;

    const nonce = ++board.nextPingNonce;
    const ping: PingMessage = { type: "ping", nonce };
    board.pendingPings.set(nonce, now);
    sendToBoard(board, ping);
  }
}, BOARD_PING_INTERVAL_MS);
