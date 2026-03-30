import http from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import WebSocket, { WebSocketServer } from "ws";

import { initDb } from "./db.js";
import * as db from "./db.js";
import * as auth from "./auth.js";
import * as boards from "./boards.js";
import { checkCommandPermission, checkPresetPermission, isPresetWithinLimits } from "./permissions.js";
import type { BoardCommand, PresetPreviewMessage, CommandMode, Preset } from "../shared/protocol.js";
import type {
  ApiDevice,
  ApiInvitation,
  ApiUser,
  DevicePermission,
  DeviceSnapshot,
  LoginResponse,
  User,
  WsClientMessage,
  WsServerMessage,
} from "../shared/types.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const PROJECT_ROOT = path.resolve(__dirname, "..", "..");
const CLIENT_DIST = path.join(PROJECT_ROOT, "dist", "client");
const DATA_DIR = path.join(PROJECT_ROOT, "data");

initDb(DATA_DIR);

// ── HTTP routing helpers ──

type Handler = (req: http.IncomingMessage, res: http.ServerResponse, params: Record<string, string>) => Promise<void>;

interface Route {
  method: string;
  pattern: RegExp;
  paramNames: string[];
  handler: Handler;
}

const routes: Route[] = [];

function route(method: string, pathPattern: string, handler: Handler): void {
  const paramNames: string[] = [];
  const regexStr = pathPattern.replace(/:(\w+)/g, (_, name: string) => {
    paramNames.push(name);
    return "([^/]+)";
  });
  routes.push({ method, pattern: new RegExp(`^${regexStr}$`), paramNames, handler });
}

async function readJsonBody<T>(req: http.IncomingMessage): Promise<T> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")) as T);
      } catch {
        reject(new Error("Invalid JSON body"));
      }
    });
    req.on("error", reject);
  });
}

function json(res: http.ServerResponse, status: number, body: unknown): void {
  const data = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  res.end(data);
}

function setSessionCookie(res: http.ServerResponse, token: string): void {
  res.setHeader("Set-Cookie", `session=${token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=${7 * 24 * 60 * 60}`);
}

