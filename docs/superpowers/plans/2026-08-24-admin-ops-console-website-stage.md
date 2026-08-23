# Nwflash Admin Ops Console Website Stage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and verify the complete website side of structured trace V2—contract fixtures, D1 schema, authenticated ingestion, administrator query APIs, and the six-page Ops Console—without changing or depending on a Rust/Tauri producer.

**Architecture:** Freeze a strict V2 JSON contract first, then implement the Cloudflare API/D1 boundary and stable administrator query responses. The modular browser UI consumes only those persisted responses and explicitly degrades legacy V1 records; it never fabricates successful steps or command evidence. Client trace production, spool, and upload are deferred to a separate Plan C handoff after every website gate passes.

**Tech Stack:** Cloudflare Workers, D1/SQLite, TypeScript, native ES modules, Vitest, `@cloudflare/vitest-plugin`, jsdom, Playwright Chromium, and `@axe-core/playwright`.

## Global Constraints

- Website phase only: do not modify `src/Nwflash.Desktop/**`, `cloudflare/user/**`, or `scripts/vmp/**`.
- Do not deploy production or mutate remote D1; only local tests and `wrangler deploy --dry-run` are allowed.
- The administrator hierarchy is exactly `user → run → events/steps → success detail or failure diagnosis → command/stdout/stderr`.
- Successful and failed steps have equal persisted evidence; exit code `0`, toast text, and browser-generated data are never sufficient proof.
- Administrator responses may expose full operational IPs, serials, paths, URLs, argv, stdout, and stderr; passwords, bearer tokens, cookies, API secrets, signatures, and private keys are removed before D1.
- Preserve V1 `/api/usage/logs` and existing `usage_logs`; V1 detail explicitly reports `legacy_client_no_step_data` and never invents events.
- Use keyset cursors, strict closed enums, `Cache-Control: no-store`, existing administrator session cookies, and existing bearer user authentication.
- No React/Vue, runtime UI framework, remote font, CDN script, inline script, inline style, or `unsafe-inline` CSP allowance.
- Every mutation follows confirmation when destructive → disabled/busy → server result → authoritative reload → durable contextual success or retryable error.
- All browser API text is rendered with DOM text nodes or `textContent`; raw API strings never enter unescaped `innerHTML`.
- TDD is mandatory: each task starts with a focused failing test and ends with an independently reviewable commit.

## Parallel Dispatch and File Ownership

- **Contract/D1/API implementer owns:** `cloudflare/src/trace-v2-*`, `cloudflare/contracts/trace-v2/**`, `cloudflare/web/schema.sql`, `cloudflare/web/migrate-usage-traces-v2.sql`, API sections of `cloudflare/src/index.ts` and `cloudflare/web/src/index.ts`, and server/contract tests.
- **Admin UI implementer owns:** `cloudflare/web/src/admin/**`, `cloudflare/web/e2e/**`, `cloudflare/web/playwright.config.ts`, and UI tests. This implementer starts only after Task 5 freezes administrator responses and does not edit either Worker `index.ts` or D1 SQL.
- **Primary agent owns:** package/config integration, static module routes/CSP, cross-owner conflict resolution, final verification, fixes, and the client handoff.
- **Independent reviewer is read-only:** checks contract/spec coverage, authentication, injection/credential leaks, focus/keyboard, narrow layouts, real loading/empty/error/retry states, and V1 degradation.
- At most three subagents run concurrently. No second implementer may edit files assigned to an active owner.

## Frozen V2 Contract

The following names and wire values are global interfaces for every task:

```ts
export const TRACE_SCHEMA_VERSION = 2 as const;
export const TRACE_UPLOAD_MAX_BODY_BYTES = 1_048_576;
export const TRACE_UPLOAD_MAX_RUNS = 20;
export const TRACE_UPLOAD_MAX_EVENTS = 100;
export const TRACE_UPLOAD_MAX_OUTPUT_CHUNKS = 200;
export const TRACE_OUTPUT_MAX_BYTES = 32_768;

export type TraceOutcomeV2 =
  | "running" | "success" | "failed" | "canceled"
  | "denied" | "aborted" | "unknown";

export type TraceEventKindV2 =
  | "authorization" | "stage" | "partition" | "command"
  | "skip" | "verification" | "terminal";

export type TraceEventStatusV2 =
  | "started" | "success" | "failed" | "canceled"
  | "skipped" | "unknown";

export type TraceOutputStreamV2 = "stdout" | "stderr";

export interface CredentialRedactionCountV2 {
  kind: string;
  count: number;
}

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

export interface TraceUploadRequestV2 {
  schema_version: 2;
  upload_id: string;
  runs: TraceRunV2[];
  events: TraceEventV2[];
  output_chunks: TraceOutputChunkV2[];
}

export interface TraceUploadResponseV2 {
  ok: true;
  accepted: { runs: string[]; events: string[]; output_chunks: string[] };
  rejected: TraceRejectedItemV2[];
}

export type TraceRejectedCodeV2 =
  | "invalid"
  | "missing_parent"
  | "sequence_conflict"
  | "incomplete_trace"
  | "credential_rejected";

export interface TraceRejectedItemV2 {
  entity: "run" | "event" | "output_chunk";
  id: string | null;
  code: TraceRejectedCodeV2;
  message: string;
}

export interface TraceApiErrorV2 {
  ok: false;
  error: {
    code: TraceApiErrorCodeV2;
    message: string;
    request_id: string;
    details?: TraceRejectedItemV2[];
  };
}

export type TraceApiErrorCodeV2 =
  | "TRACE_BODY_TOO_LARGE"
  | "TRACE_INVALID"
  | "TRACE_UNAUTHORIZED"
  | "TRACE_FORBIDDEN"
  | "TRACE_OWNERSHIP_CONFLICT"
  | "TRACE_INCOMPLETE"
  | "TRACE_INTERNAL";

export interface KeysetPageV2<T> {
  items: T[];
  next_cursor: string | null;
}

export interface TraceUserSummaryV2 {
  user_id: number;
  username: string;
  name: string;
  operation_count: number;
  failed_count: number;
  last_operation: TraceRunSummaryV2 | null;
  last_activity_at_ms: number | null;
}

export interface TraceRunSummaryV2 {
  source_schema: 1 | 2;
  trace_ref: string;
  run_id: string | null;
  legacy_id: number | null;
  user_id: number | null;
  username: string | null;
  user_name: string | null;
  operation_kind: string;
  title: string;
  outcome: TraceOutcomeV2;
  client_version: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  trace_complete: boolean;
  trace_loss_reason: string | null;
}

export interface TraceRunDetailV2 {
  source_schema: 1 | 2;
  detail_available: boolean;
  detail_unavailable_reason: "legacy_client_no_step_data" | null;
  run: TraceRunSummaryV2;
  events: TraceEventV2[];
}

export interface TraceEventDetailV2 {
  run: TraceRunSummaryV2;
  event: TraceEventV2;
}

export interface TraceOutputPageV2 {
  run_id: string;
  event_id: string;
  stream: TraceOutputStreamV2;
  chunks: TraceOutputChunkV2[];
  next_after_chunk: number | null;
  output_complete: boolean;
}

export interface TraceOverviewV2 {
  totals: { api_users: number; online_sessions: number; operations: number; failed: number };
  trend: Array<{ bucket_start_ms: number; operations: number; failed: number }>;
  recent_failures: TraceRunSummaryV2[];
}

export interface RomLogAdminRowV2 {
  id: number;
  user_id: number | null;
  user_name: string | null;
  pd: string;
  version: string;
  status: number;
  url: string | null;
  failure_reason: string | null;
  detail_unavailable_reason: "legacy_record_no_failure_reason" | null;
  created_at_ms: number;
}
```

