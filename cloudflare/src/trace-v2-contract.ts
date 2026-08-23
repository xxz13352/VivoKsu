export const TRACE_SCHEMA_VERSION = 2 as const;
export const TRACE_UPLOAD_MAX_BODY_BYTES = 1_048_576;
export const TRACE_UPLOAD_MAX_RUNS = 20;
export const TRACE_UPLOAD_MAX_EVENTS = 100;
export const TRACE_UPLOAD_MAX_OUTPUT_CHUNKS = 200;
export const TRACE_OUTPUT_MAX_BYTES = 32_768;

const MAX_TEXT_BYTES = 16_384;
const MAX_SHORT_TEXT_BYTES = 1_024;
const MAX_TEXT_ITEMS = 100;
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const SHA256 = /^[0-9a-f]{64}$/;

export type TraceOutcomeV2 = "running" | "success" | "failed" | "canceled" | "denied" | "aborted" | "unknown";
export type TraceEventKindV2 = "authorization" | "stage" | "partition" | "command" | "skip" | "verification" | "terminal";
export type TraceEventStatusV2 = "started" | "success" | "failed" | "canceled" | "skipped" | "unknown";
export type TraceOutputStreamV2 = "stdout" | "stderr";
export type TraceRejectedCodeV2 = "invalid" | "missing_parent" | "sequence_conflict" | "incomplete_trace" | "credential_rejected";
export type TraceApiErrorCodeV2 = "TRACE_BODY_TOO_LARGE" | "TRACE_INVALID" | "TRACE_UNAUTHORIZED" | "TRACE_FORBIDDEN" | "TRACE_OWNERSHIP_CONFLICT" | "TRACE_INCOMPLETE" | "TRACE_INTERNAL";

export interface CredentialRedactionCountV2 { kind: string; count: number; }
export interface TraceCommandV2 {
  program: string;
  argv: string[];
  display_command: string;
  working_directory: string | null;
  paths: string[];
  urls: string[];
  serial: string | null;
}
export interface TraceRunV2 {
  run_id: string;
  operation_kind: string;
  title: string;
  outcome: TraceOutcomeV2;
  device_serial: string | null;
  source_paths: string[];
  source_urls: string[];
  client_version: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  error_class: string | null;
  error_code: string | null;
  error_message: string | null;
  final_sequence: number | null;
  trace_complete: boolean;
  trace_loss_reason: string | null;
}
export interface TraceEventV2 {
  event_id: string;
  run_id: string;
  sequence: number;
  kind: TraceEventKindV2;
  step_name: string;
  partition_name: string | null;
  status: TraceEventStatusV2;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  command: TraceCommandV2 | null;
  exit_code: number | null;
  stdout_chunks: number;
  stderr_chunks: number;
  verification: string | null;
  device_state: string | null;
  retry_safe: boolean | null;
  remedies: string[];
  error_class: string | null;
  error_code: string | null;
  error_message: string | null;
  credential_redactions: CredentialRedactionCountV2[];
}
export interface TraceOutputChunkV2 {
  chunk_id: string;
  event_id: string;
  stream: TraceOutputStreamV2;
  chunk_index: number;
  text: string;
  byte_count: number;
  sha256: string;
}
export interface TraceUploadRequestV2 { schema_version: 2; upload_id: string; runs: TraceRunV2[]; events: TraceEventV2[]; output_chunks: TraceOutputChunkV2[]; }
export interface TraceRejectedItemV2 { entity: "run" | "event" | "output_chunk"; id: string | null; code: TraceRejectedCodeV2; message: string; }
export interface TraceUploadResponseV2 { ok: true; accepted: { runs: string[]; events: string[]; output_chunks: string[] }; rejected: TraceRejectedItemV2[]; }
export interface TraceApiErrorV2 { ok: false; error: { code: TraceApiErrorCodeV2; message: string; request_id: string; details?: TraceRejectedItemV2[] }; }
export interface KeysetPageV2<T> { items: T[]; next_cursor: string | null; }
export interface TraceCursorV2 { v: 1; started_at_ms: number; run_id: string; }
export interface TraceUserSummaryV2 { user_id: number; username: string; name: string; operation_count: number; failed_count: number; last_operation: TraceRunSummaryV2 | null; last_activity_at_ms: number | null; }
export interface TraceRunSummaryV2 { source_schema: 1 | 2; trace_ref: string; run_id: string | null; legacy_id: number | null; user_id: number | null; username: string | null; user_name: string | null; operation_kind: string; title: string; outcome: TraceOutcomeV2; client_version: string; started_at_ms: number; ended_at_ms: number | null; duration_ms: number | null; trace_complete: boolean; trace_loss_reason: string | null; }
export interface TraceRunDetailV2 { source_schema: 1 | 2; detail_available: boolean; detail_unavailable_reason: "legacy_client_no_step_data" | null; run: TraceRunSummaryV2; events: TraceEventV2[]; }
export interface TraceEventDetailV2 { run: TraceRunSummaryV2; event: TraceEventV2; }
export interface TraceOutputPageV2 { run_id: string; event_id: string; stream: TraceOutputStreamV2; chunks: TraceOutputChunkV2[]; next_after_chunk: number | null; output_complete: boolean; }
export interface TraceOverviewV2 { totals: { api_users: number; online_sessions: number; operations: number; failed: number }; trend: Array<{ bucket_start_ms: number; operations: number; failed: number }>; recent_failures: TraceRunSummaryV2[]; }
export interface RomLogAdminRowV2 { id: number; user_id: number | null; user_name: string | null; pd: string; version: string; status: number; url: string | null; failure_reason: string | null; detail_unavailable_reason: "legacy_record_no_failure_reason" | null; created_at_ms: number; }

