# Structured trace V2 Plan C handoff

This handoff freezes the P0 multi-request upload protocol for the future desktop Plan C. It does not authorize client implementation or deployment in the website stage.

## Endpoint and frozen artifacts

- Upload: `POST /api/usage/traces/v2` with `Authorization: Bearer <token>`, `X-Nwflash-Version`, `Content-Type: application/json`.
- Schema: `cloudflare/contracts/trace-v2/usage-trace-v2.schema.json`, `schema_version = 2`.
- Canonical fixtures: `upload.success.json`, `upload.failed.json`, and `upload-ack.success.json`.
- Multi-request chain: `upload.open.json` → `upload.event-only.json` → `upload.chunk-only.json` → `upload.finalize-only.json`.
- `upload_id`, `run_id`, `event_id`, and `chunk_id` are lowercase UUIDv7. Generate a fresh `upload_id` for every HTTP request and every retry attempt; it is request correlation, not item identity. Item IDs remain stable across retries.
- `run_id`, `event_id`, and `chunk_id` are globally unique. Same-user duplicate IDs are idempotent only when every persisted semantic field and natural key is identical.

## Closed enums

- Run outcome: `running`, `success`, `failed`, `canceled`, `denied`, `aborted`, `unknown`.
- Event kind: `authorization`, `stage`, `partition`, `command`, `skip`, `verification`, `terminal`.
- Event status: `started`, `success`, `failed`, `canceled`, `skipped`, `unknown`.
- Output stream: `stdout`, `stderr`.

Any value outside these exact sets is a 400 `TRACE_INVALID`; clients must not invent aliases such as `completed`.

## Request composition and spool deletion

Each HTTP request is independently bounded to 1 MiB, 20 runs, 100 events, 200 chunks, and 32 KiB UTF-8 per chunk. These are request limits; logical output may exceed 1 MiB across requests. Independently of request grouping, one logical run is capped at 100 events (`event.sequence` and non-null `run.final_sequence` are in `1..100`) and 8,388,608 UTF-8 bytes of persisted event metadata.

The 8 MiB run quota uses the same byte scope in TypeScript and D1: UTF-8 bytes of `event_id`, `run_id`, event kind/name/partition/status, command program/argv/display/working directory/paths/URLs/serial, verification, device state, remedies, error class/code/message, and credential-redaction JSON. Integer/timestamp/chunk-output columns are outside this metadata quota and have their own frozen bounds. An event that would cross either logical-run ceiling is item-level `invalid`; its fresh descendants are `missing_parent`. The administrator run-detail response is consequently bounded to at most 100 events and 8 MiB of this metadata before separately paginated output reads.

An accepted parent item may be deleted from the client spool immediately. Later requests do not resend it:

- An event-only request uses `runs: []` and carries `event.run_id` for the previously accepted run.
- A chunk-only request uses `runs: []`, `events: []`, and carries `chunk.event_id` for the previously accepted event.
- A finalize-only request carries the terminal run and may leave `events`/`output_chunks` empty.

Delete only IDs returned in `accepted`. Keep every rejected or unacknowledged item for explicit retry/remediation. Never infer acceptance from HTTP 200 alone.

Plan C MUST cap local spool retention at seven days. When an item reaches that limit, discard the expired spool data, record an explicit durable loss diagnostic for the affected trace, and stop retrying it; do not attempt to revive an old server trace from discarded evidence.

The exact successful response shape is:

```json
{
  "ok": true,
  "accepted": {
    "runs": ["<run_id>"],
    "events": ["<event_id>"],
    "output_chunks": ["<chunk_id>"]
  },
  "rejected": [
    {
      "entity": "run | event | output_chunk",
      "id": "<item-id-or-null>",
      "code": "invalid | missing_parent | sequence_conflict | credential_rejected",
      "message": "<human-readable message>"
    }
  ]
}
```

All three `accepted` arrays and `rejected` are always present. A 200 response can contain both accepted and rejected items. The frozen rejected-code enum also contains `incomplete_trace`, but current server behavior emits it only inside a 422 error's `details`, never as a 200 item rejection. API-level errors use the separate exact envelope:

```json
{
  "ok": false,
  "error": {
    "code": "TRACE_INVALID",
    "message": "<safe message>",
    "request_id": "<server UUID>",
    "details": []
  }
}
```

`details` is optional; the other fields are required. The pre-ingest 426 app-version gate is the sole exception: it keeps the legacy shape `{ "error", "code": "UPDATE_REQUIRED", "latest", "min", "download_url" }` and does not use `TraceApiErrorV2` or add `UPDATE_REQUIRED` to its enum.