HTTP behavior is fixed:

- `200`: valid upload/query, including same-user idempotent duplicates and item-level rejections.
- `400`: malformed JSON, unknown fields, invalid schema/enums/IDs/relationships/cursor.
- `401`: missing or expired bearer/admin session authentication.
- `403`: banned user or forbidden administrator action.
- `409`: an existing global ID belongs to another user; the entire upload makes no writes.
- `413`: request body exceeds 1 MiB.
- `422`: client requests `trace_complete=true` while declared sequences/chunks are missing.
- `500`: unhandled D1/internal failure; no credential-bearing details are returned.

Cursor encoding is base64url JSON of `{ "v": 1, "started_at_ms": number, "run_id": string }`. Cursor text is opaque to the UI; invalid version, types, UUID, or trailing fields return `TRACE_INVALID`.

Administrator `trace_ref` is also opaque and has one exact encoding: V2 is `v2:<run_id>` and V1 is `v1:<legacy integer id>`. The browser route field is named `runId` for compatibility with the visual design, but its value is always the complete `trace_ref` and must be URL-encoded unchanged.

---

### Task 1: Freeze V2 JSON Schema, TypeScript Contract, Cursors, and Fixtures

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/src/trace-v2-contract.ts`
- Create: `cloudflare/contracts/trace-v2/usage-trace-v2.schema.json`
- Create: `cloudflare/contracts/trace-v2/upload.success.json`
- Create: `cloudflare/contracts/trace-v2/upload.failed.json`
- Create: `cloudflare/contracts/trace-v2/upload-ack.success.json`
- Create: `cloudflare/contracts/trace-v2/admin-users-page.json`
- Create: `cloudflare/contracts/trace-v2/admin-runs-page.json`
- Create: `cloudflare/contracts/trace-v2/admin-run-success.json`
- Create: `cloudflare/contracts/trace-v2/admin-run-failed.json`
- Create: `cloudflare/contracts/trace-v2/admin-run-legacy.json`
- Create: `cloudflare/test/trace-v2-contract.test.ts`
- Modify: `cloudflare/package.json`
- Modify: `cloudflare/package-lock.json`

**Interfaces:**
- Consumes: approved design document and the frozen contract above.
- Produces: `readTraceUploadV2`, `validateTraceUploadV2`, `encodeTraceCursorV2`, `decodeTraceCursorV2`, every V2 TypeScript interface, JSON Schema, and canonical fixtures used by API and UI tests.

- [ ] **Step 1: Write failing contract and cursor tests**

```ts
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  decodeTraceCursorV2,
  encodeTraceCursorV2,
  validateTraceUploadV2,
} from "../src/trace-v2-contract";

const valid = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/upload.success.json", import.meta.url),
  "utf8",
));

describe("trace v2 contract", () => {
  it("accepts the canonical success fixture", () => {
    expect(validateTraceUploadV2(valid)).toEqual(valid);
  });

  it("rejects unknown fields", () => {
    expect(() => validateTraceUploadV2({ ...valid, token: "must-not-exist" }))
      .toThrow(/unknown field: token/);
  });

  it("round-trips the opaque keyset cursor", () => {
    const value = { v: 1 as const, started_at_ms: 1_787_500_000_123, run_id: valid.runs[0].run_id };
    expect(decodeTraceCursorV2(encodeTraceCursorV2(value))).toEqual(value);
  });
});
```

- [ ] **Step 2: Run the focused test and verify the missing contract failure**

Run:

```powershell
npm --prefix cloudflare exec -- vitest run test/trace-v2-contract.test.ts
```

Expected: FAIL because `trace-v2-contract.ts` and JSON fixtures do not exist.

- [ ] **Step 3: Implement the complete closed contract**

Create exact interfaces for `CredentialRedactionCountV2`, `TraceCommandV2`, `TraceRunV2`, `TraceEventV2`, `TraceOutputChunkV2`, `TraceRejectedItemV2`, upload/ack/error responses, administrator summaries/details, and `KeysetPageV2<T>`.

```ts
export function validateTraceUploadV2(value: unknown): TraceUploadRequestV2 {
  const root = strictObject(value, ["schema_version", "upload_id", "runs", "events", "output_chunks"]);
  requireLiteral(root.schema_version, TRACE_SCHEMA_VERSION, "schema_version");
  const uploadId = requireUuidV7(root.upload_id, "upload_id");
  const runs = requireArray(root.runs, TRACE_UPLOAD_MAX_RUNS, parseRunV2, "runs");
  const events = requireArray(root.events, TRACE_UPLOAD_MAX_EVENTS, parseEventV2, "events");
  const chunks = requireArray(root.output_chunks, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS, parseChunkV2, "output_chunks");
  validateParentRelationships(runs, events, chunks);
  return { schema_version: 2, upload_id: uploadId, runs, events, output_chunks: chunks };
}

export function encodeTraceCursorV2(cursor: TraceCursorV2): string {
  return bytesToBase64Url(new TextEncoder().encode(JSON.stringify(cursor)));
}

export function decodeTraceCursorV2(encoded: string): TraceCursorV2 {
  const parsed = JSON.parse(new TextDecoder().decode(base64UrlToBytes(encoded)));
  const root = strictObject(parsed, ["v", "started_at_ms", "run_id"]);
  return {
    v: requireLiteral(root.v, 1, "cursor.v"),
    started_at_ms: requireSafeInteger(root.started_at_ms, 0, Number.MAX_SAFE_INTEGER, "cursor.started_at_ms"),
    run_id: requireUuidV7(root.run_id, "cursor.run_id"),
  };
}
```

Validation must reject unknown fields, non-lowercase UUIDv7 values, invalid enums, unsafe integers, negative counts, inconsistent timestamps, duplicate IDs, duplicate `(run_id, sequence)`, duplicate `(event_id, stream, chunk_index)`, UTF-8 fields over limits, and chunk `byte_count`/SHA-256 mismatches against stored fixture text.

- [ ] **Step 4: Make `npm test` and typecheck discover website-stage files**

For Task 1, set the script exactly to the files that exist at this point:

```json
{
  "test": "vitest run test/*.test.ts",
  "test:workerd": "vitest run --config vitest.workerd.config.ts",
  "typecheck": "tsc --noEmit --strict --target ES2022 --module ESNext --moduleResolution Bundler --lib ES2022,WebWorker --types @cloudflare/workers-types --skipLibCheck src/index.ts src/security.ts src/trace-v2-contract.ts && wrangler deploy --dry-run --outdir .wrangler/typecheck-api"
}
```

Task 2 adds `src/trace-v2-redaction.ts`; Task 4 adds `src/trace-v2-ingest.ts`; Task 5 adds `web/src/trace-v2-query.ts` and the web dry-run command; Task 6 adds `src/trace-v2-retention.ts`. Task 11 adds the raw Text-module declaration and then includes `web/src/index.ts`. Each task updates the script in the same commit that creates its module.

- [ ] **Step 5: Run contract tests and typecheck**

Run:

```powershell
npm --prefix cloudflare exec -- vitest run test/trace-v2-contract.test.ts
npm --prefix cloudflare run typecheck
```

Expected: PASS.

- [ ] **Step 6: Commit the frozen contract**

```powershell
git add cloudflare/src/trace-v2-contract.ts cloudflare/contracts/trace-v2 cloudflare/test/trace-v2-contract.test.ts cloudflare/package.json cloudflare/package-lock.json
git commit -m "feat(cloudflare): freeze usage trace v2 contract"
```

---

### Task 2: Add the Defense-in-Depth Credential Boundary

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/src/trace-v2-redaction.ts`
- Create: `cloudflare/test/trace-v2-redaction.test.ts`
- Modify: `cloudflare/src/trace-v2-contract.ts`
- Modify: `cloudflare/package.json`

