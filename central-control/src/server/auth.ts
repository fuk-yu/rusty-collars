import { randomBytes } from "node:crypto";
import bcrypt from "bcryptjs";
import { TOTP, Secret } from "otpauth";
import QRCode from "qrcode";
import type { Session, TotpSetupResponse } from "../shared/types.js";
import * as db from "./db.js";

const SESSION_DURATION_MS = 7 * 24 * 60 * 60 * 1000; // 7 days
const BCRYPT_ROUNDS = 12;
const TOTP_ISSUER = "Rusty Collars";

export async function hashPassword(password: string): Promise<string> {
  return bcrypt.hash(password, BCRYPT_ROUNDS);
}

export async function verifyPassword(password: string, hash: string): Promise<boolean> {
  return bcrypt.compare(password, hash);
}

export function generateId(): string {
  return randomBytes(16).toString("hex");
}

export function generateToken(): string {
  return randomBytes(32).toString("hex");
}

export function createSession(userId: string): Session {
  const session: Session = {
    token: generateToken(),
    userId,
    createdAt: Date.now(),
    expiresAt: Date.now() + SESSION_DURATION_MS,
  };
  db.putSession(session);
  return session;
}

export function validateSession(token: string): Session | undefined {
  return db.getSession(token);
}

export function destroySession(token: string): void {
  db.deleteSession(token);
}

export async function generateTotpSetup(login: string): Promise<TotpSetupResponse> {
  const secret = new Secret({ size: 20 });
  const totp = new TOTP({
    issuer: TOTP_ISSUER,
    label: login,
    algorithm: "SHA1",
    digits: 6,
    period: 30,
    secret,
  });
  const uri = totp.toString();
  const qrDataUrl = await QRCode.toDataURL(uri);
  return {
    secret: secret.base32,
    uri,
    qrDataUrl,
  };
}

export function verifyTotpCode(secretBase32: string, code: string): boolean {
  const totp = new TOTP({
    issuer: TOTP_ISSUER,
    algorithm: "SHA1",
    digits: 6,
    period: 30,
    secret: Secret.fromBase32(secretBase32),
  });
  const delta = totp.validate({ token: code, window: 1 });
  return delta !== null;
}

export function extractSessionToken(cookieHeader: string | undefined, authHeader: string | undefined): string | null {
  if (authHeader?.startsWith("Bearer ")) {
    return authHeader.slice(7);
  }
  if (cookieHeader) {
    const match = cookieHeader.match(/(?:^|;\s*)session=([^;]+)/);
    if (match?.[1]) return match[1];
  }
  return null;
}