export class TraceUploadTooLargeError extends Error {}
export class TraceValidationError extends Error {}

export async function readTraceUploadV2(request: Request): Promise<TraceUploadRequestV2> {
  const contentType = request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (contentType !== "application/json") throw invalid("content-type must be application/json");
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const size = Number(declared);
    if (!Number.isSafeInteger(size) || size < 0) throw invalid("content-length must be a non-negative safe integer");
    if (size > TRACE_UPLOAD_MAX_BODY_BYTES) throw new TraceUploadTooLargeError("trace upload exceeds 1 MiB");
  }
  if (!request.body) throw invalid("request body is required");
  const reader = request.body.getReader();
  const parts: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > TRACE_UPLOAD_MAX_BODY_BYTES) {
      await reader.cancel();
      throw new TraceUploadTooLargeError("trace upload exceeds 1 MiB");
    }
    parts.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) { bytes.set(part, offset); offset += part.byteLength; }
  try {
    return validateTraceUploadV2(JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)));
  } catch (error) {
    if (error instanceof TraceValidationError) throw error;
    throw invalid("malformed JSON");
  } finally {
    bytes.fill(0);
  }
}

export function validateTraceUploadV2(value: unknown): TraceUploadRequestV2 {
  const root = strictObject(value, ["schema_version", "upload_id", "runs", "events", "output_chunks"], "upload");
  requireLiteral(root.schema_version, TRACE_SCHEMA_VERSION, "schema_version");
  const upload_id = requireUuidV7(root.upload_id, "upload_id");
  const runs = requireArray(root.runs, TRACE_UPLOAD_MAX_RUNS, parseRunV2, "runs");
  const events = requireArray(root.events, TRACE_UPLOAD_MAX_EVENTS, parseEventV2, "events");
  const output_chunks = requireArray(root.output_chunks, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS, parseChunkV2, "output_chunks");
  validateParentRelationships(runs, events, output_chunks);
  return { schema_version: 2, upload_id, runs, events, output_chunks };
}

export function encodeTraceCursorV2(cursor: TraceCursorV2): string {
  const checked = decodeTraceCursorV2(bytesToBase64Url(new TextEncoder().encode(JSON.stringify(cursor))));
  return bytesToBase64Url(new TextEncoder().encode(JSON.stringify(checked)));
}