**Interfaces:**
- Consumes: `TraceUploadRequestV2` and authenticated bearer value.
- Produces: `redactTraceUploadV2(payload, exactSecrets)` with redaction counts for runs, events, and chunks. Operational paths, serials, argv, and noncredential URLs remain visible.

- [ ] **Step 1: Write failing credential-sentinel tests**

```ts
it("removes credentials but preserves operational fields", () => {
  const source = canonicalUploadWithSentinels({
    bearer: "bearer-sentinel-314159",
    cookie: "cookie-sentinel-271828",
    serial: "9A7F23BC10D4",
    path: "C:\\Users\\Public\\Nwflash\\super.img",
  });
  const result = redactTraceUploadV2(source, ["bearer-sentinel-314159"]);
  const stored = JSON.stringify(result.payload);
  expect(stored).not.toContain("bearer-sentinel-314159");
  expect(stored).not.toContain("cookie-sentinel-271828");
  expect(stored).toContain("9A7F23BC10D4");
  expect(stored).toContain("C:\\\\Users\\\\Public\\\\Nwflash\\\\super.img");
  expect([...result.chunk_redactions.values()].flat()).toContainEqual({ kind: "bearer", count: 1 });
});
```

- [ ] **Step 2: Run the focused test and verify it fails**

```powershell
npm --prefix cloudflare exec -- vitest run test/trace-v2-redaction.test.ts
```

Expected: FAIL because the redaction module does not exist.

- [ ] **Step 3: Implement exact redaction behavior**

```ts
export interface RedactedTraceUploadV2 {
  payload: TraceUploadRequestV2;
  run_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
  event_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
  chunk_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
}

export function redactTraceUploadV2(
  payload: TraceUploadRequestV2,
  exactSecrets: readonly string[],
): RedactedTraceUploadV2 {
  const matcher = buildCredentialMatcher(exactSecrets.filter((value) => value.length >= 6));
  return mapTraceTextFields(payload, (text, location) => matcher.replace(text, location));
}
```

Patterns remove only credential values for Authorization/Bearer, Cookie/Set-Cookie, password/token/api-key/secret/signature assignments, CLI secret flags, URL userinfo/query credentials, PEM blocks, and OpenSSH private-key blocks. Unparseable high-risk material becomes `[CREDENTIAL_REMOVED:HIGH_RISK]`. Store `{kind,count}` only; never store secret hashes.

When chunk text changes, recompute `byte_count` and SHA-256 from the post-redaction UTF-8 text before constructing D1 statements or acknowledgements.

- [ ] **Step 4: Run redaction, contract, and type tests**

```powershell
npm --prefix cloudflare exec -- vitest run test/trace-v2-redaction.test.ts test/trace-v2-contract.test.ts
npm --prefix cloudflare run typecheck
```

Expected: PASS.

- [ ] **Step 5: Commit the boundary**

```powershell
git add cloudflare/src/trace-v2-redaction.ts cloudflare/src/trace-v2-contract.ts cloudflare/test/trace-v2-redaction.test.ts
git commit -m "feat(cloudflare): add trace credential boundary"
```

---

### Task 3: Add the Idempotent D1 V2 Schema and Migration

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/web/migrate-usage-traces-v2.sql`
- Modify: `cloudflare/web/schema.sql`
- Create: `cloudflare/test/trace-v2-migration.workerd.test.ts`
- Modify: `cloudflare/vitest.workerd.config.ts`

**Interfaces:**
- Consumes: Task 1 field names and closed enums.
- Produces: `usage_operation_runs`, `usage_operation_events`, `usage_output_chunks`, indexes, run/chunk credential redaction counts, and unchanged V1 `usage_logs`.

- [ ] **Step 1: Write a failing real-D1 migration test**

```ts
it("applies the V2 migration twice and preserves V1 rows", async () => {
  await env.DB.prepare(
    "INSERT INTO usage_logs (operation_kind,status,event_key,started_at) VALUES ('Flashing','success','legacy-1',1)",
  ).run();
  await env.DB.exec(migrationSql);
  await env.DB.exec(migrationSql);
  expect(await scalar("SELECT COUNT(*) FROM usage_logs WHERE event_key='legacy-1'")).toBe(1);
  expect(await tableExists("usage_operation_runs")).toBe(true);
  expect(await tableExists("usage_operation_events")).toBe(true);
  expect(await tableExists("usage_output_chunks")).toBe(true);
});
```

- [ ] **Step 2: Run the migration test and verify missing tables**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-migration.workerd.test.ts
```

Expected: FAIL because the migration and V2 tables do not exist.

- [ ] **Step 3: Implement the full idempotent SQL**

Create all columns from the approved spec plus:

```sql
credential_redactions_json TEXT NOT NULL DEFAULT '[]'
```

on both `usage_operation_runs` and `usage_output_chunks`, plus these event-detail columns:

```sql
device_state TEXT,
retry_safe INTEGER CHECK(retry_safe IS NULL OR retry_safe IN (0,1)),
remedies_json TEXT NOT NULL DEFAULT '[]'
```

Add `IF NOT EXISTS`, nonnegative checks, closed outcome/event/status/stream checks, global primary keys, unique `(run_id, sequence)` and `(event_id, stream, chunk_index)`, and every index named in the specification. Keep `usage_logs` byte-for-byte compatible.

- [ ] **Step 4: Make Workerd discover every website-stage integration test**

```ts
test: {
  include: ["test/*.workerd.test.ts"],
  testTimeout: 10_000,
}
```

Add Text module rules for `**/*.sql`, `**/*.html`, `**/*.css`, and `**/admin/**/*.js` as their files appear.

- [ ] **Step 5: Run migration and existing Workerd tests**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-migration.workerd.test.ts test/security.workerd.test.ts
```

Expected: PASS and existing kick/session behavior remains green.

- [ ] **Step 6: Commit the schema**

```powershell
git add cloudflare/web/migrate-usage-traces-v2.sql cloudflare/web/schema.sql cloudflare/test/trace-v2-migration.workerd.test.ts cloudflare/vitest.workerd.config.ts
git commit -m "feat(d1): add usage trace v2 schema"
```

---

### Task 4: Implement Authenticated V2 Ingestion, Idempotent Ack, and V1 Projection

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/src/trace-v2-ingest.ts`
- Modify: `cloudflare/src/index.ts`
- Create: `cloudflare/test/trace-v2-ingest.workerd.test.ts`
- Modify: `cloudflare/package.json`

**Interfaces:**
- Consumes: validated/redacted `TraceUploadRequestV2`, D1 schema, enabled unbanned bearer user, `CF-Connecting-IP`.
- Produces: `POST /api/usage/traces/v2`, item ack/rejections, cross-user conflict rollback, finalization checks, and legacy summary projection.