## Logical totals, gaps, retries, and completion

`event.stdout_chunks` and `event.stderr_chunks` are logical stream totals, not counts for the current request, and each stream total is limited to 200. While a run is open, any unique in-range subset is valid and gaps are allowed. Each fresh chunk must satisfy `0 <= chunk_index < declared_total`; same `(event_id, stream, chunk_index)` with another ID is a conflict. Retrying the same chunk ID requires the same `event_id`, stream, index, text, `byte_count`, and SHA-256. Redaction metadata is derived deterministically by the server and is not an `outputChunk` wire field.

Same-user idempotency compares complete persisted semantics, not merely IDs. An event duplicate must retain its run parent, sequence, metadata, command fields, declared totals, error fields, remedies, and redaction counts. A chunk duplicate must retain event parent, stream, index, post-redaction text, byte count, SHA-256, and redaction counts. A run duplicate must retain client-authored immutable fields; the only mutation is the controlled open-to-terminal transition. Initial server-derived user name and source IP are preserved rather than compared with the current request.

Only `trace_complete=true` is strict. It requires a non-`running` terminal outcome plus a non-null `final_sequence`. The atomic D1 transaction must observe exactly sequences `1..final_sequence` and, for every event stream, exactly indexes `0..declared_total-1` across persisted plus current items. Incomplete evidence returns 422 and writes nothing from that request.

## Status catalogue and execution order

The server executes gates in this order: app-version gate → bearer authentication and ban check → bounded body/contract validation → foreign entity/parent ownership fail-closed check → persisted exact durable acknowledgement → parent/ancestor validation → completed-run rejection → cross-raw-chunk credential rejection → declared bounds → natural-tuple conflict → atomic finalization validation.

1. `200`: structurally valid request; exact duplicates and item-level `missing_parent`/`invalid`/`sequence_conflict`/`credential_rejected` rejections use the success envelope and only accepted IDs may be deleted. If a parent item carried in the same request is rejected, every fresh descendant is `missing_parent`. A new event beyond an accepted open run's known non-null `final_sequence` is `invalid`; appends to a completed run are `sequence_conflict` first.
2. `400 TRACE_INVALID`: malformed JSON, wrong content type, unknown fields, invalid enums/UUIDs/numbers, duplicate local IDs/tuples, or byte-count/SHA mismatch.
3. `401 TRACE_UNAUTHORIZED`: missing, invalid, disabled, or expired bearer authentication.
4. `403 TRACE_FORBIDDEN`: authenticated but banned/forbidden user.
5. `426` legacy `UPDATE_REQUIRED`: app version is below the minimum; this pre-auth gate uses the legacy version-policy shape described above.
6. `409 TRACE_OWNERSHIP_CONFLICT`: any uploaded entity or referenced parent belongs to another user; the entire request atomically writes nothing.
7. `413 TRACE_BODY_TOO_LARGE`: request body exceeds 1 MiB.
8. `422 TRACE_INCOMPLETE`: requested finalization lacks complete persisted-plus-current sequences or stream chunks; the request writes nothing. If finalization also rejected cross-boundary chunks, `error.details` contains the run `incomplete_trace` plus each chunk `credential_rejected`; there is no `accepted` object and the client must delete no IDs from that request.
9. `500 TRACE_INTERNAL`: unclassified D1/internal failure; response details never include trace content or credentials.

## Credential threat model

Plan C MUST synchronously streaming-redact each complete logical stdout/stderr stream before any spool write, HTTP body construction, or chunk split. The scanner must retain state across process read buffers and proposed chunk boundaries. Plan C tests must place bearer, PEM, CLI assignment, cookie, URL credential, and exact-secret sentinels across those boundaries and prove zero matches in the client spool and serialized bodies. If V2 returns `credential_rejected`, keep all affected chunk IDs, redact the complete logical stream locally, recompute each chunk's text/byte count/hash, and retry before finalization.

The server remains defense-in-depth. A credential wholly inside one raw chunk is deterministically removed and the server recomputes UTF-8 `byte_count` and SHA-256, so regrouped retries remain idempotent. For contiguous same-event/stream indexes in one request, the server compares whole-group matching with concatenated per-chunk matching; a cross-raw-boundary difference rejects the entire contiguous group as `credential_rejected` before D1. Non-contiguous indexes are never joined. The V2 server does not promise detection when a hostile client deliberately splits any credential across separate HTTP requests, including fragments of an Authorization bearer already known to the server. A stronger server-side guarantee requires a future V3 encrypted-pending protocol and separate migration; administrators and UI copy must not claim absolute cross-request removal.