function clearSessionCookie(res: http.ServerResponse): void {
  res.setHeader("Set-Cookie", "session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
}

function requireAuth(req: http.IncomingMessage): User {
  const token = auth.extractSessionToken(req.headers.cookie, req.headers.authorization);
  if (!token) throw new HttpError(401, "Not authenticated");
  const session = auth.validateSession(token);
  if (!session) throw new HttpError(401, "Session expired");
  const user = db.getUserById(session.userId);
  if (!user) throw new HttpError(401, "User not found");
  return user;
}

class HttpError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

function toApiUser(user: User): ApiUser {
  return { id: user.id, login: user.login, totpEnabled: user.totpVerified };
}

// ── Pending TOTP tokens (in-memory, short-lived) ──

const pendingTotpTokens = new Map<string, { userId: string; expiresAt: number }>();

setInterval(() => {
  const now = Date.now();
  for (const [token, data] of pendingTotpTokens) {
    if (data.expiresAt < now) pendingTotpTokens.delete(token);
  }
}, 60_000);

// ── Auth API ──

route("POST", "/api/auth/signup", async (req, res) => {
  const body = await readJsonBody<{ login: string; password: string }>(req);
  if (!body.login || body.login.length < 3) throw new HttpError(400, "Login must be at least 3 characters");
  if (!body.password || body.password.length < 8) throw new HttpError(400, "Password must be at least 8 characters");
  if (db.getUserByLogin(body.login)) throw new HttpError(409, "Login already taken");

  const user: User = {
    id: auth.generateId(),
    login: body.login,
    passwordHash: await auth.hashPassword(body.password),
    totpSecret: null,
    totpVerified: false,
    createdAt: Date.now(),
  };
  db.putUser(user);
  const session = auth.createSession(user.id);
  setSessionCookie(res, session.token);
  json(res, 201, { sessionToken: session.token, user: toApiUser(user) });
});

route("POST", "/api/auth/login", async (req, res) => {
  const body = await readJsonBody<{ login: string; password: string }>(req);
  const user = db.getUserByLogin(body.login);
  if (!user) throw new HttpError(401, "Invalid login or password");
  const valid = await auth.verifyPassword(body.password, user.passwordHash);
  if (!valid) throw new HttpError(401, "Invalid login or password");

  if (user.totpVerified && user.totpSecret) {
    const pendingToken = auth.generateToken();
    pendingTotpTokens.set(pendingToken, { userId: user.id, expiresAt: Date.now() + 5 * 60 * 1000 });
    const result: LoginResponse = { requiresTotp: true, pendingToken };
    json(res, 200, result);
    return;
  }

  const session = auth.createSession(user.id);
  setSessionCookie(res, session.token);
  const result: LoginResponse = { requiresTotp: false, sessionToken: session.token, user: toApiUser(user) };
  json(res, 200, result);
});

route("POST", "/api/auth/totp/validate", async (req, res) => {
  const body = await readJsonBody<{ pendingToken: string; code: string }>(req);
  const pending = pendingTotpTokens.get(body.pendingToken);
  if (!pending || pending.expiresAt < Date.now()) throw new HttpError(401, "Expired or invalid pending token");

  const user = db.getUserById(pending.userId);
  if (!user || !user.totpSecret) throw new HttpError(401, "Invalid state");

  if (!auth.verifyTotpCode(user.totpSecret, body.code)) {
    throw new HttpError(401, "Invalid TOTP code");
  }

  pendingTotpTokens.delete(body.pendingToken);
  const session = auth.createSession(user.id);
  setSessionCookie(res, session.token);
  json(res, 200, { sessionToken: session.token, user: toApiUser(user) });
});

route("POST", "/api/auth/totp/setup", async (req, res) => {
  const user = requireAuth(req);
  const setup = await auth.generateTotpSetup(user.login);
  user.totpSecret = setup.secret;
  user.totpVerified = false;
  db.putUser(user);
  json(res, 200, setup);
});

route("POST", "/api/auth/totp/verify", async (req, res) => {
  const user = requireAuth(req);
  const body = await readJsonBody<{ code: string }>(req);
  if (!user.totpSecret) throw new HttpError(400, "TOTP not set up");
  if (!auth.verifyTotpCode(user.totpSecret, body.code)) {
    throw new HttpError(400, "Invalid TOTP code");
  }
  user.totpVerified = true;
  db.putUser(user);
  json(res, 200, { success: true });
});

route("POST", "/api/auth/logout", async (req, res) => {
  const token = auth.extractSessionToken(req.headers.cookie, req.headers.authorization);
  if (token) auth.destroySession(token);
  clearSessionCookie(res);
  json(res, 200, { success: true });
});

route("GET", "/api/auth/me", async (req, res) => {
  const user = requireAuth(req);
  json(res, 200, toApiUser(user));
});

// ── Device API ──

route("GET", "/api/devices", async (req, res) => {
  const user = requireAuth(req);
  const ownedDevices = db.listDevicesByOwner(user.id);
  const result: ApiDevice[] = ownedDevices.map((d) => ({
    uuid: d.uuid,
    nickname: d.nickname,
    ownerLogin: user.login,
    connected: boards.isBoardConnected(d.uuid),
  }));
  json(res, 200, result);
});

route("GET", "/api/devices/shared", async (req, res) => {
  const user = requireAuth(req);
  const perms = db.listPermissionsForUser(user.id);
  const result: ApiDevice[] = [];
  for (const perm of perms) {
    const device = db.getDevice(perm.deviceUuid);
    if (!device) continue;
    const owner = db.getUserById(device.ownerUserId);
    result.push({
      uuid: device.uuid,
      nickname: device.nickname,
      ownerLogin: owner?.login ?? "unknown",
      connected: boards.isBoardConnected(device.uuid),
    });
  }
  json(res, 200, result);
});

route("POST", "/api/devices", async (req, res) => {
  const user = requireAuth(req);
  const body = await readJsonBody<{ nickname: string }>(req);
  if (!body.nickname?.trim()) throw new HttpError(400, "Nickname is required");

  const device = {
    uuid: auth.generateId(),
    ownerUserId: user.id,
    nickname: body.nickname.trim(),
    createdAt: Date.now(),
  };
  db.putDevice(device);

  const host = req.headers.host ?? "localhost:3001";
  const protocol = req.headers["x-forwarded-proto"] === "https" ? "wss" : "ws";
  const url = `${protocol}://${host}/device/${device.uuid}`;

  json(res, 201, { device, connectionUrl: url });
});

route("DELETE", "/api/devices/:uuid", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.uuid!);
  if (!device) throw new HttpError(404, "Device not found");
  if (device.ownerUserId !== user.id) throw new HttpError(403, "Not the device owner");
  db.deleteDevice(params.uuid!);
  json(res, 200, { success: true });
});