export function decodeTraceCursorV2(encoded: string): TraceCursorV2 {
  if (typeof encoded !== "string" || encoded.length === 0) throw invalid("cursor must be base64url text");
  let parsed: unknown;
  try { parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(base64UrlToBytes(encoded))); }
  catch { throw invalid("cursor is not valid base64url JSON"); }
  const root = strictObject(parsed, ["v", "started_at_ms", "run_id"], "cursor");
  return {
    v: requireLiteral(root.v, 1, "cursor.v"),
    started_at_ms: requireSafeInteger(root.started_at_ms, 0, Number.MAX_SAFE_INTEGER, "cursor.started_at_ms"),
    run_id: requireUuidV7(root.run_id, "cursor.run_id"),
  };
}

function parseRunV2(value: unknown, field: string): TraceRunV2 {
  const root = strictObject(value, ["run_id", "operation_kind", "title", "outcome", "device_serial", "source_paths", "source_urls", "client_version", "started_at_ms", "ended_at_ms", "duration_ms", "error_class", "error_code", "error_message", "final_sequence", "trace_complete", "trace_loss_reason"], field);
  const started_at_ms = requireSafeInteger(root.started_at_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.started_at_ms`);
  const ended_at_ms = requireNullableInteger(root.ended_at_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.ended_at_ms`);
  const duration_ms = requireNullableInteger(root.duration_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.duration_ms`);
  requireTimestampPair(started_at_ms, ended_at_ms, duration_ms, field);
  const trace_complete = requireBoolean(root.trace_complete, `${field}.trace_complete`);
  const trace_loss_reason = requireNullableText(root.trace_loss_reason, MAX_TEXT_BYTES, `${field}.trace_loss_reason`);
  if (trace_complete && trace_loss_reason !== null) throw invalid(`${field}.trace_loss_reason must be null for complete traces`);
  return {
    run_id: requireUuidV7(root.run_id, `${field}.run_id`), operation_kind: requireText(root.operation_kind, MAX_SHORT_TEXT_BYTES, `${field}.operation_kind`), title: requireText(root.title, MAX_TEXT_BYTES, `${field}.title`), outcome: requireEnum(root.outcome, ["running", "success", "failed", "canceled", "denied", "aborted", "unknown"], `${field}.outcome`), device_serial: requireNullableText(root.device_serial, MAX_SHORT_TEXT_BYTES, `${field}.device_serial`), source_paths: requireStringArray(root.source_paths, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.source_paths`), source_urls: requireStringArray(root.source_urls, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.source_urls`), client_version: requireText(root.client_version, MAX_SHORT_TEXT_BYTES, `${field}.client_version`), started_at_ms, ended_at_ms, duration_ms, error_class: requireNullableText(root.error_class, MAX_SHORT_TEXT_BYTES, `${field}.error_class`), error_code: requireNullableText(root.error_code, MAX_SHORT_TEXT_BYTES, `${field}.error_code`), error_message: requireNullableText(root.error_message, MAX_TEXT_BYTES, `${field}.error_message`), final_sequence: requireNullableInteger(root.final_sequence, 0, Number.MAX_SAFE_INTEGER, `${field}.final_sequence`), trace_complete, trace_loss_reason,
  };
}

function parseEventV2(value: unknown, field: string): TraceEventV2 {
  const root = strictObject(value, ["event_id", "run_id", "sequence", "kind", "step_name", "partition_name", "status", "started_at_ms", "ended_at_ms", "duration_ms", "command", "exit_code", "stdout_chunks", "stderr_chunks", "verification", "device_state", "retry_safe", "remedies", "error_class", "error_code", "error_message", "credential_redactions"], field);
  const started_at_ms = requireSafeInteger(root.started_at_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.started_at_ms`);
  const ended_at_ms = requireNullableInteger(root.ended_at_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.ended_at_ms`);
  const duration_ms = requireNullableInteger(root.duration_ms, 0, Number.MAX_SAFE_INTEGER, `${field}.duration_ms`);
  requireTimestampPair(started_at_ms, ended_at_ms, duration_ms, field);
  return {
    event_id: requireUuidV7(root.event_id, `${field}.event_id`), run_id: requireUuidV7(root.run_id, `${field}.run_id`), sequence: requireSafeInteger(root.sequence, 0, Number.MAX_SAFE_INTEGER, `${field}.sequence`), kind: requireEnum(root.kind, ["authorization", "stage", "partition", "command", "skip", "verification", "terminal"], `${field}.kind`), step_name: requireText(root.step_name, MAX_SHORT_TEXT_BYTES, `${field}.step_name`), partition_name: requireNullableText(root.partition_name, MAX_SHORT_TEXT_BYTES, `${field}.partition_name`), status: requireEnum(root.status, ["started", "success", "failed", "canceled", "skipped", "unknown"], `${field}.status`), started_at_ms, ended_at_ms, duration_ms, command: root.command === null ? null : parseCommandV2(root.command, `${field}.command`), exit_code: requireNullableInteger(root.exit_code, -2_147_483_648, 2_147_483_647, `${field}.exit_code`), stdout_chunks: requireSafeInteger(root.stdout_chunks, 0, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS, `${field}.stdout_chunks`), stderr_chunks: requireSafeInteger(root.stderr_chunks, 0, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS, `${field}.stderr_chunks`), verification: requireNullableText(root.verification, MAX_TEXT_BYTES, `${field}.verification`), device_state: requireNullableText(root.device_state, MAX_SHORT_TEXT_BYTES, `${field}.device_state`), retry_safe: requireNullableBoolean(root.retry_safe, `${field}.retry_safe`), remedies: requireStringArray(root.remedies, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.remedies`), error_class: requireNullableText(root.error_class, MAX_SHORT_TEXT_BYTES, `${field}.error_class`), error_code: requireNullableText(root.error_code, MAX_SHORT_TEXT_BYTES, `${field}.error_code`), error_message: requireNullableText(root.error_message, MAX_TEXT_BYTES, `${field}.error_message`), credential_redactions: requireArray(root.credential_redactions, MAX_TEXT_ITEMS, parseRedaction, `${field}.credential_redactions`),
  };
}

