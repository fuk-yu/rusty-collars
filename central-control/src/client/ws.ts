import type { BoardCommand } from "../shared/protocol.js";
import type { WsServerMessage } from "../shared/types.js";

type MessageHandler = (msg: WsServerMessage) => void;
type StatusHandler = (connected: boolean) => void;

let socket: WebSocket | null = null;
let messageHandler: MessageHandler = () => {};
let statusHandler: StatusHandler = () => {};
let shouldConnect = false;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

export function setHandlers(onMessage: MessageHandler, onStatus: StatusHandler): void {
  messageHandler = onMessage;
  statusHandler = onStatus;
}

export function connect(): void {
  shouldConnect = true;
  doConnect();
}

export function disconnect(): void {
  shouldConnect = false;
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (socket) {
    socket.close();
    socket = null;
  }
}

function doConnect(): void {
  if (!shouldConnect) return;

  const protocol = location.protocol === "https:" ? "wss://" : "ws://";
  socket = new WebSocket(`${protocol}${location.host}/ws`);

  socket.addEventListener("open", () => {
    // Authentication happens via cookie sent with the upgrade request
    statusHandler(true);
  });

  socket.addEventListener("message", (event) => {
    const msg = JSON.parse(event.data as string) as WsServerMessage;
    messageHandler(msg);
  });

  socket.addEventListener("close", () => {
    statusHandler(false);
    scheduleReconnect();
  });

  socket.addEventListener("error", () => {
    // close event will fire after error
  });
}

function scheduleReconnect(): void {
  if (!shouldConnect) return;
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    doConnect();
  }, 2000);
}

export function sendDeviceCommand(deviceUuid: string, command: BoardCommand): void {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    throw new Error("WebSocket not connected");
  }
  socket.send(JSON.stringify({ type: "device_command", deviceUuid, command }));
}

export function isConnected(): boolean {
  return socket !== null && socket.readyState === WebSocket.OPEN;
}