// ── Invitation API ──

route("GET", "/api/invitations", async (req, res) => {
  const user = requireAuth(req);
  const invitations = db.listInvitationsByUser(user.id);
  const result: ApiInvitation[] = invitations.map((inv) => ({
    id: inv.id,
    name: inv.name,
    token: inv.token,
    fromLogin: db.getUserById(inv.fromUserId)?.login ?? "unknown",
    toLogin: inv.toUserId ? (db.getUserById(inv.toUserId)?.login ?? "unknown") : null,
    status: inv.status,
    createdAt: inv.createdAt,
  }));
  json(res, 200, result);
});

route("POST", "/api/invitations", async (req, res) => {
  const user = requireAuth(req);
  const body = await readJsonBody<{ name: string }>(req);
  if (!body.name?.trim()) throw new HttpError(400, "Invitation name is required");

  const invitation = {
    id: auth.generateId(),
    name: body.name.trim(),
    token: auth.generateToken(),
    fromUserId: user.id,
    toUserId: null,
    status: "pending" as const,
    createdAt: Date.now(),
    respondedAt: null,
  };
  db.putInvitation(invitation);

  const host = req.headers.host ?? "localhost:8099";
  const protocol = req.headers["x-forwarded-proto"] === "https" ? "https" : "http";
  const link = `${protocol}://${host}/#/invite/${invitation.token}`;

  json(res, 201, { invitation: { ...invitation, fromLogin: user.login, toLogin: null }, link });
});

route("GET", "/api/invitations/:token", async (req, res, params) => {
  requireAuth(req);
  const invitation = db.getInvitation(params.token!);
  if (!invitation) throw new HttpError(404, "Invitation not found");
  const fromUser = db.getUserById(invitation.fromUserId);
  json(res, 200, {
    id: invitation.id,
    name: invitation.name,
    fromLogin: fromUser?.login ?? "unknown",
    status: invitation.status,
  });
});

route("POST", "/api/invitations/:token/accept", async (req, res, params) => {
  const user = requireAuth(req);
  const invitation = db.getInvitation(params.token!);
  if (!invitation) throw new HttpError(404, "Invitation not found");
  if (invitation.status !== "pending") throw new HttpError(400, "Invitation already responded to");
  if (invitation.fromUserId === user.id) throw new HttpError(400, "Cannot accept your own invitation");

  invitation.toUserId = user.id;
  invitation.status = "accepted";
  invitation.respondedAt = Date.now();
  db.putInvitation(invitation);
  json(res, 200, { success: true });
});

route("POST", "/api/invitations/:token/reject", async (req, res, params) => {
  const user = requireAuth(req);
  const invitation = db.getInvitation(params.token!);
  if (!invitation) throw new HttpError(404, "Invitation not found");
  if (invitation.status !== "pending") throw new HttpError(400, "Invitation already responded to");

  invitation.toUserId = user.id;
  invitation.status = "rejected";
  invitation.respondedAt = Date.now();
  db.putInvitation(invitation);
  json(res, 200, { success: true });
});

route("DELETE", "/api/invitations/:token", async (req, res, params) => {
  const user = requireAuth(req);
  const invitation = db.getInvitation(params.token!);
  if (!invitation) throw new HttpError(404, "Invitation not found");
  if (invitation.fromUserId !== user.id) throw new HttpError(403, "Not your invitation");
  if (invitation.status !== "pending") throw new HttpError(400, "Can only cancel pending invitations");
  db.deleteInvitation(params.token!);
  json(res, 200, { success: true });
});

// ── Permission API ──

route("GET", "/api/permissions/:deviceUuid", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.deviceUuid!);
  if (!device) throw new HttpError(404, "Device not found");
  if (device.ownerUserId !== user.id) throw new HttpError(403, "Not the device owner");
  const perms = db.listPermissionsForDevice(params.deviceUuid!);
  const result = perms.map((p) => ({
    ...p,
    granteeLogin: db.getUserById(p.granteeUserId)?.login ?? "unknown",
  }));
  json(res, 200, result);
});

