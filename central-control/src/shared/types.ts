import type { BoardCommand, Collar, EventLogEntry, Preset, PresetPreviewMessage, StateMessage } from "./protocol.js";

// ── Database entities ──

export interface User {
  id: string;
  login: string;
  passwordHash: string;
  totpSecret: string | null;
  totpVerified: boolean;
  createdAt: number;
}

export interface Session {
  token: string;
  userId: string;
  createdAt: number;
  expiresAt: number;
}

export interface Device {
  uuid: string;
  ownerUserId: string;
  nickname: string;
  createdAt: number;
}

export interface Invitation {
  id: string;
  token: string;
  fromUserId: string;
  toUserId: string | null;
  status: "pending" | "accepted" | "rejected";
  createdAt: number;
  respondedAt: number | null;
}

export interface ModeLimit {
  maxIntensity: number;
  maxDurationMs: number;
}

export interface BeepLimit {
  maxDurationMs: number;
}

export interface CollarPermission {
  collarName: string;
  shock: ModeLimit | null;
  vibrate: ModeLimit | null;
  beep: BeepLimit | null;
}

export interface DevicePermission {
  deviceUuid: string;
  granteeUserId: string;
  collars: CollarPermission[];
}

export interface UserPreset {
  id: string;
  userId: string;
  deviceUuid: string;
  preset: Preset;
}

// ── WebSocket protocol: UI ↔ Server ──

export interface WsAuthMessage {
  type: "authenticate";
  token: string;
}

export interface WsDeviceCommand {
  type: "device_command";
  deviceUuid: string;
  command: BoardCommand;
}

export type WsClientMessage = WsAuthMessage | WsDeviceCommand;

export interface DeviceSnapshot {
  uuid: string;
  nickname: string;
  isOwner: boolean;
  connected: boolean;
  rttMs: number | null;
  state: StateMessage | null;
  eventLogEvents: EventLogEntry[];
  permissions: DevicePermission | null;
}

export interface WsAuthenticatedMessage {
  type: "authenticated";
  userId: string;
  login: string;
}

export interface WsDeviceUpdateMessage {
  type: "device_update";
  device: DeviceSnapshot;
}

export interface WsDeviceDisconnectedMessage {
  type: "device_disconnected";
  deviceUuid: string;
}

export interface WsDevicesListMessage {
  type: "devices_list";
  devices: DeviceSnapshot[];
}

export interface WsErrorMessage {
  type: "error";
  message: string;
}

export interface WsInfoMessage {
  type: "info";
  message: string;
}

export type WsServerMessage =
  | WsAuthenticatedMessage
  | WsDeviceUpdateMessage
  | WsDeviceDisconnectedMessage
  | WsDevicesListMessage
  | WsErrorMessage
  | WsInfoMessage
  | PresetPreviewMessage;

// ── REST API types ──

export interface ApiUser {
  id: string;
  login: string;
  totpEnabled: boolean;
}

export interface ApiDevice {
  uuid: string;
  nickname: string;
  ownerLogin: string;
  connected: boolean;
}

export interface ApiInvitation {
  id: string;
  token: string;
  fromLogin: string;
  toLogin: string | null;
  status: "pending" | "accepted" | "rejected";
  createdAt: number;
}

export interface LoginResponse {
  requiresTotp: boolean;
  pendingToken?: string;
  sessionToken?: string;
  user?: ApiUser;
}

export interface TotpSetupResponse {
  secret: string;
  uri: string;
  qrDataUrl: string;
}
