import { readFile } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket, { WebSocketServer } from "ws";

import type {
  BoardCommand,
  BoardIncomingMessage,
  BoardSnapshot,
  ErrorMessage,
  EventLogEntry,
  EventLogEventMessage,
  EventLogStateMessage,
  PingMessage,
  PongMessage,
  PresetPreviewMessage,
  SnapshotMessage,
  StateMessage,
  UiErrorMessage,
  UiIncomingMessage,
  UiInfoMessage,
  UiOutgoingMessage,
} from "./protocol.js";

const BOARD_PING_INTERVAL_MS = 5_000;
const BOARD_PING_TIMEOUT_MS = 15_000;

interface BoardConnection {
  id: string;
  socket: WebSocket;
  peer: string;
  path: string;
  connectedAtMs: number;
  lastSeenAtMs: number;
  nextPingNonce: number;
  pendingPings: Map<number, number>;
  rttMs: number | null;
  lastError: string | null;
  receivedMessages: number;
  remoteControlStatus: BoardSnapshot["remote_control_status"];
  state: StateMessage | null;
  eventLogEnabled: boolean;
  eventLogEvents: EventLogEntry[];
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const serverRoot = path.resolve(__dirname, "..");
const publicRoot = path.join(serverRoot, "dist", "public");
const serverStartedAtMs = Date.now();

const boards = new Map<string, BoardConnection>();
const uiClients = new Set<WebSocket>();
let nextBoardId = 1;

const httpServer = http.createServer(async (req, res) => {
  try {
    const requestUrl = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
    const pathname = requestUrl.pathname === "/" ? "/index.html" : requestUrl.pathname;
    const safePath = path.normalize(pathname).replace(/^(\.\.(\/|\\|$))+/, "");
    const filePath = path.resolve(publicRoot, `.${safePath}`);

    if (!filePath.startsWith(publicRoot)) {
      res.writeHead(403).end("Forbidden");
      return;
    }

    const file = await readFile(filePath);
    res.writeHead(200, { "Content-Type": contentTypeFor(filePath) }).end(file);
  } catch {
    res
      .writeHead(404, { "Content-Type": "text/plain; charset=utf-8" })
      .end("Not found. Run `npm run build` in ./server before starting the server.");
  }
});

const boardWss = new WebSocketServer({ noServer: true, perMessageDeflate: false });
const uiWss = new WebSocketServer({ noServer: true, perMessageDeflate: false });

boardWss.on("connection", (socket, request) => {
  const requestUrl = new URL(request.url ?? "/device", `http://${request.headers.host ?? "localhost"}`);

  // Extract device_id from URL path: /device/{device_id}
  const pathSegments = requestUrl.pathname.split("/").filter(Boolean);
  const deviceId = pathSegments.length > 1 ? pathSegments.slice(1).join("/") : null;

  let boardId: string;
  if (deviceId) {
    boardId = deviceId;
    // Close existing connection from same device if still open
    const existing = boards.get(boardId);
    if (existing) {
      console.log(`[board ${boardId}] replacing existing connection from ${existing.peer}`);
      existing.socket.close(1000, "Replaced by new connection");
      boards.delete(boardId);
    }
  } else {
    boardId = `board-${String(nextBoardId).padStart(4, "0")}`;
    nextBoardId += 1;
  }

  const board: BoardConnection = {
    id: boardId,
    socket,
    peer: `${request.socket.remoteAddress ?? "unknown"}:${request.socket.remotePort ?? 0}`,
    path: requestUrl.pathname,
    connectedAtMs: Date.now(),
    lastSeenAtMs: Date.now(),
    nextPingNonce: 0,
    pendingPings: new Map<number, number>(),
    rttMs: null,
    lastError: null,
    receivedMessages: 0,
    remoteControlStatus: null,
    state: null,
    eventLogEnabled: false,
    eventLogEvents: [],
  };

  boards.set(boardId, board);
  console.log(`[board ${boardId}] connected from ${board.peer} ${board.path}`);
  broadcastSnapshot();

  socket.on("message", (data, isBinary) => {
    if (isBinary) {
      board.lastError = "Binary messages are not supported";
      broadcastSnapshot();
      return;
    }

    try {
      const message = parseBoardIncomingMessage(data.toString());
      board.receivedMessages += 1;
      board.lastSeenAtMs = Date.now();
      handleBoardMessage(board, message);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      board.lastError = `Invalid board message: ${message}`;
      console.error(`[board ${board.id}] invalid message: ${message}`);
      broadcastSnapshot();
    }
  });

  socket.on("close", (code, reason) => {
    boards.delete(boardId);
    console.log(
      `[board ${boardId}] disconnected after ${Date.now() - board.connectedAtMs}ms code=${code} reason=${formatCloseReason(reason)}`,
    );
    broadcastSnapshot();
  });

  socket.on("error", (error) => {
    board.lastError = `Socket error: ${error.message}`;
    console.error(`[board ${board.id}] socket error: ${error.message}`);
    broadcastSnapshot();
  });
});

uiWss.on("connection", (socket) => {
  uiClients.add(socket);
  sendUi(socket, makeSnapshot());

  socket.on("message", (data, isBinary) => {
    if (isBinary) {
      sendUiError(socket, "Binary messages are not supported");
      return;
    }

    try {
      const message = parseUiIncomingMessage(data.toString());
      handleUiMessage(socket, message);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      sendUiError(socket, `Invalid UI message: ${message}`);
    }
  });

  socket.on("close", () => {
    uiClients.delete(socket);
  });

  socket.on("error", () => {
    uiClients.delete(socket);
  });
});

httpServer.on("upgrade", (request, socket, head) => {
  const requestUrl = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);
  if (requestUrl.pathname === "/ui") {
    uiWss.handleUpgrade(request, socket, head, (ws) => {
      uiWss.emit("connection", ws, request);
    });
    return;
  }

  if (requestUrl.pathname === "/device" || requestUrl.pathname.startsWith("/device/")) {
    boardWss.handleUpgrade(request, socket, head, (ws) => {
      boardWss.emit("connection", ws, request);
    });
    return;
  }

  socket.destroy();
});