route("GET", "/api/permissions/:deviceUuid/my", async (req, res, params) => {
  const user = requireAuth(req);
  const perm = db.getPermission(params.deviceUuid!, user.id);
  json(res, 200, perm ?? null);
});

route("PUT", "/api/permissions/:deviceUuid/:userId", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.deviceUuid!);
  if (!device) throw new HttpError(404, "Device not found");
  if (device.ownerUserId !== user.id) throw new HttpError(403, "Not the device owner");

  const body = await readJsonBody<{ collars: DevicePermission["collars"] }>(req);
  const perm: DevicePermission = {
    deviceUuid: params.deviceUuid!,
    granteeUserId: params.userId!,
    collars: body.collars,
  };
  db.putPermission(perm);
  notifyPermissionChange(params.deviceUuid!, params.userId!);
  json(res, 200, perm);
});

route("DELETE", "/api/permissions/:deviceUuid/:userId", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.deviceUuid!);
  if (!device) throw new HttpError(404, "Device not found");
  if (device.ownerUserId !== user.id) throw new HttpError(403, "Not the device owner");
  db.deletePermission(params.deviceUuid!, params.userId!);
  notifyPermissionChange(params.deviceUuid!, params.userId!);
  json(res, 200, { success: true });
});

// ── Invited users list (for device owner) ──

route("GET", "/api/devices/:uuid/users", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.uuid!);
  if (!device) throw new HttpError(404, "Device not found");
  if (device.ownerUserId !== user.id) throw new HttpError(403, "Not the device owner");

  const inviteeIds = db.listAcceptedInviteeIds(user.id);
  const result = inviteeIds.map((id) => {
    const invitee = db.getUserById(id);
    const perm = db.getPermission(params.uuid!, id);
    return {
      userId: id,
      login: invitee?.login ?? "unknown",
      hasPermission: perm !== undefined,
      permission: perm ?? null,
    };
  });
  json(res, 200, result);
});

// ── User Presets API ──

route("GET", "/api/user-presets/:deviceUuid", async (req, res, params) => {
  const user = requireAuth(req);
  const presets = db.listUserPresets(user.id, params.deviceUuid!);
  json(res, 200, presets);
});

route("POST", "/api/user-presets/:deviceUuid", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.deviceUuid!);
  if (!device) throw new HttpError(404, "Device not found");

  // Check permission if not owner
  if (device.ownerUserId !== user.id) {
    const perm = db.getPermission(params.deviceUuid!, user.id);
    if (!perm) throw new HttpError(403, "No access to this device");
    const body = await readJsonBody<{ preset: Preset }>(req);
    const error = checkPresetPermission(perm, body.preset);
    if (error) throw new HttpError(400, `Preset exceeds limits: ${error}`);

    const userPreset = {
      id: auth.generateId(),
      userId: user.id,
      deviceUuid: params.deviceUuid!,
      preset: body.preset,
    };
    db.putUserPreset(userPreset);
    json(res, 201, userPreset);
    return;
  }

  const body = await readJsonBody<{ preset: Preset }>(req);
  const userPreset = {
    id: auth.generateId(),
    userId: user.id,
    deviceUuid: params.deviceUuid!,
    preset: body.preset,
  };
  db.putUserPreset(userPreset);
  json(res, 201, userPreset);
});

route("PUT", "/api/user-presets/:deviceUuid/:presetId", async (req, res, params) => {
  const user = requireAuth(req);
  const existing = db.getUserPreset(user.id, params.deviceUuid!, params.presetId!);
  if (!existing) throw new HttpError(404, "Preset not found");

  const device = db.getDevice(params.deviceUuid!);
  if (!device) throw new HttpError(404, "Device not found");

  const body = await readJsonBody<{ preset: Preset }>(req);

  if (device.ownerUserId !== user.id) {
    const perm = db.getPermission(params.deviceUuid!, user.id);
    if (!perm) throw new HttpError(403, "No access to this device");
    const error = checkPresetPermission(perm, body.preset);
    if (error) throw new HttpError(400, `Preset exceeds limits: ${error}`);
  }

  existing.preset = body.preset;
  db.putUserPreset(existing);
  json(res, 200, existing);
});