- [ ] **Step 1: Write failing ingestion tests using canonical success and failure fixtures**

```ts
it("acks the canonical upload and projects one terminal V1 summary", async () => {
  await seedUser("trace-bearer", 7);
  const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual(successAckFixture);
  expect(await scalar("SELECT COUNT(*) FROM usage_operation_runs WHERE api_user_id=7")).toBe(1);
  expect(await scalar("SELECT COUNT(*) FROM usage_logs WHERE event_key=?", successFixture.runs[0].run_id)).toBe(1);
});

it("rolls back a cross-user global ID conflict", async () => {
  await seedRunOwnedBy(8, successFixture.runs[0].run_id);
  const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");
  expect(response.status).toBe(409);
  expect(await scalar("SELECT COUNT(*) FROM usage_operation_events")).toBe(0);
});
```

- [ ] **Step 2: Run the focused Workerd test and verify route failure**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-ingest.workerd.test.ts
```

Expected: FAIL with `404` because `/api/usage/traces/v2` is absent.

- [ ] **Step 3: Implement the service boundary and route**

```ts
export interface AuthenticatedTraceUser {
  id: number;
  username: string;
  name: string;
  bearer_token: string;
}

export async function ingestTraceUploadV2(
  env: Pick<Env, "DB">,
  request: Request,
  user: AuthenticatedTraceUser,
): Promise<Response> {
  const payload = await readTraceUploadV2(request);
  const sanitized = redactTraceUploadV2(payload, [user.bearer_token]);
  const conflict = await findCrossUserOwnershipConflict(env.DB, sanitized.payload, user.id);
  if (conflict) return traceError(409, "TRACE_OWNERSHIP_CONFLICT", "日志标识已属于其他用户。", conflict);
  const statements = await buildValidatedTraceStatements(env.DB, sanitized, user, request.headers.get("CF-Connecting-IP") ?? "");
  const committed = await env.DB.batch(statements);
  return traceJson(buildAcknowledgement(committed), 200);
}
```

Route after the existing app-version gate and bearer authentication. Reject disabled/banned users. Validate all parent ownership before writes. Same-user duplicate IDs are accepted idempotently. Missing parents/sequence conflicts become item rejections. `trace_complete=true` requires sequences `1..final_sequence` and every declared chunk index across persisted plus current items; otherwise return `422` without marking complete. Project terminal V2 run summary to `usage_logs` with `event_key=run_id` in the same D1 batch.

- [ ] **Step 4: Run ingestion, V1 compatibility, and credential scans**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-ingest.workerd.test.ts test/security.workerd.test.ts
```

Expected: PASS; credential sentinels have zero matches in D1 and responses.

- [ ] **Step 5: Commit ingestion**

```powershell
git add cloudflare/src/trace-v2-ingest.ts cloudflare/src/index.ts cloudflare/test/trace-v2-ingest.workerd.test.ts
git commit -m "feat(api): ingest structured usage traces v2"
```

---

### Task 5: Freeze Administrator Queries, V1/V2 Dual Read, Overview, and Export

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/web/src/trace-v2-query.ts`
- Modify: `cloudflare/web/src/index.ts`
- Create: `cloudflare/test/trace-v2-admin.workerd.test.ts`
- Modify/Create: administrator response fixtures under `cloudflare/contracts/trace-v2/`
- Modify: `cloudflare/package.json`

**Interfaces:**
- Consumes: Tasks 1–4 and administrator cookie auth.
- Produces: stable pages/detail/output, authoritative overview, audited NDJSON export, V1 degradation, and exact filters consumed by the UI.

- [ ] **Step 1: Write failing query, cursor, deduplication, overview, and audit tests**

```ts
it("paginates equal timestamps without duplicates or gaps", async () => {
  await seedRunsAtSameTimestamp(75, 1_787_500_000_000);
  const first = await adminGet("/api/usage-logs/v2/runs?limit=50");
  const page1 = await first.json() as KeysetPageV2<TraceRunSummaryV2>;
  const second = await adminGet(`/api/usage-logs/v2/runs?limit=50&cursor=${encodeURIComponent(page1.next_cursor!)}`);
  const page2 = await second.json() as KeysetPageV2<TraceRunSummaryV2>;
  expect(new Set([...page1.items, ...page2.items].map((item) => item.trace_ref)).size).toBe(75);
});

it("returns explicit legacy detail unavailability", async () => {
  const response = await adminGet("/api/usage-logs/v2/runs/v1%3A42");
  expect(await response.json()).toMatchObject({
    source_schema: 1,
    detail_available: false,
    detail_unavailable_reason: "legacy_client_no_step_data",
    events: [],
  });
});
```

- [ ] **Step 2: Run the focused Workerd test and verify missing routes**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-admin.workerd.test.ts
```

Expected: FAIL with `404` for all V2 administrator routes.

- [ ] **Step 3: Implement exact route handlers**

```ts
export async function listTraceUsersV2(request: Request, url: URL, env: Env): Promise<Response>;
export async function listTraceRunsV2(request: Request, url: URL, env: Env): Promise<Response>;
export async function getTraceRunV2(request: Request, traceRef: string, env: Env): Promise<Response>;
export async function getTraceEventV2(request: Request, traceRef: string, eventId: string, env: Env): Promise<Response>;
export async function getTraceOutputV2(request: Request, traceRef: string, eventId: string, url: URL, admin: AdminIdentity, env: Env): Promise<Response>;
export async function getTraceOverviewV2(request: Request, url: URL, env: Env): Promise<Response>;
export async function exportTracesV2(request: Request, url: URL, admin: AdminIdentity, env: Env): Promise<Response>;
```

Endpoints and filters:

```text
GET /api/usage-logs/v2/users?from&to&status&q&limit&cursor
GET /api/usage-logs/v2/runs?userId&kind&status&from&to&partition&errorCode&q&limit&cursor
GET /api/usage-logs/v2/runs/{traceRef}
GET /api/usage-logs/v2/runs/{traceRef}/events/{eventId}
GET /api/usage-logs/v2/runs/{traceRef}/events/{eventId}/output?stream&afterChunk&limit
GET /api/usage-logs/v2/overview?from&to&bucket=hour
GET /api/usage-logs/v2/export?<same filters as runs>
```

`q` searches username/name, run/event ID, partition, error code, serial, path, URL, title, and operation kind. Keyset SQL is:

```sql
WHERE (started_at_ms < ? OR (started_at_ms = ? AND run_id < ?))
ORDER BY started_at_ms DESC, run_id DESC
LIMIT ?
```

Default limit is 50; maximum 200. V2 rows suppress projected V1 duplicate where `usage_logs.event_key = usage_operation_runs.run_id`. V1 `trace_ref` is `v1:<id>`. Complete output reads and NDJSON exports write `view_trace_output` and `export_trace` to `admin_audit_log` before returning data.

- [ ] **Step 4: Add authoritative version and ROM administrator contracts**

Add:

```text
GET /api/app-versions/summary
  -> {current_version,minimum_version,supported_versions,today_426,as_of_ms}

GET /api/rom-logs/v2?userId&pd&version&status&q&limit&cursor
  -> KeysetPageV2<RomLogAdminRowV2>
```

`RomLogAdminRowV2` contains the complete persisted URL and `failure_reason: string | null`; legacy rows without a reason return `failure_reason=null` and `detail_unavailable_reason="legacy_record_no_failure_reason"`.

