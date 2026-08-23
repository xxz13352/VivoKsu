export const LEASE_TTL_SECONDS = 120;
export const PINSET_TTL_SECONDS = 7 * 24 * 60 * 60;
export const API_PINSET_HOST = "api.nwflash.cc.cd";
export const API_PRIMARY_PIN = "kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=";
export const API_BACKUP_PIN = "kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4=";
export const INTEGRITY_MAX_BODY_BYTES = 4_096;
export const INTEGRITY_RATE_WINDOW_SECONDS = 60;
export const INTEGRITY_RATE_LIMIT = 20;

const INTEGRITY_PHASES = new Set([
  "startup",
  "login",
  "session_restore",
  "heartbeat",
  "operation_admission",
  "pin_validation",
]);
const INTEGRITY_REASONS = new Set([
  "image_crc_invalid",
  "lease_signature_invalid",
  "lease_binding_invalid",
  "lease_expired",
  "sequence_rollback",
  "pin_mismatch",
  "debugger_detected",
  "virtual_machine_detected",
  "authenticode_invalid",
  "release_manifest_invalid",
]);
const INTEGRITY_FIELDS = new Set([
  "event_id",
  "phase",
  "reason",
  "client_version",
  "build_id",
  "occurred_at",
]);

export type LeaseKind = "login" | "heartbeat";

export interface LeaseClaims {
  version: 1;
  kind: LeaseKind;
  username: string;
  token_sha256: string;
  client_version: string;
  build_id: string;
  process_nonce: string;
  session_id: string;
  sequence: number;
  issued_at: number;
  expires_at: number;
}

export interface SignedLeaseEnvelope {
  lease_payload: string;
  lease_signature: string;
}

export interface PinsetPayload {
  version: 1;
  host: typeof API_PINSET_HOST;
  not_before: number;
  expires_at: number;
  primary_pin: typeof API_PRIMARY_PIN;
  backup_pin: typeof API_BACKUP_PIN;
}

export interface SignedPinsetEnvelope {
  pinset_payload: string;
  pinset_signature: string;
}

export interface IntegrityReport {
  event_id: string;
  phase: string;
  reason: string;
  client_version: string;
  build_id: string;
  occurred_at: number;
}

export class SigningConfigurationError extends Error {
  constructor() {
    super("SESSION_SIGNING_PRIVATE_KEY_PKCS8 is not configured");
    this.name = "SigningConfigurationError";
  }
}

export class IntegrityBodyTooLargeError extends Error {}
export class InvalidIntegrityReportError extends Error {}

export async function importSigningKey(privateKeyPkcs8: string | undefined): Promise<CryptoKey> {
  if (!privateKeyPkcs8) throw new SigningConfigurationError();
  const der = decodeBase64Url(privateKeyPkcs8);
  try {
    return await crypto.subtle.importKey(
      "pkcs8",
      der.buffer,
      { name: "Ed25519" },
      false,
      ["sign"],
    );
  } catch {
    throw new SigningConfigurationError();
  } finally {
    der.fill(0);
  }
}

export async function signLease(
  claims: LeaseClaims,
  privateKeyPkcs8: string | undefined,
): Promise<SignedLeaseEnvelope> {
  const signed = await signJson(claims, privateKeyPkcs8);
  return { lease_payload: signed.payload, lease_signature: signed.signature };
}

export async function signPinset(
  payload: PinsetPayload,
  privateKeyPkcs8: string | undefined,
): Promise<SignedPinsetEnvelope> {
  const signed = await signJson(payload, privateKeyPkcs8);
  return { pinset_payload: signed.payload, pinset_signature: signed.signature };
}

export async function tokenSha256(token: string): Promise<string> {
  const encoded = new TextEncoder().encode(token);
  try {
    return encodeBase64Url(await crypto.subtle.digest("SHA-256", encoded));
  } finally {
    encoded.fill(0);
  }
}

