import { open, type Database, type RootDatabase } from "lmdb";
import type { Device, DevicePermission, Invitation, Session, User, UserPreset } from "../shared/types.js";

let root: RootDatabase;
let users: Database<User, string>;
let usersById: Database<string, string>;
let sessions: Database<Session, string>;
let devices: Database<Device, string>;
let invitations: Database<Invitation, string>;
let permissions: Database<DevicePermission, string>;
let userPresets: Database<UserPreset, string>;

export function initDb(dataPath: string): void {
  root = open({ path: dataPath, compression: true });
  users = root.openDB("users", {});
  usersById = root.openDB("users-by-id", {});
  sessions = root.openDB("sessions", {});
  devices = root.openDB("devices", {});
  invitations = root.openDB("invitations", {});
  permissions = root.openDB("permissions", {});
  userPresets = root.openDB("user-presets", {});
}

// ── Users ──

export function getUserByLogin(login: string): User | undefined {
  return users.get(login);
}

export function getUserById(userId: string): User | undefined {
  const login = usersById.get(userId);
  if (!login) return undefined;
  return users.get(login);
}

export function putUser(user: User): void {
  users.putSync(user.login, user);
  usersById.putSync(user.id, user.login);
}

// ── Sessions ──

export function getSession(token: string): Session | undefined {
  const session = sessions.get(token);
  if (!session) return undefined;
  if (session.expiresAt < Date.now()) {
    sessions.removeSync(token);
    return undefined;
  }
  return session;
}

export function putSession(session: Session): void {
  sessions.putSync(session.token, session);
}

export function deleteSession(token: string): void {
  sessions.removeSync(token);
}

// ── Devices ──

export function getDevice(uuid: string): Device | undefined {
  return devices.get(uuid);
}

export function putDevice(device: Device): void {
  devices.putSync(device.uuid, device);
}

export function deleteDevice(uuid: string): void {
  devices.removeSync(uuid);
}

export function listDevicesByOwner(ownerUserId: string): Device[] {
  const result: Device[] = [];
  for (const { value } of devices.getRange({})) {
    if (value.ownerUserId === ownerUserId) {
      result.push(value);
    }
  }
  return result;
}

export function listDevicesAccessibleBy(userId: string): Device[] {
  const owned = listDevicesByOwner(userId);
  const shared: Device[] = [];
  for (const { value } of permissions.getRange({})) {
    if (value.granteeUserId === userId) {
      const device = devices.get(value.deviceUuid);
      if (device) shared.push(device);
    }
  }
  return [...owned, ...shared];
}

// ── Invitations ──

export function getInvitation(token: string): Invitation | undefined {
  return invitations.get(token);
}

export function getInvitationById(id: string): Invitation | undefined {
  for (const { value } of invitations.getRange({})) {
    if (value.id === id) return value;
  }
  return undefined;
}

export function putInvitation(invitation: Invitation): void {
  invitations.putSync(invitation.token, invitation);
}

export function deleteInvitation(token: string): void {
  invitations.removeSync(token);
}

export function listInvitationsByUser(userId: string): Invitation[] {
  const result: Invitation[] = [];
  for (const { value } of invitations.getRange({})) {
    if (value.fromUserId === userId || value.toUserId === userId) {
      result.push(value);
    }
  }
  return result;
}

export function listAcceptedInviteeIds(fromUserId: string): string[] {
  const result: string[] = [];
  for (const { value } of invitations.getRange({})) {
    if (value.fromUserId === fromUserId && value.status === "accepted" && value.toUserId) {
      result.push(value.toUserId);
    }
  }
  return [...new Set(result)];
}

// ── Permissions ──

function permKey(deviceUuid: string, granteeUserId: string): string {
  return `${deviceUuid}:${granteeUserId}`;
}

export function getPermission(deviceUuid: string, granteeUserId: string): DevicePermission | undefined {
  return permissions.get(permKey(deviceUuid, granteeUserId));
}

export function putPermission(perm: DevicePermission): void {
  permissions.putSync(permKey(perm.deviceUuid, perm.granteeUserId), perm);
}

export function deletePermission(deviceUuid: string, granteeUserId: string): void {
  permissions.removeSync(permKey(deviceUuid, granteeUserId));
}

export function listPermissionsForDevice(deviceUuid: string): DevicePermission[] {
  const result: DevicePermission[] = [];
  for (const { value } of permissions.getRange({})) {
    if (value.deviceUuid === deviceUuid) {
      result.push(value);
    }
  }
  return result;
}

export function listPermissionsForUser(userId: string): DevicePermission[] {
  const result: DevicePermission[] = [];
  for (const { value } of permissions.getRange({})) {
    if (value.granteeUserId === userId) {
      result.push(value);
    }
  }
  return result;
}

// ── User Presets ──

function presetKey(userId: string, deviceUuid: string, presetId: string): string {
  return `${userId}:${deviceUuid}:${presetId}`;
}

export function getUserPreset(userId: string, deviceUuid: string, presetId: string): UserPreset | undefined {
  return userPresets.get(presetKey(userId, deviceUuid, presetId));
}

export function putUserPreset(preset: UserPreset): void {
  userPresets.putSync(presetKey(preset.userId, preset.deviceUuid, preset.id), preset);
}

export function deleteUserPreset(userId: string, deviceUuid: string, presetId: string): void {
  userPresets.removeSync(presetKey(userId, deviceUuid, presetId));
}

export function listUserPresets(userId: string, deviceUuid: string): UserPreset[] {
  const result: UserPreset[] = [];
  const prefix = `${userId}:${deviceUuid}:`;
  for (const { key, value } of userPresets.getRange({})) {
    if (typeof key === "string" && key.startsWith(prefix)) {
      result.push(value);
    }
  }
  return result;
}