function parseCommandV2(value: unknown, field: string): TraceCommandV2 {
  const root = strictObject(value, ["program", "argv", "display_command", "working_directory", "paths", "urls", "serial"], field);
  return { program: requireText(root.program, MAX_TEXT_BYTES, `${field}.program`), argv: requireStringArray(root.argv, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.argv`), display_command: requireText(root.display_command, MAX_TEXT_BYTES, `${field}.display_command`), working_directory: requireNullableText(root.working_directory, MAX_TEXT_BYTES, `${field}.working_directory`), paths: requireStringArray(root.paths, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.paths`), urls: requireStringArray(root.urls, MAX_TEXT_ITEMS, MAX_TEXT_BYTES, `${field}.urls`), serial: requireNullableText(root.serial, MAX_SHORT_TEXT_BYTES, `${field}.serial`) };
}

function parseRedaction(value: unknown, field: string): CredentialRedactionCountV2 {
  const root = strictObject(value, ["kind", "count"], field);
  return { kind: requireText(root.kind, MAX_SHORT_TEXT_BYTES, `${field}.kind`), count: requireSafeInteger(root.count, 1, Number.MAX_SAFE_INTEGER, `${field}.count`) };
}

function parseChunkV2(value: unknown, field: string): TraceOutputChunkV2 {
  const root = strictObject(value, ["chunk_id", "event_id", "stream", "chunk_index", "text", "byte_count", "sha256"], field);
  const text = requireText(root.text, TRACE_OUTPUT_MAX_BYTES, `${field}.text`);
  const byte_count = requireSafeInteger(root.byte_count, 0, TRACE_OUTPUT_MAX_BYTES, `${field}.byte_count`);
  const actualBytes = new TextEncoder().encode(text);
  if (actualBytes.byteLength !== byte_count) throw invalid(`${field}.byte_count does not match UTF-8 text`);
  const sha256 = requireText(root.sha256, 64, `${field}.sha256`);
  if (!SHA256.test(sha256)) throw invalid(`${field}.sha256 must be lowercase SHA-256 hex`);
  if (sha256Hex(actualBytes) !== sha256) throw invalid(`${field}.sha256 does not match UTF-8 text`);
  return { chunk_id: requireUuidV7(root.chunk_id, `${field}.chunk_id`), event_id: requireUuidV7(root.event_id, `${field}.event_id`), stream: requireEnum(root.stream, ["stdout", "stderr"], `${field}.stream`), chunk_index: requireSafeInteger(root.chunk_index, 0, Number.MAX_SAFE_INTEGER, `${field}.chunk_index`), text, byte_count, sha256 };
}