route("DELETE", "/api/user-presets/:deviceUuid/:presetId", async (req, res, params) => {
  const user = requireAuth(req);
  const existing = db.getUserPreset(user.id, params.deviceUuid!, params.presetId!);
  if (!existing) throw new HttpError(404, "Preset not found");
  db.deleteUserPreset(user.id, params.deviceUuid!, params.presetId!);
  json(res, 200, { success: true });
});

// ── Owner presets (device-stored presets viewable by invitees) ──

route("GET", "/api/devices/:uuid/owner-presets", async (req, res, params) => {
  const user = requireAuth(req);
  const device = db.getDevice(params.uuid!);
  if (!device) throw new HttpError(404, "Device not found");

  const isOwner = device.ownerUserId === user.id;
  const perm = isOwner ? null : db.getPermission(params.uuid!, user.id);
  if (!isOwner && !perm) throw new HttpError(403, "No access to this device");

  const board = boards.getBoard(device.uuid);
  const presets = board?.state?.presets ?? [];

  const result = presets.map((preset) => ({
    preset,
    withinLimits: isOwner ? true : isPresetWithinLimits(perm!, preset),
  }));
  json(res, 200, result);
});

// ── WebSocket for UI ──

interface UiClient {
  socket: WebSocket;
  userId: string;
  authenticated: boolean;
}

const uiClients = new Set<UiClient>();

function makeDeviceSnapshot(deviceUuid: string, userId: string): DeviceSnapshot | null {
  const device = db.getDevice(deviceUuid);
  if (!device) return null;

  const isOwner = device.ownerUserId === userId;
  const perm = isOwner ? null : db.getPermission(deviceUuid, userId);
  if (!isOwner && !perm) return null;

  const board = boards.getBoard(deviceUuid);
  return {
    uuid: device.uuid,
    nickname: device.nickname,
    isOwner,
    connected: boards.isBoardConnected(deviceUuid),
    rttMs: board?.rttMs ?? null,
    state: board?.state ?? null,
    eventLogEvents: board?.eventLogEvents ?? [],
    permissions: perm ?? null,
  };
}

function getAccessibleDeviceUuids(userId: string): string[] {
  const owned = db.listDevicesByOwner(userId).map((d) => d.uuid);
  const shared = db.listPermissionsForUser(userId).map((p) => p.deviceUuid);
  return [...new Set([...owned, ...shared])];
}

function sendDevicesListToClient(client: UiClient): void {
  const uuids = getAccessibleDeviceUuids(client.userId);
  const devices: DeviceSnapshot[] = [];
  for (const uuid of uuids) {
    const snap = makeDeviceSnapshot(uuid, client.userId);
    if (snap) devices.push(snap);
  }
  sendWs(client.socket, { type: "devices_list", devices });
}

boards.onBoardChange((deviceId) => {
  for (const client of uiClients) {
    if (!client.authenticated) continue;
    const snap = makeDeviceSnapshot(deviceId, client.userId);
    if (snap) {
      sendWs(client.socket, { type: "device_update", device: snap });
    }
  }
});

boards.onPresetPreview((_deviceId, message) => {
  for (const client of uiClients) {
    if (!client.authenticated) continue;
    sendWs(client.socket, message as PresetPreviewMessage);
  }
});

function notifyPermissionChange(deviceUuid: string, granteeUserId: string): void {
  for (const client of uiClients) {
    if (!client.authenticated) continue;
    if (client.userId !== granteeUserId) continue;
    // Re-send the full device snapshot so the invitee sees updated permissions
    const snap = makeDeviceSnapshot(deviceUuid, client.userId);
    if (snap) {
      sendWs(client.socket, { type: "device_update", device: snap });
    } else {
      // Permissions revoked — device no longer accessible
      sendWs(client.socket, { type: "device_disconnected", deviceUuid });
    }
  }
}

