import type {
  ApiDevice,
  ApiInvitation,
  ApiUser,
  CollarPermission,
  DevicePermission,
  LoginResponse,
  TotpSetupResponse,
  UserPreset,
} from "../shared/types.js";
import type { Preset } from "../shared/protocol.js";

class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const opts: RequestInit = {
    method,
    headers: { "Content-Type": "application/json" },
    credentials: "include",
  };
  if (body !== undefined) {
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  const data = await res.json();
  if (!res.ok) {
    throw new ApiError(res.status, data.error ?? "Unknown error");
  }
  return data as T;
}

// Auth
export const signup = (login: string, password: string) =>
  request<LoginResponse>("POST", "/api/auth/signup", { login, password });

export const login = (login: string, password: string) =>
  request<LoginResponse>("POST", "/api/auth/login", { login, password });

export const validateTotp = (pendingToken: string, code: string) =>
  request<{ sessionToken: string; user: ApiUser }>("POST", "/api/auth/totp/validate", { pendingToken, code });

export const setupTotp = () => request<TotpSetupResponse>("POST", "/api/auth/totp/setup");

export const verifyTotp = (code: string) =>
  request<{ success: boolean }>("POST", "/api/auth/totp/verify", { code });

export const logout = () => request<{ success: boolean }>("POST", "/api/auth/logout");

export const getMe = () => request<ApiUser>("GET", "/api/auth/me");

// Devices
export const listDevices = () => request<ApiDevice[]>("GET", "/api/devices");

export const listSharedDevices = () => request<ApiDevice[]>("GET", "/api/devices/shared");

export const addDevice = (nickname: string) =>
  request<{ device: ApiDevice; connectionUrl: string }>("POST", "/api/devices", { nickname });

export const deleteDevice = (uuid: string) =>
  request<{ success: boolean }>("DELETE", `/api/devices/${uuid}`);

// Invitations
export const listInvitations = () => request<ApiInvitation[]>("GET", "/api/invitations");

export const createInvitation = (name: string) =>
  request<{ invitation: ApiInvitation; link: string }>("POST", "/api/invitations", { name });

export const getInvitation = (token: string) =>
  request<{ id: string; fromLogin: string; status: string }>("GET", `/api/invitations/${token}`);

export const acceptInvitation = (token: string) =>
  request<{ success: boolean }>("POST", `/api/invitations/${token}/accept`);

export const rejectInvitation = (token: string) =>
  request<{ success: boolean }>("POST", `/api/invitations/${token}/reject`);

export const cancelInvitation = (token: string) =>
  request<{ success: boolean }>("DELETE", `/api/invitations/${token}`);

// Permissions
export const listDevicePermissions = (deviceUuid: string) =>
  request<(DevicePermission & { granteeLogin: string })[]>("GET", `/api/permissions/${deviceUuid}`);

export const getMyPermission = (deviceUuid: string) =>
  request<DevicePermission | null>("GET", `/api/permissions/${deviceUuid}/my`);

export const setPermission = (deviceUuid: string, userId: string, collars: CollarPermission[]) =>
  request<DevicePermission>("PUT", `/api/permissions/${deviceUuid}/${userId}`, { collars });

export const deletePermission = (deviceUuid: string, userId: string) =>
  request<{ success: boolean }>("DELETE", `/api/permissions/${deviceUuid}/${userId}`);

// Device users
export const listDeviceUsers = (uuid: string) =>
  request<{ userId: string; login: string; hasPermission: boolean; permission: DevicePermission | null }[]>(
    "GET",
    `/api/devices/${uuid}/users`,
  );

// User presets
export const listUserPresets = (deviceUuid: string) =>
  request<UserPreset[]>("GET", `/api/user-presets/${deviceUuid}`);

export const createUserPreset = (deviceUuid: string, preset: Preset) =>
  request<UserPreset>("POST", `/api/user-presets/${deviceUuid}`, { preset });

export const updateUserPreset = (deviceUuid: string, presetId: string, preset: Preset) =>
  request<UserPreset>("PUT", `/api/user-presets/${deviceUuid}/${presetId}`, { preset });

export const deleteUserPreset = (deviceUuid: string, presetId: string) =>
  request<{ success: boolean }>("DELETE", `/api/user-presets/${deviceUuid}/${presetId}`);

// Owner presets
export const listOwnerPresets = (deviceUuid: string) =>
  request<{ preset: Preset; withinLimits: boolean }[]>("GET", `/api/devices/${deviceUuid}/owner-presets`);

export { ApiError };