function validateParentRelationships(runs: TraceRunV2[], events: TraceEventV2[], chunks: TraceOutputChunkV2[]): void {
  const runIds = new Set<string>();
  for (const run of runs) { if (runIds.has(run.run_id)) throw invalid("duplicate run_id"); runIds.add(run.run_id); }
  const eventIds = new Set<string>();
  const sequences = new Set<string>();
  for (const event of events) {
    if (!runIds.has(event.run_id)) throw invalid(`event ${event.event_id} has unknown run_id`);
    if (eventIds.has(event.event_id)) throw invalid("duplicate event_id");
    eventIds.add(event.event_id);
    const key = `${event.run_id}\u0000${event.sequence}`;
    if (sequences.has(key)) throw invalid("duplicate (run_id, sequence)");
    sequences.add(key);
  }
  const chunkIds = new Set<string>();
  const outputKeys = new Set<string>();
  const counts = new Map<string, { stdout: number; stderr: number }>();
  for (const chunk of chunks) {
    if (!eventIds.has(chunk.event_id)) throw invalid(`output chunk ${chunk.chunk_id} has unknown event_id`);
    if (chunkIds.has(chunk.chunk_id)) throw invalid("duplicate chunk_id");
    chunkIds.add(chunk.chunk_id);
    const key = `${chunk.event_id}\u0000${chunk.stream}\u0000${chunk.chunk_index}`;
    if (outputKeys.has(key)) throw invalid("duplicate (event_id, stream, chunk_index)");
    outputKeys.add(key);
    const count = counts.get(chunk.event_id) ?? { stdout: 0, stderr: 0 };
    count[chunk.stream] += 1;
    counts.set(chunk.event_id, count);
  }
  for (const event of events) {
    const count = counts.get(event.event_id) ?? { stdout: 0, stderr: 0 };
    if (count.stdout !== event.stdout_chunks || count.stderr !== event.stderr_chunks) throw invalid(`event ${event.event_id} chunk counts do not match stored chunks`);
  }
  for (const run of runs) {
    const runEvents = events.filter((event) => event.run_id === run.run_id);
    if (run.final_sequence !== null && !runEvents.some((event) => event.sequence === run.final_sequence)) throw invalid(`run ${run.run_id} final_sequence does not exist`);
    if (run.trace_complete && run.final_sequence === null) throw invalid(`run ${run.run_id} trace_complete requires final_sequence`);
  }
}