setInterval(() => {
  const now = Date.now();
  for (const board of boards.values()) {
    if (board.socket.readyState !== WebSocket.OPEN) {
      continue;
    }

    if (board.receivedMessages === 0) {
      continue;
    }

    for (const [nonce, startedAtMs] of board.pendingPings) {
      if (now - startedAtMs > BOARD_PING_TIMEOUT_MS) {
        board.lastError = `Board ping timeout for nonce ${nonce}`;
        board.socket.close(1011, "Ping timeout");
        broadcastSnapshot();
        break;
      }
    }

    if (board.pendingPings.size > 0 || board.socket.readyState !== WebSocket.OPEN) {
      continue;
    }

    const nonce = ++board.nextPingNonce;
    const ping: PingMessage = { type: "ping", nonce };
    board.pendingPings.set(nonce, now);
    sendBoardCommand(board, ping);
  }
}, BOARD_PING_INTERVAL_MS);

const port = parsePort(process.argv.slice(2), process.env.PORT);
httpServer.listen(port, "0.0.0.0", () => {
  console.log(`Debug control server listening on http://0.0.0.0:${port}`);
  console.log(`Boards should connect to ws://HOST:${port}/device`);
});

function handleBoardMessage(board: BoardConnection, message: BoardIncomingMessage): void {
  switch (message.type) {
    case "state":
      board.state = message;
      broadcastSnapshot();
      return;
    case "event_log_state":
      board.eventLogEnabled = message.enabled;
      board.eventLogEvents = message.events.slice(-100);
      broadcastSnapshot();
      return;
    case "event_log_event":
      board.eventLogEvents.push(message.event);
      if (board.eventLogEvents.length > 100) {
        board.eventLogEvents.splice(0, board.eventLogEvents.length - 100);
      }
      broadcastSnapshot();
      return;
    case "remote_control_status":
      board.remoteControlStatus = message.status;
      broadcastSnapshot();
      return;
    case "preset_preview":
      broadcastToUi(message);
      return;
    case "error":
      board.lastError = message.message;
      broadcastSnapshot();
      return;
    case "ping":
      sendBoardCommand(board, { type: "pong", nonce: message.nonce });
      return;
    case "pong":
      handleBoardPong(board, message);
      return;
    default:
      assertNever(message);
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

  broadcastSnapshot();
}

function handleUiMessage(socket: WebSocket, message: UiIncomingMessage): void {
  switch (message.type) {
    case "board_command": {
      const board = boards.get(message.board_id);
      if (!board) {
        sendUiError(socket, `Unknown board: ${message.board_id}`);
        return;
      }

      sendBoardCommand(board, message.command);
      const info: UiInfoMessage = {
        type: "ui_info",
        message: `Sent ${message.command.type} to ${board.id}`,
      };
      sendUi(socket, info);
      return;
    }
    case "ping_board": {
      const board = boards.get(message.board_id);
      if (!board) {
        sendUiError(socket, `Unknown board: ${message.board_id}`);
        return;
      }

      const nonce = ++board.nextPingNonce;
      board.pendingPings.set(nonce, Date.now());
      sendBoardCommand(board, { type: "ping", nonce });
      return;
    }
    default:
      assertNever(message);
  }
}

function sendBoardCommand(board: BoardConnection, command: BoardCommand): void {
  if (board.socket.readyState !== WebSocket.OPEN) {
    throw new Error(`Board ${board.id} is not connected`);
  }

  board.socket.send(JSON.stringify(command));
}

function makeSnapshot(): SnapshotMessage {
  const boardSnapshots: BoardSnapshot[] = [...boards.values()]
    .sort((left, right) => left.connectedAtMs - right.connectedAtMs)
    .map((board) => ({
      id: board.id,
      peer: board.peer,
      path: board.path,
      connected_at_ms: board.connectedAtMs,
      last_seen_at_ms: board.lastSeenAtMs,
      rtt_ms: board.rttMs,
      last_error: board.lastError,
      remote_control_status: board.remoteControlStatus,
      state: board.state,
      event_log_enabled: board.eventLogEnabled,
      event_log_events: board.eventLogEvents,
    }));

  return {
    type: "snapshot",
    server_started_at_ms: serverStartedAtMs,
    boards: boardSnapshots,
  };
}

function broadcastToUi(message: UiOutgoingMessage): void {
  const payload = JSON.stringify(message);
  for (const client of [...uiClients]) {
    if (client.readyState !== WebSocket.OPEN) {
      uiClients.delete(client);
      continue;
    }
    client.send(payload);
  }
}

function broadcastSnapshot(): void {
  const snapshot = makeSnapshot();
  const payload = JSON.stringify(snapshot);
  for (const client of [...uiClients]) {
    if (client.readyState !== WebSocket.OPEN) {
      uiClients.delete(client);
      continue;
    }
    client.send(payload);
  }
}

function sendUi(socket: WebSocket, message: UiOutgoingMessage): void {
  if (socket.readyState !== WebSocket.OPEN) {
    return;
  }
  socket.send(JSON.stringify(message));
}

function sendUiError(socket: WebSocket, message: string): void {
  const errorMessage: UiErrorMessage = { type: "ui_error", message };
  sendUi(socket, errorMessage);
}

function contentTypeFor(filePath: string): string {
  if (filePath.endsWith(".html")) {
    return "text/html; charset=utf-8";
  }
  if (filePath.endsWith(".js")) {
    return "text/javascript; charset=utf-8";
  }
  if (filePath.endsWith(".map")) {
    return "application/json; charset=utf-8";
  }
  if (filePath.endsWith(".css")) {
    return "text/css; charset=utf-8";
  }
  return "application/octet-stream";
}

function parsePort(argv: string[], envPort: string | undefined): number {
  const cliPort = argv.find((arg, index) => arg === "--port" && argv[index + 1] !== undefined)
    ? Number(argv[argv.indexOf("--port") + 1])
    : undefined;
  const port = cliPort ?? (envPort !== undefined ? Number(envPort) : 8080);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`Invalid port: ${port}`);
  }
  return port;
}

function parseBoardIncomingMessage(raw: string): BoardIncomingMessage {
  const value = JSON.parse(raw) as unknown;
  assertRecord(value, "Board message must be an object");
  assertString(value.type, "Board message.type must be a string");
  return value as unknown as BoardIncomingMessage;
}

function parseUiIncomingMessage(raw: string): UiIncomingMessage {
  const value = JSON.parse(raw) as unknown;
  assertRecord(value, "UI message must be an object");
  assertString(value.type, "UI message.type must be a string");
  return value as unknown as UiIncomingMessage;
}

function assertRecord(value: unknown, message: string): asserts value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(message);
  }
}

function assertString(value: unknown, message: string): asserts value is string {
  if (typeof value !== "string") {
    throw new Error(message);
  }
}

function formatCloseReason(reason: Buffer): string {
  if (reason.length === 0) {
    return "<empty>";
  }

  return reason.toString("utf8");
}

function assertNever(value: never): never {
  throw new Error(`Unhandled value: ${JSON.stringify(value)}`);
}