export async function integrityIpHash(ip: string): Promise<string> {
  const encoded = new TextEncoder().encode(ip || "unknown");
  try {
    return encodeBase64Url(await crypto.subtle.digest("SHA-256", encoded));
  } finally {
    encoded.fill(0);
  }
}

export async function readIntegrityReport(request: Request): Promise<IntegrityReport> {
  const contentType = request.headers.get("Content-Type")?.split(";", 1)[0].trim().toLowerCase();
  if (contentType !== "application/json") throw new InvalidIntegrityReportError();

  const contentLength = request.headers.get("Content-Length");
  if (contentLength !== null) {
    const declared = Number(contentLength);
    if (!Number.isSafeInteger(declared) || declared < 0) throw new InvalidIntegrityReportError();
    if (declared > INTEGRITY_MAX_BODY_BYTES) throw new IntegrityBodyTooLargeError();
  }

  if (!request.body) throw new InvalidIntegrityReportError();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > INTEGRITY_MAX_BODY_BYTES) {
      await reader.cancel();
      throw new IntegrityBodyTooLargeError();
    }
    chunks.push(value);
  }

  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new InvalidIntegrityReportError();
  } finally {
    bytes.fill(0);
  }
  return validateIntegrityReport(parsed);
}

export function createPinset(now: number): PinsetPayload {
  return {
    version: 1,
    host: API_PINSET_HOST,
    not_before: now - 60,
    expires_at: now + PINSET_TTL_SECONDS,
    primary_pin: API_PRIMARY_PIN,
    backup_pin: API_BACKUP_PIN,
  };
}

function validateIntegrityReport(value: unknown): IntegrityReport {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new InvalidIntegrityReportError();
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== INTEGRITY_FIELDS.size || keys.some((key) => !INTEGRITY_FIELDS.has(key))) {
    throw new InvalidIntegrityReportError();
  }
  if (typeof record.event_id !== "string" || !/^[A-Za-z0-9._:-]{1,64}$/.test(record.event_id)) {
    throw new InvalidIntegrityReportError();
  }
  if (typeof record.phase !== "string" || !INTEGRITY_PHASES.has(record.phase)) {
    throw new InvalidIntegrityReportError();
  }
  if (typeof record.reason !== "string" || !INTEGRITY_REASONS.has(record.reason)) {
    throw new InvalidIntegrityReportError();
  }
  if (typeof record.client_version !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._+-]{0,31}$/.test(record.client_version)) {
    throw new InvalidIntegrityReportError();
  }
  if (typeof record.build_id !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/.test(record.build_id)) {
    throw new InvalidIntegrityReportError();
  }
  if (!Number.isSafeInteger(record.occurred_at) || Number(record.occurred_at) <= 0) {
    throw new InvalidIntegrityReportError();
  }
  return record as unknown as IntegrityReport;
}

async function signJson(
  value: unknown,
  privateKeyPkcs8: string | undefined,
): Promise<{ payload: string; signature: string }> {
  const key = await importSigningKey(privateKeyPkcs8);
  const jsonBytes = new TextEncoder().encode(JSON.stringify(value));
  const payload = encodeBase64Url(jsonBytes);
  const payloadBytes = new TextEncoder().encode(payload);
  try {
    const signature = await crypto.subtle.sign({ name: "Ed25519" }, key, payloadBytes);
    return { payload, signature: encodeBase64Url(signature) };
  } finally {
    jsonBytes.fill(0);
    payloadBytes.fill(0);
  }
}

function encodeBase64Url(value: ArrayBuffer | Uint8Array): string {
  const bytes = value instanceof Uint8Array ? value : new Uint8Array(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]+$/.test(value) || value.length % 4 === 1) {
    throw new SigningConfigurationError();
  }
  const padded = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  try {
    const binary = atob(padded);
    const decoded = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) decoded[index] = binary.charCodeAt(index);
    return decoded;
  } catch {
    throw new SigningConfigurationError();
  }
}