function strictObject(value: unknown, fields: readonly string[], name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw invalid(`${name} must be an object`);
  const root = value as Record<string, unknown>;
  for (const key of Object.keys(root)) if (!fields.includes(key)) throw invalid(`unknown field: ${key}`);
  for (const key of fields) if (!Object.prototype.hasOwnProperty.call(root, key)) throw invalid(`missing field: ${name}.${key}`);
  return root;
}
function requireArray<T>(value: unknown, maximum: number, parse: (item: unknown, field: string) => T, field: string): T[] {
  if (!Array.isArray(value) || value.length > maximum) throw invalid(`${field} must be an array of at most ${maximum} items`);
  return value.map((item, index) => parse(item, `${field}[${index}]`));
}
function requireText(value: unknown, maxBytes: number, field: string): string {
  if (typeof value !== "string") throw invalid(`${field} must be a string`);
  if (new TextEncoder().encode(value).byteLength > maxBytes) throw invalid(`${field} exceeds UTF-8 byte limit of ${maxBytes}`);
  return value;
}
function requireNullableText(value: unknown, maxBytes: number, field: string): string | null { return value === null ? null : requireText(value, maxBytes, field); }
function requireStringArray(value: unknown, maxItems: number, maxBytes: number, field: string): string[] { return requireArray(value, maxItems, (item, index) => requireText(item, maxBytes, index), field); }
function requireSafeInteger(value: unknown, min: number, max: number, field: string): number { if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) throw invalid(`${field} must be a safe integer from ${min} to ${max}`); return value as number; }
function requireNullableInteger(value: unknown, min: number, max: number, field: string): number | null { return value === null ? null : requireSafeInteger(value, min, max, field); }
function requireBoolean(value: unknown, field: string): boolean { if (typeof value !== "boolean") throw invalid(`${field} must be a boolean`); return value; }
function requireNullableBoolean(value: unknown, field: string): boolean | null { return value === null ? null : requireBoolean(value, field); }
function requireEnum<T extends string>(value: unknown, values: readonly T[], field: string): T { if (typeof value !== "string" || !values.includes(value as T)) throw invalid(`${field} must be one of ${values.join(", ")}`); return value as T; }
function requireLiteral<T extends string | number>(value: unknown, expected: T, field: string): T { if (value !== expected) throw invalid(`${field} must equal ${expected}`); return expected; }
function requireUuidV7(value: unknown, field: string): string { if (typeof value !== "string" || !UUID_V7.test(value)) throw invalid(`${field} must be a lowercase UUIDv7`); return value; }
function requireTimestampPair(started: number, ended: number | null, duration: number | null, field: string): void { if ((ended === null) !== (duration === null)) throw invalid(`${field}.ended_at_ms and duration_ms must both be null or present`); if (ended !== null && duration !== null && (ended < started || duration !== ended - started)) throw invalid(`${field}.duration_ms must equal ended_at_ms - started_at_ms`); }
function invalid(message: string): TraceValidationError { return new TraceValidationError(message); }

function bytesToBase64Url(bytes: Uint8Array): string { let binary = ""; for (const byte of bytes) binary += String.fromCharCode(byte); return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, ""); }
function base64UrlToBytes(value: string): Uint8Array { if (!/^[A-Za-z0-9_-]+$/.test(value) || value.length % 4 === 1) throw new Error("invalid base64url"); const binary = atob(value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=")); return Uint8Array.from(binary, (character) => character.charCodeAt(0)); }

function sha256Hex(bytes: Uint8Array): string {
  const words: number[] = []; for (let i = 0; i < bytes.length; i += 1) words[i >> 2] = (words[i >> 2] ?? 0) | (bytes[i] << (24 - (i % 4) * 8));
  const bitLength = bytes.length * 8; words[bitLength >> 5] = (words[bitLength >> 5] ?? 0) | (0x80 << (24 - (bitLength % 32))); const final = (((bitLength + 64) >> 9) << 4) + 15; words[final] = bitLength;
  const hash = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const constants = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
  for (let offset = 0; offset < words.length; offset += 16) {
    const w = Array.from({ length: 64 }, (_, index) => words[offset + index] ?? 0);
    for (let i = 16; i < 64; i += 1) {
      const left = w[i - 15];
      const right = w[i - 2];
      const sigma0 = ((left >>> 7) | (left << 25)) ^ ((left >>> 18) | (left << 14)) ^ (left >>> 3);
      const sigma1 = ((right >>> 17) | (right << 15)) ^ ((right >>> 19) | (right << 13)) ^ (right >>> 10);
      w[i] = (sigma0 + w[i - 7] + sigma1 + w[i - 16]) | 0;
    }
    let [a, b, c, d, e, f, g, h] = hash;
    for (let i = 0; i < 64; i += 1) {
      const s1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const choice = (e & f) ^ (~e & g);
      const t1 = (h + s1 + choice + constants[i] + w[i]) | 0;
      const s0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const majority = (a & b) ^ (a & c) ^ (b & c);
      h = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + s0 + majority) | 0;
    }
    hash[0] = (hash[0] + a) | 0; hash[1] = (hash[1] + b) | 0; hash[2] = (hash[2] + c) | 0; hash[3] = (hash[3] + d) | 0;
    hash[4] = (hash[4] + e) | 0; hash[5] = (hash[5] + f) | 0; hash[6] = (hash[6] + g) | 0; hash[7] = (hash[7] + h) | 0;
  }
  return hash.map((word) => (word >>> 0).toString(16).padStart(8, "0")).join("");
}