function handleUiConnection(socket: WebSocket, request: http.IncomingMessage): void {
  const client: UiClient = { socket, userId: "", authenticated: false };
  uiClients.add(client);

  // Try cookie-based auth on connect
  const cookieToken = auth.extractSessionToken(request.headers.cookie, undefined);
  if (cookieToken) {
    const session = auth.validateSession(cookieToken);
    if (session) {
      const user = db.getUserById(session.userId);
      if (user) {
        client.userId = user.id;
        client.authenticated = true;
        sendWs(client.socket, { type: "authenticated", userId: user.id, login: user.login });
        sendDevicesListToClient(client);
      }
    }
  }

  socket.on("message", (data, isBinary) => {
    if (isBinary) {
      sendWs(socket, { type: "error", message: "Binary messages not supported" });
      return;
    }

    try {
      const message = JSON.parse(data.toString()) as WsClientMessage;
      handleUiMessage(client, message);
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      sendWs(socket, { type: "error", message: msg });
    }
  });

  socket.on("close", () => {
    uiClients.delete(client);
  });

  socket.on("error", () => {
    uiClients.delete(client);
  });
}

function handleUiMessage(client: UiClient, message: WsClientMessage): void {
  switch (message.type) {
    case "authenticate": {
      const session = auth.validateSession(message.token);
      if (!session) {
        sendWs(client.socket, { type: "error", message: "Invalid session" });
        return;
      }
      const user = db.getUserById(session.userId);
      if (!user) {
        sendWs(client.socket, { type: "error", message: "User not found" });
        return;
      }
      client.userId = user.id;
      client.authenticated = true;
      sendWs(client.socket, { type: "authenticated", userId: user.id, login: user.login });
      sendDevicesListToClient(client);
      return;
    }
    case "device_command": {
      if (!client.authenticated) {
        sendWs(client.socket, { type: "error", message: "Not authenticated" });
        return;
      }
      handleDeviceCommand(client, message.deviceUuid, message.command);
      return;
    }
  }
}

function handleDeviceCommand(client: UiClient, deviceUuid: string, command: BoardCommand): void {
  const device = db.getDevice(deviceUuid);
  if (!device) {
    sendWs(client.socket, { type: "error", message: `Device ${deviceUuid} not found` });
    return;
  }

  const isOwner = device.ownerUserId === client.userId;

  // Permission check for non-owners
  if (!isOwner) {
    const perm = db.getPermission(deviceUuid, client.userId);
    if (!perm) {
      sendWs(client.socket, { type: "error", message: "No access to this device" });
      return;
    }

    const error = checkBoardCommandPermission(perm, command);
    if (error) {
      sendWs(client.socket, { type: "error", message: error });
      return;
    }
  }

  try {
    boards.sendCommandToDevice(deviceUuid, command);
    sendWs(client.socket, { type: "info", message: `Sent ${command.type} to ${deviceUuid}` });
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error);
    sendWs(client.socket, { type: "error", message: msg });
  }
}

function checkBoardCommandPermission(perm: DevicePermission, command: BoardCommand): string | null {
  switch (command.type) {
    case "run_action":
      return checkCommandPermission(perm, {
        collarName: command.collar_name,
        mode: command.mode,
        intensity: command.intensity_max ?? command.intensity,
        durationMs: command.duration_max_ms ?? command.duration_ms,
      });
    case "start_action":
      return checkCommandPermission(perm, {
        collarName: command.collar_name,
        mode: command.mode,
        intensity: command.intensity_max ?? command.intensity,
        durationMs: 30_000, // max reasonable duration for held actions
      });
    case "stop_action":
    case "stop_preset":
    case "stop_all":
      return null; // always allow stopping
    case "run_preset": {
      const board = boards.getBoard(perm.deviceUuid);
      const preset = board?.state?.presets.find((p) => p.name === command.name);
      if (!preset) return null; // board will error if preset doesn't exist
      return checkPresetPermission(perm, preset);
    }
    case "save_preset":
      return checkPresetPermission(perm, command.preset);
    case "delete_preset":
      return "Only the device owner can delete device presets";
    case "reorder_presets":
      return "Only the device owner can reorder device presets";
    case "preview_preset":
      return checkPresetPermission(perm, command.preset);
    case "ping":
    case "pong":
      return null;
  }
}

function sendWs(socket: WebSocket, message: WsServerMessage): void {
  if (socket.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify(message));
}

// ── HTTP Server ──