`today_426` is an authoritative count from V2 runs whose persisted `error_code` is `UPDATE_REQUIRED` within the current UTC day. Until a client producer uploads such runs, the correct value is `0`; the browser never estimates it.

- [ ] **Step 5: Run the entire contract/API gate**

```powershell
npm --prefix cloudflare exec -- vitest run test/trace-v2-contract.test.ts test/trace-v2-redaction.test.ts
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-migration.workerd.test.ts test/trace-v2-ingest.workerd.test.ts test/trace-v2-admin.workerd.test.ts
npm --prefix cloudflare run typecheck
```

Expected: PASS. This is the hard gate before the Admin UI implementer starts.

- [ ] **Step 6: Commit administrator contracts**

```powershell
git add cloudflare/web/src/trace-v2-query.ts cloudflare/web/src/index.ts cloudflare/test/trace-v2-admin.workerd.test.ts cloudflare/contracts/trace-v2
git commit -m "feat(admin-api): add trace v2 query contract"
```

---

### Task 6: Enforce Trace Retention in the Existing API Cron

**Owner:** Contract/D1/API implementer

**Files:**
- Create: `cloudflare/src/trace-v2-retention.ts`
- Modify: `cloudflare/src/index.ts`
- Create: `cloudflare/test/trace-v2-retention.workerd.test.ts`
- Modify: `cloudflare/package.json`

**Interfaces:**
- Consumes: D1 V2 schema.
- Produces: `purgeExpiredTraceData(db, nowMs)` with 30/90/180-day behavior and count-only logging.

- [ ] **Step 1: Write failing retention-boundary tests**

```ts
it("clears operational detail after thirty days but preserves the run summary", async () => {
  await seedTrace({ ageDays: 31, marker: "expired-command-marker" });
  const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);
  expect(result.output_chunks_deleted).toBeGreaterThan(0);
  expect(await scalar("SELECT COUNT(*) FROM usage_operation_runs")).toBe(1);
  expect(await databaseContains("expired-command-marker")).toBe(false);
});
```

- [ ] **Step 2: Run and verify missing retention module**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-retention.workerd.test.ts
```

Expected: FAIL because the module is absent.

- [ ] **Step 3: Implement count-only retention**

```ts
export interface TraceRetentionResult {
  output_chunks_deleted: number;
  sensitive_fields_cleared: number;
  events_deleted: number;
  runs_deleted: number;
  cutoff_30d_ms: number;
  cutoff_90d_ms: number;
  cutoff_180d_ms: number;
}

export async function purgeExpiredTraceData(db: D1Database, nowMs: number): Promise<TraceRetentionResult>;
```

At 30 days delete outputs and clear command/paths/URLs/IP/serial; at 90 days delete event metadata; at 180 days delete runs. The scheduled handler logs only result counts and cutoffs.

- [ ] **Step 4: Run retention and existing cron tests**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/trace-v2-retention.workerd.test.ts test/security.workerd.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit retention**

```powershell
git add cloudflare/src/trace-v2-retention.ts cloudflare/src/index.ts cloudflare/test/trace-v2-retention.workerd.test.ts
git commit -m "feat(cloudflare): enforce trace retention windows"
```

---

### Task 7: Build the Modular Authenticated Admin Shell and Router

**Owner:** Admin UI implementer, after Task 5 gate

**Files:**
- Create: `cloudflare/web/src/admin/index.html`
- Create: `cloudflare/web/src/admin/styles.css`
- Create: `cloudflare/web/src/admin/app.js`
- Create: `cloudflare/web/src/admin/api.js`
- Create: `cloudflare/web/src/admin/router.js`
- Create: `cloudflare/web/src/admin/components.js`
- Create: `cloudflare/web/src/admin/tests/router.test.js`
- Create: `cloudflare/web/src/admin/tests/api.test.js`
- Create: `cloudflare/web/src/admin/tests/components.test.js`
- Create: `cloudflare/web/e2e/admin-api-fixtures.ts`
- Create: `cloudflare/web/e2e/serve-admin.mjs`
- Create: `cloudflare/web/e2e/admin-shell.spec.ts`
- Create: `cloudflare/web/playwright.config.ts`
- Modify: `cloudflare/package.json`
- Modify: `cloudflare/package-lock.json`

**Interfaces:**
- Consumes: stable Task 5 fixtures/endpoints.
- Produces: six-menu UTF-8 shell, authentication restore/login/logout, strict API client, documented URL state, dialogs/status/page-state components, unit tests, and browser fixture harness. Does not edit Worker routes.

- [ ] **Step 1: Add test-only browser dependencies**

```powershell
npm --prefix cloudflare install --save-dev jsdom @playwright/test @axe-core/playwright
npm --prefix cloudflare exec -- playwright install chromium
```

Expected: package and lockfile contain test-only dependencies; no runtime framework is added.

- [ ] **Step 2: Write failing router, API, component, and shell tests**

```js
it("round-trips a command deep link without sensitive fields", () => {
  const route = {
    view: "audit", userId: "42", runId: "v2:019d0000-0000-7000-8000-000000000001",
    eventId: "019d0000-0000-7000-8000-000000000002", level: "command", stream: "stderr",
    from: null, to: null, status: "failed", kind: "Flashing", partition: "super",
    errorCode: "FLASH_PARTITION_NOT_FOUND", q: null, cursor: null,
  };
  const encoded = serializeRoute(route);
  expect(parseRoute(encoded)).toEqual(route);
  expect(encoded).not.toMatch(/token|password|stdout|stderr=.*FAILED/);
});
```

Browser test asserts exactly six visible menu labels, one `aria-current="page"`, keyboard Arrow/Home/End navigation, login recovery through `/api/me`, and session-expiry return to login.

The UI owner does not need Worker-route ownership. `serve-admin.mjs` is a test-only Node server:

```js
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join } from "node:path";