## D1 and compatibility

- Apply `migrate-usage-traces-v2.sql`, then `migrate-usage-traces-v2-p0.sql`, then the one-time `migrate-usage-traces-v2-retention-stage.sql`. The last migration upgrades existing V2 tables with internal retention markers and partial indexes; these columns are server-only and do not change the V2 wire contract. All three migrations must complete successfully before the API deployment receives upload traffic.
- Parent ownership is checked before ingestion and again inside the atomic batch guard. D1 triggers reject missing/completed parents, event count/storage quota violations, and out-of-range indexes.
- Terminal V2 runs project one compatibility `usage_logs` summary with `event_key = run_id` in the same transaction. D1-only `source_schema = 2` and owner-bound `trace_run_id` provenance identify that row; they are not wire fields. Historical V1 rows remain `source_schema = 1`, and a V1 event key may coexist with the same V2 run ID without being hidden or deleted.
- The V1 projection contains the terminal user/name, operation kind, title, outcome, start/end, and duration summary. Administrator dual-read suppresses that projected V1 row while its V2 run exists; historical V1-only rows remain available with explicit legacy detail degradation.
- Retention drains backlog progressively with stable ordered batches of at most 100 rows per D1 mutation. Thirty-day detail candidates seek partial indexes containing only rows whose internal `retention_detail_cleared` marker is false; clearing detail atomically sets the marker, so later cron runs do not rescan clean history. A 30-day open run is permanently detail-sealed: its marker is never reset by an open-to-terminal attempt, and later run finalization, identity/detail mutation, event upload, or chunk retry is rejected as item-level `invalid` with a `retention_expired`/detail-sealed explanation. It remains unfinalized and produces no V1 projection. When a V2 run crosses 180 days, retention removes only that run's `usage_logs.event_key = run_id` compatibility projection before deleting the run; unrelated historical V1 rows remain intact and cannot reappear as a false V1 duplicate.

The seven administrator trace query endpoints, all protected by the administrator session cookie, are:

1. `GET /api/usage-logs/v2/users?from&to&status&q&limit&cursor`
2. `GET /api/usage-logs/v2/runs?userId&kind&status&from&to&partition&errorCode&q&limit&cursor`
3. `GET /api/usage-logs/v2/runs/{traceRef}`
4. `GET /api/usage-logs/v2/runs/{traceRef}/events/{eventId}`
5. `GET /api/usage-logs/v2/runs/{traceRef}/events/{eventId}/output?stream&afterChunk&limit`
6. `GET /api/usage-logs/v2/overview?from&to&bucket=hour`
7. `GET /api/usage-logs/v2/export?<same filters as runs>`

The additional authoritative administrator summaries are `GET /api/app-versions/summary` and `GET /api/rom-logs/v2?userId&pd&version&status&q&limit&cursor`.

The JSON Schema, TypeScript interfaces, fixtures, upload behavior, response shapes, and administrator query shapes are one frozen V2 contract. Any incompatible field, enum, identity, status, or semantic change requires a schema-version increase and new fixtures; never silently reinterpret V2.

## Rollout and rollback boundary

The website release operator must take and verify a D1 recovery point before applying base → P0 → retention-stage. A failed migration stops the sequence and keeps V2 traffic disabled; recovery is a reviewed forward repair or restoration of that recovery point, never ad-hoc reverse SQL. If migrations succeed but API verification fails, roll back only to a Worker build proven compatible with the additive schema, or quiesce traffic and restore D1 and Worker versions together. If only the modular UI fails, retain the schema/API and restore the previous website Worker.

After any V2 acknowledgement has been issued, preserve current D1 and prefer a forward repair or a schema-compatible Worker rollback. An older D1 recovery point is no longer a lossless rollback because clients may immediately delete accepted spool IDs. If disaster recovery unavoidably restores behind issued acknowledgements, it requires explicit incident authorization, a recorded affected acknowledgement window, and an explicit permanent trace-loss record; the system must not claim complete audit history for that window. A dedicated synthetic rollout trace and its accepted IDs may be retained separately through the observation window for reconciliation, without weakening normal spool-deletion semantics. Production remains blocked until an authorized operator confirms the D1 binding, domain, required secrets, administrator authentication, the synthetic upload/query smoke path, and a clear rollback stop-condition record. Plan C implementation and production deployment remain separately authorized work.