const httpServer = http.createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  const method = req.method ?? "GET";

  // CORS for development
  res.setHeader("Access-Control-Allow-Origin", req.headers.origin ?? "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type, Authorization");
  res.setHeader("Access-Control-Allow-Credentials", "true");

  if (method === "OPTIONS") {
    res.writeHead(204).end();
    return;
  }

  // API routing
  if (url.pathname.startsWith("/api/")) {
    for (const r of routes) {
      if (r.method !== method) continue;
      const match = url.pathname.match(r.pattern);
      if (!match) continue;

      const params: Record<string, string> = {};
      r.paramNames.forEach((name, i) => {
        params[name] = decodeURIComponent(match[i + 1]!);
      });

      try {
        await r.handler(req, res, params);
      } catch (error) {
        if (error instanceof HttpError) {
          json(res, error.status, { error: error.message });
        } else {
          console.error("API error:", error);
          json(res, 500, { error: "Internal server error" });
        }
      }
      return;
    }

    json(res, 404, { error: "Not found" });
    return;
  }

  // Static file serving (production)
  try {
    let filePath = url.pathname === "/" ? "/index.html" : url.pathname;
    const safePath = path.normalize(filePath).replace(/^(\.\.(\/|\\|$))+/, "");
    const fullPath = path.resolve(CLIENT_DIST, `.${safePath}`);

    if (!fullPath.startsWith(CLIENT_DIST)) {
      res.writeHead(403).end("Forbidden");
      return;
    }

    const file = await readFile(fullPath);
    res.writeHead(200, { "Content-Type": contentTypeFor(fullPath) }).end(file);
  } catch {
    // SPA fallback — serve index.html for any unmatched route
    try {
      const indexFile = await readFile(path.join(CLIENT_DIST, "index.html"));
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }).end(indexFile);
    } catch {
      res.writeHead(404, { "Content-Type": "text/plain" }).end("Not found. Run `npm run build:client` first.");
    }
  }
});

// ── WebSocket upgrade ──

const boardWss = new WebSocketServer({ noServer: true, perMessageDeflate: false });
const uiWss = new WebSocketServer({ noServer: true, perMessageDeflate: false });

boardWss.on("connection", (socket, request) => {
  const url = new URL(request.url ?? "/device", `http://${request.headers.host ?? "localhost"}`);
  const pathSegments = url.pathname.split("/").filter(Boolean);
  const deviceId = pathSegments.length > 1 ? pathSegments.slice(1).join("/") : null;
  if (!deviceId) {
    socket.close(1008, "Device ID required in path");
    return;
  }

  // Verify this device UUID is registered
  const device = db.getDevice(deviceId);
  if (!device) {
    socket.close(1008, "Unknown device UUID");
    return;
  }

  const peer = `${request.socket.remoteAddress ?? "unknown"}:${request.socket.remotePort ?? 0}`;
  boards.handleBoardConnection(socket, deviceId, peer);
});

uiWss.on("connection", (socket, request) => {
  handleUiConnection(socket, request);
});

httpServer.on("upgrade", (request, socket, head) => {
  const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);
  if (url.pathname.startsWith("/device/") || url.pathname === "/device") {
    boardWss.handleUpgrade(request, socket, head, (ws) => {
      boardWss.emit("connection", ws, request);
    });
    return;
  }
  if (url.pathname === "/ws") {
    uiWss.handleUpgrade(request, socket, head, (ws) => {
      uiWss.emit("connection", ws, request);
    });
    return;
  }
  socket.destroy();
});

// ── Start ──

const port = Number(process.env.PORT ?? process.argv[2] ?? 8099);
httpServer.listen(port, "0.0.0.0", () => {
  console.log(`Central Control server listening on http://0.0.0.0:${port}`);
  console.log(`Devices connect to ws://HOST:${port}/device/{uuid}`);
  console.log(`UI WebSocket at ws://HOST:${port}/ws`);
});

function contentTypeFor(filePath: string): string {
  if (filePath.endsWith(".html")) return "text/html; charset=utf-8";
  if (filePath.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (filePath.endsWith(".css")) return "text/css; charset=utf-8";
  if (filePath.endsWith(".map")) return "application/json; charset=utf-8";
  if (filePath.endsWith(".svg")) return "image/svg+xml";
  if (filePath.endsWith(".png")) return "image/png";
  return "application/octet-stream";
}