const root = new URL("../src/admin/", import.meta.url);
const mime = { ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8", ".js": "text/javascript; charset=utf-8" };
createServer(async (request, response) => {
  const pathname = new URL(request.url, "http://127.0.0.1").pathname;
  const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/admin\//, "");
  try {
    const body = await readFile(new URL(relative, root));
    response.writeHead(200, { "Content-Type": mime[extname(relative)] ?? "application/octet-stream" });
    response.end(body);
  } catch {
    response.writeHead(404).end("Not found");
  }
}).listen(4179, "127.0.0.1");
```

`playwright.config.ts` starts it with `node web/e2e/serve-admin.mjs`, uses `http://127.0.0.1:4179`, and each spec intercepts `/api/**` with canonical contract fixtures. Production Worker static routing remains Task 11.

- [ ] **Step 3: Run tests and verify missing modules**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/*.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-shell.spec.ts
```

Expected: FAIL because the modular shell does not exist.

- [ ] **Step 4: Implement exact router and page contracts**

```js
export function parseRoute(search) {}
export function serializeRoute(route) {}
export function createRouter({ window, onRoute }) {
  return { start, navigate, restoreReturnPoint, destroy };
}

export function createApiClient({ fetchImpl, onUnauthorized }) {
  return {
    request, getMe, login, logout, changePassword,
    getAppVersions, getVersionSummary, createAppVersion, updateAppVersion, deleteAppVersion,
    getUsers, createUser, updateUser, deleteUser, rotateUserToken,
    getOnlineSessions, kickSession,
    getTraceOverview, getTraceUsers, getTraceRuns, getTraceRun, getTraceEvent,
    getTraceOutput, exportTrace, getRomLogs,
  };
}
```

`request()` accepts `AbortSignal`, adds `X-Requested-With: XMLHttpRequest` only to mutations, classifies `401`, never retries mutations automatically, and parses the frozen error envelope. `components.js` implements DOM-safe element creation, loading/empty/partial/stale/unauthorized/error/retry states, focus-trapped confirmation, short status announcement, persistent alert, cursor controls, six-menu roving focus, and history focus return.

- [ ] **Step 5: Implement shell semantics and base responsive tokens**

Exactly six primary items: `概览`, `版本策略`, `用户管理`, `在线会话`, `操作审计`, `ROM 查询`. Change password/logout remain in the account menu. Core body text is at least 13px, code/time 12px, targets 44px, and narrow navigation keeps text labels.

- [ ] **Step 6: Run focused UI tests**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/*.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-shell.spec.ts
```

Expected: PASS with zero browser console errors.

- [ ] **Step 7: Commit UI foundation**

```powershell
git add cloudflare/web/src/admin cloudflare/web/e2e/admin-api-fixtures.ts cloudflare/web/e2e/serve-admin.mjs cloudflare/web/e2e/admin-shell.spec.ts cloudflare/web/playwright.config.ts cloudflare/package.json cloudflare/package-lock.json
git commit -m "feat(admin): add authenticated ops console shell"
```

---

### Task 8: Implement Overview, Version, User, Session, and ROM Workspaces

**Owner:** Admin UI implementer

**Files:**
- Create: `cloudflare/web/src/admin/pages/overview.js`
- Create: `cloudflare/web/src/admin/pages/versions.js`
- Create: `cloudflare/web/src/admin/pages/users.js`
- Create: `cloudflare/web/src/admin/pages/sessions.js`
- Create: `cloudflare/web/src/admin/pages/rom.js`
- Create: `cloudflare/web/src/admin/tests/pages.test.js`
- Create: `cloudflare/web/e2e/admin-workspaces.spec.ts`
- Modify: `cloudflare/web/src/admin/app.js`
- Modify: `cloudflare/web/src/admin/styles.css`
- Modify: `cloudflare/web/e2e/admin-api-fixtures.ts`

**Interfaces:**
- Consumes: Task 5 administrator contracts and Task 7 page context.
- Produces: five real workspaces with authoritative states and safe mutations; audit is Task 9/10.

- [ ] **Step 1: Write failing page lifecycle and mutation tests**

```js
for (const createPage of [createOverviewPage, createVersionsPage, createUsersPage, createSessionsPage, createRomPage]) {
  it(`${createPage.name} renders loading failure and retry`, async () => {
    const page = createPage(failingContext());
    const activation = page.activate(defaultRoute(), new AbortController().signal);
    expect(screen.getByRole("status")).toHaveTextContent("正在加载");
    await activation;
    expect(screen.getByRole("alert")).toHaveTextContent("加载失败");
    expect(screen.getByRole("button", { name: "重试" })).toBeVisible();
  });
}
```

Browser tests cover overview fixture totals, version delete confirmation, one-time token clearing, visibility-aware session polling, kick pending-until-removal, complete ROM URL display, and legacy missing failure reason.

- [ ] **Step 2: Run tests and verify missing pages**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/pages.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-workspaces.spec.ts
```

Expected: FAIL because page modules are missing.

- [ ] **Step 3: Implement the shared page lifecycle**

Every module exports:

```js
export function createXPage(context) {
  return {
    async activate(route, signal) {},
    deactivate() {},
    destroy() {},
  };
}
```

Overview consumes only `/api/usage-logs/v2/overview`. Version and user mutations use the existing routes plus authoritative reload. One-time token DOM is cleared on navigation/refresh. Session polling runs every 10 seconds only while active, visible, and idle. ROM consumes the stable cursor endpoint and never invents a legacy failure reason.

- [ ] **Step 4: Implement destructive-action state machines**

Version delete, token rotation, user ban/delete, and session kick all use the shared confirmation dialog, focus Cancel initially, disable duplicate submission, retain failure context, and reload server state after success. Toast/status is supplemental, never the only result.

- [ ] **Step 5: Run page and browser tests**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/pages.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-workspaces.spec.ts
```

Expected: PASS.

- [ ] **Step 6: Commit five workspaces**

```powershell
git add cloudflare/web/src/admin cloudflare/web/e2e/admin-workspaces.spec.ts cloudflare/web/e2e/admin-api-fixtures.ts
git commit -m "feat(admin): rebuild operational workspaces"
```

---

### Task 9: Implement Audit Levels 1–3 and Legacy Degradation

**Owner:** Admin UI implementer

**Files:**
- Create: `cloudflare/web/src/admin/pages/audit.js`
- Create: `cloudflare/web/src/admin/tests/audit-navigation.test.js`
- Create: `cloudflare/web/e2e/admin-audit-navigation.spec.ts`
- Modify: `cloudflare/web/src/admin/app.js`
- Modify: `cloudflare/web/src/admin/styles.css`
- Modify: `cloudflare/web/e2e/admin-api-fixtures.ts`

**Interfaces:**
- Consumes: `getTraceUsers`, `getTraceRuns`, `getTraceRun`, route fields, `TraceUserSummaryV2`, `TraceRunSummaryV2`.
- Produces: user summary → run list → persisted event list, URL state, focus restoration, and explicit V1 stop.

- [ ] **Step 1: Write failing hierarchy and history tests**

```ts
test("drills user to run to ordered persisted events and returns focus", async ({ page }) => {
  await page.goto("/?view=audit");
  await page.getByRole("button", { name: /张三/ }).click();
  await page.getByRole("button", { name: /VIVO 线刷/ }).click();
  await expect(page.locator("[data-event-sequence]")).toHaveText(["1", "2", "3", "4"]);
  await page.goBack();
  await expect(page.getByRole("button", { name: /VIVO 线刷/ })).toBeFocused();
});
```

- [ ] **Step 2: Run tests and verify hierarchy is absent**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/audit-navigation.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-audit-navigation.spec.ts
```

Expected: FAIL.

- [ ] **Step 3: Implement levels 1–3 from persisted responses only**

Use `trace_ref` exactly, including URL-encoded `v1:<id>`. Render closed statuses with text/icon/color. When `source_schema=1`, stop at the run summary and display `旧客户端未上传步骤数据`; do not create synthetic events. When `trace_complete=false`, show `trace_loss_reason` and partial state.

- [ ] **Step 4: Implement breadcrumb/history/focus semantics**

Current breadcrumb uses `aria-current="page"`. Each drill stores `focusId` and `scrollY` in history state, focuses the new level heading, and restores the source row on Back. The trace tree is not a live region.

- [ ] **Step 5: Run hierarchy tests**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/audit-navigation.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-audit-navigation.spec.ts
```

Expected: PASS.

- [ ] **Step 6: Commit audit navigation**

```powershell
git add cloudflare/web/src/admin cloudflare/web/e2e/admin-audit-navigation.spec.ts cloudflare/web/e2e/admin-api-fixtures.ts
git commit -m "feat(admin): add user-first trace audit navigation"
```

---

### Task 10: Implement Success/Failure Detail, Command Streams, and Export

**Owner:** Admin UI implementer

**Files:**
- Modify: `cloudflare/web/src/admin/pages/audit.js`
- Create: `cloudflare/web/src/admin/tests/audit-detail.test.js`
- Create: `cloudflare/web/e2e/admin-audit-detail.spec.ts`
- Modify: `cloudflare/web/src/admin/styles.css`
- Modify: `cloudflare/web/e2e/admin-api-fixtures.ts`

**Interfaces:**
- Consumes: `getTraceEvent`, `getTraceOutput`, `exportTrace`, persisted `verification`, ordered output chunks.
- Produces: levels 4–5 for every success/failure step, separate complete stdout/stderr, no browser-inferred evidence, and audited filtered export.

- [ ] **Step 1: Write failing success/failure parity and stream tests**

```ts
test("does not synthesize success from exit code zero", async ({ page }) => {
  await openEventFixture(page, { exit_code: 0, status: "unknown", verification: null });
  await expect(page.getByText("成功", { exact: true })).toHaveCount(0);
  await expect(page.getByText("UNKNOWN", { exact: true })).toBeVisible();
});

test("appends stdout chunks in persisted order", async ({ page }) => {
  await openCommandOutput(page, "stdout");
  await expect(page.locator("[data-output-stream=stdout]")).toHaveText("chunk-0chunk-1chunk-2");
});
```

- [ ] **Step 2: Run tests and verify detail/stream failure**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/audit-detail.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-audit-detail.spec.ts
```

Expected: FAIL.

- [ ] **Step 3: Implement level 4 from the event detail response**

Success and failure both display result category, stage, partition, exit code, sequence, full returned evidence, duration, and server verification. Failure additionally displays class/code/reason, last verified step, first failed step, device stop state, retry safety, skipped remainder, and remedies. No value is derived from exit code in browser code.

- [ ] **Step 4: Implement level 5 ordered output pagination**

Request stdout and stderr independently. Append by `chunk_index`, enforce `next_after_chunk`, show `(empty)` only when `output_complete=true` and no chunks exist, and constrain long path/output overflow to labeled local code regions.

- [ ] **Step 5: Implement server-filter export**

Pass only current non-output filters to the export endpoint, trigger the NDJSON download, and never serialize command output into the URL or browser storage.

- [ ] **Step 6: Run audit detail tests**

```powershell
npm --prefix cloudflare exec -- vitest run web/src/admin/tests/audit-detail.test.js
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-audit-detail.spec.ts
```

Expected: PASS.

- [ ] **Step 7: Commit full audit detail**

```powershell
git add cloudflare/web/src/admin cloudflare/web/e2e/admin-audit-detail.spec.ts cloudflare/web/e2e/admin-api-fixtures.ts
git commit -m "feat(admin): add complete trace command evidence"
```

---

### Task 11: Integrate Static Module Routes and Strict CSP

**Owner:** Primary agent after API and UI owners finish

**Files:**
- Modify: `cloudflare/web/src/index.ts`
- Modify: `cloudflare/web/wrangler.toml`
- Modify: `cloudflare/vitest.workerd.config.ts`
- Create: `cloudflare/web/src/admin/text-modules.d.ts`
- Modify: `cloudflare/package.json`
- Delete: `cloudflare/web/src/admin.html`
- Create: `cloudflare/test/admin-static.workerd.test.ts`

**Interfaces:**
- Consumes: complete `cloudflare/web/src/admin/**` tree and existing administrator Worker.
- Produces: same-origin modular asset serving, exact MIME types, no-store, strict CSP, and no legacy monolith.

- [ ] **Step 1: Write failing static/CSP Workerd tests**

```ts
it("serves every administrator module with strict same-origin CSP", async () => {
  const page = await adminWorker.fetch(new Request("https://web.nwflash.cc.cd/"), env);
  expect(page.headers.get("content-type")).toBe("text/html; charset=utf-8");
  expect(page.headers.get("content-security-policy")).not.toContain("unsafe-inline");
  for (const path of ["/admin/styles.css", "/admin/app.js", "/admin/pages/audit.js"]) {
    const asset = await adminWorker.fetch(new Request(`https://web.nwflash.cc.cd${path}`), env);
    expect(asset.status).toBe(200);
    expect(asset.headers.get("cache-control")).toBe("no-store");
  }
});
```

- [ ] **Step 2: Run and verify modular routes fail**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/admin-static.workerd.test.ts
```

Expected: FAIL with `404` assets or old inline CSP.

- [ ] **Step 3: Serve explicit Text-module assets**

Import each HTML/CSS/JS text module explicitly and map exact paths to `{body,mime}`. Serve unknown administrator assets as `404`. Use:

```text
default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none';
object-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:;
font-src 'self'; connect-src 'self'
```

Retain HSTS, nosniff, DENY, no-referrer, Permissions-Policy, and no-store; add `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Resource-Policy: same-origin`.

Give TypeScript an exact raw-text declaration used only by the Worker build:

```ts
declare module "*.html" { const source: string; export default source; }
declare module "*.css" { const source: string; export default source; }
declare module "*.js" { const source: string; export default source; }
```

- [ ] **Step 4: Extend Text module rules and delete the monolith**

Wrangler/Workerd Text rules include `**/*.html`, `**/*.css`, and `**/admin/**/*.js`. Remove `admin.html` only after `/` and every module test passes.

- [ ] **Step 5: Run static, API, and UI shell tests**

```powershell
npm --prefix cloudflare exec -- vitest run --config vitest.workerd.config.ts test/admin-static.workerd.test.ts test/trace-v2-admin.workerd.test.ts
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-shell.spec.ts
```

Expected: PASS.

- [ ] **Step 6: Commit static integration**

```powershell
git add cloudflare/web/src/index.ts cloudflare/web/wrangler.toml cloudflare/vitest.workerd.config.ts cloudflare/web/src/admin cloudflare/test/admin-static.workerd.test.ts cloudflare/package.json
git rm cloudflare/web/src/admin.html
git commit -m "feat(admin): serve modular console with strict CSP"
```

---

### Task 12: Close Accessibility, Responsive, Injection, and Mutation Gates

**Owner:** Admin UI implementer for tests/styles; primary agent integrates fixes

**Files:**
- Create: `cloudflare/web/e2e/admin-accessibility.spec.ts`
- Create: `cloudflare/web/e2e/admin-responsive.spec.ts`
- Create: `cloudflare/web/e2e/admin-mutations.spec.ts`
- Modify: `cloudflare/web/src/admin/styles.css`
- Modify: `cloudflare/web/src/admin/components.js`
- Modify: `cloudflare/package.json`
- Modify: `cloudflare/package-lock.json`
- Modify: `cloudflare/web/README.md`

**Interfaces:**
- Consumes: all six pages and five audit levels.
- Produces: automated WCAG/keyboard/narrow/injection/mutation gates and documented local commands.

- [ ] **Step 1: Write the failing browser matrix**

```ts
for (const width of [320, 360, 768, 1024, 1440]) {
  test(`has no body overflow at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/?view=audit");
    expect(await page.evaluate(() => document.documentElement.scrollWidth === document.documentElement.clientWidth)).toBe(true);
  });
}
```

Add axe WCAG 2 A/AA, keyboard-only six-menu/breadcrumb/pagination/dialog paths, 44px targets, 13px body/12px code, reduced motion, focus trap/restore, no repetitive live announcements, malicious fixture text, mutation duplicate guards, and zero console error/mojibake checks.

- [ ] **Step 2: Run the full browser matrix and capture failures**

```powershell
npm --prefix cloudflare exec -- playwright test --config web/playwright.config.ts admin-accessibility.spec.ts admin-responsive.spec.ts admin-mutations.spec.ts
```

Expected: FAIL on any remaining UI gap.

- [ ] **Step 3: Fix only evidenced accessibility/responsive/state failures**

Do not add decorative UI. Keep filters reachable on narrow screens, constrain code overflow locally, use persistent `role="alert"` and short `role="status"`, restore focus, and honor reduced motion.

- [ ] **Step 4: Add final package scripts and documentation**

```json
{
  "test:admin:unit": "vitest run web/src/admin/tests/*.test.js",
  "test:admin:workerd": "vitest run --config vitest.workerd.config.ts test/admin-static.workerd.test.ts test/trace-v2-admin.workerd.test.ts",
  "test:admin:browser": "playwright test --config web/playwright.config.ts",
  "test:admin": "npm run test:admin:unit && npm run test:admin:workerd && npm run test:admin:browser"
}
```

- [ ] **Step 5: Run all administrator gates**

```powershell
npm --prefix cloudflare run test:admin
```

Expected: PASS.

- [ ] **Step 6: Commit closure tests**

```powershell
git add cloudflare/web/e2e cloudflare/web/src/admin cloudflare/package.json cloudflare/package-lock.json cloudflare/web/README.md
git commit -m "test(admin): verify accessible responsive ops console"
```

---

### Task 13: Run the Website Release Gate and Write Client Plan C Handoff

**Owner:** Primary agent with read-only independent reviewer

**Files:**
- Create: `docs/superpowers/notes/2026-08-24-structured-trace-client-handoff.md`
- Create: `docs/architecture/admin-website-subsystem.md`
- Modify only files required by findings from the independent review.

**Interfaces:**
- Consumes: every previous website task.
- Produces: verified website stage, no deployment, and a precise client producer/spool/uploader handoff. Does not start client work.

- [ ] **Step 1: Run the complete nondeployment gate**

```powershell
npm --prefix cloudflare test
npm --prefix cloudflare run test:workerd
npm --prefix cloudflare run typecheck
npm --prefix cloudflare run test:admin
npm --prefix cloudflare exec -- wrangler deploy --dry-run --outdir .wrangler/website-stage-api
npm --prefix cloudflare exec -- wrangler deploy --dry-run --config web/wrangler.toml --outdir .wrangler/website-stage-web
git diff --check
```

Expected: every command exits `0`; no network deployment occurs.

- [ ] **Step 2: Dispatch one read-only whole-stage reviewer**

Reviewer checks the approved spec and this plan against the diff, authentication on every route, D1 ownership/idempotence, credential-sentinel scans, V1 deduplication/degradation, no mock success evidence, keyboard/focus, narrow layouts, real page states, and forbidden-path changes. The reviewer edits nothing and returns prioritized findings.

- [ ] **Step 3: Fix every confirmed P0/P1 finding and rerun affected tests**

Use the exact failing test command from the finding. Add a regression test before each fix. Do not change `cloudflare/user/**`, `src/Nwflash.Desktop/**`, or `scripts/vmp/**`.

- [ ] **Step 4: Write the exact client Plan C handoff**

The handoff must include:

```text
- POST /api/usage/traces/v2 URL, bearer auth, X-Nwflash-Version gate
- schema version, JSON Schema path, success/failure fixture paths
- UUIDv7/upload/run/event/chunk identity rules
- closed outcomes/event kinds/statuses/output streams
- 1 MiB/20/100/200/32 KiB limits
- item ack and rejected shapes
- 200/400/401/403/409/413/422/500 behavior
- same-user idempotency and cross-user conflict rule
- trace_complete/final_sequence/chunk completeness rule
- credential removal boundary and operational-field preservation
- retry rule: delete only accepted IDs; keep rejected/unacknowledged tail
- V1 projection and administrator query endpoints
- explicit prohibition on modifying website contract without a version bump
```

- [ ] **Step 5: Write the administrator website subsystem architecture**

Create `docs/architecture/admin-website-subsystem.md` with exactly these scopes:

```text
1. Purpose and ownership boundary (`cloudflare/web` plus website trace contract only)
2. Browser module tree and one responsibility per module
3. Admin Worker route split, static asset delivery, CSP, no-store, and security headers
4. D1 V1/V2 tables, indexes, retention windows, projection and deduplication
5. Bearer upload authentication versus administrator session-cookie query authentication
6. V2 flow: upload contract → credential boundary → D1 transaction/ack → admin keyset queries → five-level UI
7. Browser URL router, page lifecycle, loading/empty/partial/stale/error/retry and mutation states
8. Node/Workerd/Playwright/axe test layers and the exact nondeployment release gate
9. Deployment boundary: schema/API before UI, dry-run only in this task, client producer deferred to Plan C
```

Do not describe the desktop application, user portal, VMP, release pipeline, or whole-project architecture beyond the interface boundary required to explain the website.

- [ ] **Step 6: Safely remove only task-generated temporary/build/preview output**

First configure Playwright output under the exact task-owned directory `cloudflare/web/.artifacts/admin-website/`. After tests and review, resolve each candidate and verify it is inside the isolated worktree and one of these roots:

```text
cloudflare/.wrangler/website-stage-api
cloudflare/.wrangler/website-stage-web
cloudflare/web/.artifacts/admin-website/test-results
cloudflare/web/.artifacts/admin-website/playwright-report
```

Use this guarded PowerShell shape from the isolated worktree root:

```powershell
$taskRoot = (Resolve-Path '.').Path
$relativeTargets = @(
  'cloudflare\.wrangler\website-stage-api',
  'cloudflare\.wrangler\website-stage-web',
  'cloudflare\web\.artifacts\admin-website\test-results',
  'cloudflare\web\.artifacts\admin-website\playwright-report'
)
$approvedTargets = $relativeTargets | ForEach-Object { [IO.Path]::GetFullPath((Join-Path $taskRoot $_)) }
foreach ($relativeTarget in $relativeTargets) {
  $candidate = Join-Path $taskRoot $relativeTarget
  if (-not (Test-Path -LiteralPath $candidate)) { continue }
  $resolved = (Resolve-Path -LiteralPath $candidate).Path
  if (-not $resolved.StartsWith($taskRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove path outside task worktree: $resolved"
  }
  if ($resolved -notin $approvedTargets) { throw "Unapproved cleanup target: $resolved" }
  Remove-Item -LiteralPath $resolved -Recurse -Force
}
```

Then run `git status --short` and inspect every remaining untracked path. Do not use `git clean`; do not delete `.superpowers/sdd/**`, browser brainstorm sessions, dependency caches, unknown untracked files, or any path outside the four exact targets.

- [ ] **Step 7: Rerun the complete gate after review fixes and cleanup**

Run the Step 1 command block again. Expected: all exit `0`.

- [ ] **Step 8: Commit website acceptance, subsystem architecture, and handoff**

```powershell
git add cloudflare docs/architecture/admin-website-subsystem.md docs/superpowers/notes/2026-08-24-structured-trace-client-handoff.md
git commit -m "docs: hand off structured trace v2 client contract"
```

The final response notifies the root coordination task that the website gate and handoff are complete. Do not begin Rust/Tauri producer, spool, or uploader changes in this task.
