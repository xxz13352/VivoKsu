import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import worker, { type Env as WorkerEnv } from "../src/index";
import { purgeExpiredTraceData } from "../src/trace-v2-retention";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const DAY_MS = 24 * 60 * 60 * 1_000;
const FIXED_NOW_MS = Date.UTC(2026, 7, 26, 12, 0, 0);
const RETENTION_BATCH_LIMIT = 100;

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("trace V2 retention", () => {
  it("seeks only indexed pending detail after a large clean history", async () => {
    await seedCleanRetentionHistory(250, 31);
    const pending = await seedTrace("pending-indexed-detail", 31);
    const preparedSql: string[] = [];

    const first = await purgeExpiredTraceData(recordingDatabase(preparedSql), FIXED_NOW_MS);

    expect(first.sensitive_fields_cleared).toBe(2);
    expect(await scalarCount(
      "SELECT retention_detail_cleared AS value FROM usage_operation_runs WHERE run_id = ?",
      pending.runId,
    )).toBe(1);
    expect(await scalarCount(
      "SELECT retention_detail_cleared AS value FROM usage_operation_events WHERE event_id = ?",
      pending.eventId,
    )).toBe(1);
    const runUpdate = preparedSql.find((sql) => sql.includes("UPDATE usage_operation_runs")) ?? "";
    const eventUpdate = preparedSql.find((sql) => sql.includes("UPDATE usage_operation_events")) ?? "";
    expect(runUpdate).toContain("run.retention_detail_cleared = 0");
    expect(eventUpdate).toContain("event.retention_detail_cleared = 0");
    expect(runUpdate).not.toContain("device_serial IS NOT NULL");
    expect(eventUpdate).not.toContain("command_program IS NOT NULL");

    const runPlan = await queryPlan(
      `SELECT run_id FROM usage_operation_runs
       WHERE retention_detail_cleared = 0 AND started_at_ms < ?
       ORDER BY started_at_ms ASC, run_id ASC LIMIT ?`,
      FIXED_NOW_MS - 30 * DAY_MS,
      RETENTION_BATCH_LIMIT,
    );
    expect(runPlan).toContain("idx_trace_runs_retention_detail_pending");
    expect(runPlan).not.toContain("SCAN usage_operation_runs");
    const eventPlan = await queryPlan(
      `SELECT event.event_id
       FROM usage_operation_events AS event
       JOIN usage_operation_runs AS run ON run.run_id = event.run_id
       WHERE event.retention_detail_cleared = 0 AND run.started_at_ms < ?
       ORDER BY run.started_at_ms ASC, run.run_id ASC, event.sequence ASC, event.event_id ASC
       LIMIT ?`,
      FIXED_NOW_MS - 30 * DAY_MS,
      RETENTION_BATCH_LIMIT,
    );
    expect(eventPlan).toContain("idx_trace_events_retention_detail_pending");

    const second = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);
    expect(second.sensitive_fields_cleared).toBe(0);
  });

  it("bounds child-table reads when twenty thousand young traces miss every cutoff", async () => {
    await seedYoungTraceCorpus(20_000, FIXED_NOW_MS - DAY_MS);
    const preparedSql: string[] = [];
    const batchResults: D1Result<unknown>[] = [];

    const result = await purgeExpiredTraceData(
      recordingDatabase(preparedSql, batchResults),
      FIXED_NOW_MS,
    );

    expect(result).toMatchObject({
      output_chunks_deleted: 0,
      sensitive_fields_cleared: 0,
      events_deleted: 0,
      runs_deleted: 0,
    });
    for (const statement of [preparedSql[0], preparedSql[2], preparedSql[3]]) {
      expect(statement).toContain("candidate_runs AS MATERIALIZED");
      expect(statement).toContain("INDEXED BY idx_trace_runs_time");
      expect(statement).toContain("CROSS JOIN usage_operation_events");
    }
    const outputPlan = await queryPlan(
      preparedSql[0],
      FIXED_NOW_MS - 30 * DAY_MS,
      RETENTION_BATCH_LIMIT,
    );
    const eventDetailPlan = await queryPlan(
      preparedSql[2],
      FIXED_NOW_MS - 30 * DAY_MS,
      RETENTION_BATCH_LIMIT,
    );
    const eventDeletePlan = await queryPlan(
      preparedSql[3],
      FIXED_NOW_MS - 90 * DAY_MS,
      RETENTION_BATCH_LIMIT,
    );
    for (const plan of [outputPlan, eventDetailPlan, eventDeletePlan]) {
      expect(plan).toContain("idx_trace_runs_time");
      expect(plan).not.toMatch(/SCAN (?:chunk|event|usage_output_chunks|usage_operation_events)\b/);
    }
    expect(outputPlan).toContain("idx_trace_events_run_seq");
    expect(outputPlan).toContain("idx_trace_output_event_stream");
    expect(eventDetailPlan).toContain("idx_trace_events_retention_detail_pending");
    expect(eventDeletePlan).toContain("idx_trace_events_run_seq");
    expect(batchResults.reduce(
      (total, item) => total + Number(item.meta.rows_read ?? 0),
      0,
    )).toBeLessThan(500);
  }, 30_000);

  it("processes operational detail in stable batches of one hundred rows", async () => {
    const traces = await seedTraceBatch("detail-batch", RETENTION_BATCH_LIMIT + 1, 31);

    const first = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(first).toMatchObject({
      output_chunks_deleted: RETENTION_BATCH_LIMIT,
      sensitive_fields_cleared: RETENTION_BATCH_LIMIT * 2,
      events_deleted: 0,
      runs_deleted: 0,
    });
    expect(await count("usage_output_chunks")).toBe(1);
    expect(await operationalRunCount()).toBe(1);
    expect(await operationalEventCount()).toBe(1);
    expect(await runRow(traces.at(-1)!.runId)).toMatchObject({
      device_serial: traces.at(-1)!.marker,
    });

    const second = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(second).toMatchObject({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 0,
      runs_deleted: 0,
    });
    expect(await count("usage_output_chunks")).toBe(0);
    expect(await operationalRunCount()).toBe(0);
    expect(await operationalEventCount()).toBe(0);
  });

  it("clears overdue detail while draining chunks before deleting their expired trace", async () => {
    const trace = await seedTrace("chunk-batch", 181);
    await env.DB.prepare(
      "UPDATE usage_operation_events SET stdout_chunks = ? WHERE event_id = ?",
    ).bind(RETENTION_BATCH_LIMIT + 1, trace.eventId).run();
    const extraChunks = Array.from({ length: RETENTION_BATCH_LIMIT }, (_, index) => {
      const chunkIndex = index + 1;
      return env.DB.prepare(
        `INSERT INTO usage_output_chunks
           (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256,
            credential_redactions_json)
         VALUES (?, ?, 'stdout', ?, ?, ?, ?, '[]')`,
      ).bind(
        `chunk-chunk-batch-${chunkIndex.toString().padStart(3, "0")}`,
        trace.eventId,
        chunkIndex,
        `output-${chunkIndex}`,
        new TextEncoder().encode(`output-${chunkIndex}`).byteLength,
        "b".repeat(64),
      );
    });
    for (let offset = 0; offset < extraChunks.length; offset += 80) {
      await env.DB.batch(extraChunks.slice(offset, offset + 80));
    }

    const first = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(first).toMatchObject({
      output_chunks_deleted: RETENTION_BATCH_LIMIT,
      sensitive_fields_cleared: 2,
      events_deleted: 0,
      runs_deleted: 0,
    });
    expect(await count("usage_output_chunks")).toBe(1);
    expect(await rowCount("usage_operation_events", "event_id", trace.eventId)).toBe(1);
    expect(await runRow(trace.runId)).toMatchObject({ device_serial: null });
    expect(await eventRow(trace.eventId)).toMatchObject({ command_program: null });

    const second = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(second).toMatchObject({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 0,
      events_deleted: 1,
      runs_deleted: 1,
    });
    expect(await count("usage_output_chunks")).toBe(0);
    expect(await rowCount("usage_operation_events", "event_id", trace.eventId)).toBe(0);
    expect(await rowCount("usage_operation_runs", "run_id", trace.runId)).toBe(0);
  });

  it("progressively deletes expired V2 projections without deleting unrelated legacy logs", async () => {
    const traces = await seedTraceBatch("expired-batch", RETENTION_BATCH_LIMIT + 1, 181, true);
    await env.DB.prepare(
      `INSERT INTO usage_logs
         (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
       VALUES (7, 'User 7', 'legacy', 'Unrelated legacy run', 'failed', 'legacy-unrelated', ?)`,
    ).bind(Math.floor((FIXED_NOW_MS - 181 * DAY_MS) / 1_000)).run();

    const first = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(first).toMatchObject({
      output_chunks_deleted: RETENTION_BATCH_LIMIT,
      sensitive_fields_cleared: RETENTION_BATCH_LIMIT * 2,
      events_deleted: RETENTION_BATCH_LIMIT,
      runs_deleted: RETENTION_BATCH_LIMIT,
    });
    expect(await count("usage_operation_runs")).toBe(1);
    expect(await count("usage_operation_events")).toBe(1);
    expect(await count("usage_output_chunks")).toBe(1);
    expect(await rowCount("usage_operation_runs", "run_id", traces.at(-1)!.runId)).toBe(1);
    expect(await projectedLogCount("run-expired-batch-")).toBe(1);
    expect(await rowCount("usage_logs", "event_key", traces.at(-1)!.runId)).toBe(1);
    expect(await rowCount("usage_logs", "event_key", "legacy-unrelated")).toBe(1);

    const second = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(second).toMatchObject({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 1,
      runs_deleted: 1,
    });
    expect(await count("usage_operation_runs")).toBe(0);
    expect(await count("usage_operation_events")).toBe(0);
    expect(await count("usage_output_chunks")).toBe(0);
    expect(await projectedLogCount("run-expired-batch-")).toBe(0);
    expect(await rowCount("usage_logs", "event_key", "legacy-unrelated")).toBe(1);

    const third = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);
    expect(third).toMatchObject({
      output_chunks_deleted: 0,
      sensitive_fields_cleared: 0,
      events_deleted: 0,
      runs_deleted: 0,
    });
    expect(await rowCount("usage_logs", "event_key", "legacy-unrelated")).toBe(1);
  });

  it("deletes only the owner-bound V2 projection when a V1 row has the same event key", async () => {
    const runId = "019d9c40-7b3c-7000-8000-000000000188";
    const startedAtMs = FIXED_NOW_MS - 181 * DAY_MS;
    await env.DB.batch([
      env.DB.prepare(
        `INSERT INTO usage_operation_runs
           (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
            client_version, started_at_ms, ended_at_ms, duration_ms, final_sequence, trace_complete,
            retention_detail_cleared)
         VALUES (?, 7, 'User 7', 2, 'retention', 'Expired V2', 'success', '1.4.0', ?, ?, 1, 1, 1, 1)`,
      ).bind(runId, startedAtMs, startedAtMs + 1),
      env.DB.prepare(
        `INSERT INTO usage_logs
           (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (8, 'Legacy user', 'legacy', 'Colliding V1 history', 'failed', ?, ?, 1, NULL)`,
      ).bind(runId, Math.floor(startedAtMs / 1_000)),
      env.DB.prepare(
        `INSERT INTO usage_logs
           (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (7, 'User 7', 'retention', 'Expired V2', 'success', ?, ?, 2, ?)`,
      ).bind(runId, Math.floor(startedAtMs / 1_000), runId),
    ]);

    const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);
    const remaining = await env.DB.prepare(
      "SELECT api_user_id, source_schema, trace_run_id FROM usage_logs WHERE event_key = ?",
    ).bind(runId).all<{ api_user_id: number; source_schema: number; trace_run_id: string | null }>();

    expect(result.runs_deleted).toBe(1);
    expect(await rowCount("usage_operation_runs", "run_id", runId)).toBe(0);
    expect(remaining.results).toEqual([{ api_user_id: 8, source_schema: 1, trace_run_id: null }]);
  });

  it("clears operational detail after thirty days but preserves run and event metadata", async () => {
    const trace = await seedTrace("thirty-one", 31);

    const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(result).toEqual({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 0,
      runs_deleted: 0,
      cutoff_30d_ms: FIXED_NOW_MS - 30 * DAY_MS,
      cutoff_90d_ms: FIXED_NOW_MS - 90 * DAY_MS,
      cutoff_180d_ms: FIXED_NOW_MS - 180 * DAY_MS,
    });
    expect(await runRow(trace.runId)).toMatchObject({
      title: "Summary thirty-one",
      outcome: "success",
      device_serial: null,
      source_ip: null,
      source_paths_json: "[]",
      source_urls_json: "[]",
      error_message: null,
      credential_redactions_json: "[]",
    });
    expect(await eventRow(trace.eventId)).toMatchObject({
      step_name: "Step thirty-one",
      partition_name: "boot_a",
      status: "success",
      command_program: null,
      command_argv_json: null,
      command_line: null,
      working_directory: null,
      paths_json: "[]",
      urls_json: "[]",
      serial: null,
      verification: null,
      device_state: null,
      remedies_json: "[]",
      error_message: null,
      credential_redactions_json: "[]",
    });
    expect(await count("usage_output_chunks")).toBe(0);
    expect(await databaseContains(trace.marker)).toBe(false);
  });

  it("deletes event metadata after ninety days while retaining a cleared run summary", async () => {
    const trace = await seedTrace("ninety-one", 91);

    const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(result).toMatchObject({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 1,
      runs_deleted: 0,
    });
    expect(await count("usage_operation_runs")).toBe(1);
    expect(await runRow(trace.runId)).toMatchObject({
      title: "Summary ninety-one",
      device_serial: null,
      source_ip: null,
      source_paths_json: "[]",
      source_urls_json: "[]",
    });
    expect(await count("usage_operation_events")).toBe(0);
    expect(await count("usage_output_chunks")).toBe(0);
    expect(await databaseContains(trace.marker)).toBe(false);
  });

  it("deletes runs and all subordinate trace data after one hundred eighty days", async () => {
    await seedTrace("one-eighty-one", 181);

    const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(result).toMatchObject({
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 1,
      runs_deleted: 1,
    });
    expect(await count("usage_operation_runs")).toBe(0);
    expect(await count("usage_operation_events")).toBe(0);
    expect(await count("usage_output_chunks")).toBe(0);
  });

  it("preserves younger traces and reports zero changes on an already-purged database", async () => {
    const young = await seedTrace("young", 29);
    const expired = await seedTrace("expired", 31);

    await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);
    const second = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(second).toMatchObject({
      output_chunks_deleted: 0,
      sensitive_fields_cleared: 0,
      events_deleted: 0,
      runs_deleted: 0,
    });
    expect(await databaseContains(young.marker)).toBe(true);
    expect(await databaseContains(expired.marker)).toBe(false);
    expect(await count("usage_operation_runs")).toBe(2);
    expect(await count("usage_operation_events")).toBe(2);
    expect(await count("usage_output_chunks")).toBe(1);
  });

  it("preserves exact retention cutoffs and purges rows one millisecond older", async () => {
    const exact30 = await seedTraceAtAgeMs("exact-thirty", 30 * DAY_MS);
    const older30 = await seedTraceAtAgeMs("older-thirty", 30 * DAY_MS + 1);
    const exact90 = await seedTraceAtAgeMs("exact-ninety", 90 * DAY_MS);
    const older90 = await seedTraceAtAgeMs("older-ninety", 90 * DAY_MS + 1);
    const exact180 = await seedTraceAtAgeMs("exact-one-eighty", 180 * DAY_MS);
    const older180 = await seedTraceAtAgeMs("older-one-eighty", 180 * DAY_MS + 1);

    const result = await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    expect(result).toMatchObject({
      output_chunks_deleted: 5,
      sensitive_fields_cleared: 10,
      events_deleted: 3,
      runs_deleted: 1,
    });
    expect(await databaseContains(exact30.marker)).toBe(true);
    expect(await databaseContains(older30.marker)).toBe(false);
    expect(await rowCount("usage_operation_events", "event_id", exact90.eventId)).toBe(1);
    expect(await rowCount("usage_operation_events", "event_id", older90.eventId)).toBe(0);
    expect(await rowCount("usage_operation_runs", "run_id", exact180.runId)).toBe(1);
    expect(await rowCount("usage_operation_runs", "run_id", older180.runId)).toBe(0);
  });

  it("runs retention from scheduled and logs only counts and cutoffs", async () => {
    const trace = await seedTrace("scheduled-secret", 31);
    const nowSeconds = Math.floor(FIXED_NOW_MS / 1_000);
    await seedScheduledCleanupRows(nowSeconds);
    vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS);
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);

    await worker.scheduled({} as ScheduledEvent, env, {} as ExecutionContext);

    expect(await databaseContains(trace.marker)).toBe(false);
    expect(await rowCount("online_sessions", "session_id", "stale-session")).toBe(0);
    expect(await rowCount("session_leases", "session_id", "stale-session")).toBe(0);
    expect(await rowCount("integrity_rate_limits", "ip_hash", "stale-ip-hash")).toBe(0);
    expect(await rowCount("online_sessions", "session_id", "fresh-session")).toBe(1);
    expect(await rowCount("session_leases", "session_id", "fresh-session")).toBe(1);
    expect(await rowCount("integrity_rate_limits", "ip_hash", "fresh-ip-hash")).toBe(1);
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith("trace-v2-retention", {
      output_chunks_deleted: 1,
      sensitive_fields_cleared: 2,
      events_deleted: 0,
      runs_deleted: 0,
      cutoff_30d_ms: FIXED_NOW_MS - 30 * DAY_MS,
      cutoff_90d_ms: FIXED_NOW_MS - 90 * DAY_MS,
      cutoff_180d_ms: FIXED_NOW_MS - 180 * DAY_MS,
    });
    expect(JSON.stringify(log.mock.calls)).not.toContain(trace.marker);
  });
});

async function seedTrace(key: string, ageDays: number): Promise<{
  runId: string;
  eventId: string;
  marker: string;
}> {
  return seedTraceAtAgeMs(key, ageDays * DAY_MS);
}

async function seedTraceAtAgeMs(key: string, ageMs: number): Promise<{
  runId: string;
  eventId: string;
  marker: string;
}> {
  const runId = `run-${key}`;
  const eventId = `event-${key}`;
  const chunkId = `chunk-${key}`;
  const marker = `operational-marker-${key}`;
  const startedAtMs = FIXED_NOW_MS - ageMs;
  await env.DB.prepare(
    `INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        device_serial, source_ip, source_paths_json, source_urls_json, client_version,
        started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
        final_sequence, trace_complete, trace_loss_reason, credential_redactions_json)
     VALUES (?, 7, 'User 7', 2, 'fastboot_flash', ?, 'success', ?, '203.0.113.45', ?, ?,
             '1.4.0', ?, ?, 2500, 'OperationError', 'FAILED', ?, 1, 0, NULL, ?)`,
  ).bind(
    runId,
    `Summary ${key}`,
    marker,
    JSON.stringify([`C:\\private\\${marker}.img`]),
    JSON.stringify([`https://example.invalid/${marker}`]),
    startedAtMs,
    startedAtMs + 2_500,
    marker,
    JSON.stringify([{ kind: marker, count: 1 }]),
  ).run();
  await env.DB.prepare(
    `INSERT INTO usage_operation_events
       (event_id, run_id, sequence, event_kind, step_name, partition_name, status,
        started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
        command_line, working_directory, paths_json, urls_json, serial, exit_code,
        stdout_chunks, stderr_chunks, verification, device_state, retry_safe, remedies_json,
        error_class, error_code, error_message, credential_redactions_json)
     VALUES (?, ?, 1, 'command', ?, 'boot_a', 'success', ?, ?, 2000, ?, ?, ?, ?, ?, ?, ?, 0,
             1, 0, ?, ?, 1, ?, 'CommandError', 'FAILED', ?, ?)`,
  ).bind(
    eventId,
    runId,
    `Step ${key}`,
    startedAtMs,
    startedAtMs + 2_000,
    marker,
    JSON.stringify(["flash", marker]),
    marker,
    marker,
    JSON.stringify([`C:\\private\\${marker}.img`]),
    JSON.stringify([`https://example.invalid/${marker}`]),
    marker,
    marker,
    marker,
    JSON.stringify([marker]),
    marker,
    JSON.stringify([{ kind: marker, count: 1 }]),
  ).run();
  await env.DB.prepare(
    `INSERT INTO usage_output_chunks
       (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256, credential_redactions_json)
     VALUES (?, ?, 'stdout', 0, ?, ?, ?, ?)`,
  ).bind(
    chunkId,
    eventId,
    marker,
    new TextEncoder().encode(marker).byteLength,
    "a".repeat(64),
    JSON.stringify([{ kind: marker, count: 1 }]),
  ).run();
  return { runId, eventId, marker };
}

async function seedTraceBatch(
  prefix: string,
  traceCount: number,
  ageDays: number,
  includeProjection = false,
): Promise<Array<{ runId: string; eventId: string; marker: string }>> {
  const startedAtMs = FIXED_NOW_MS - ageDays * DAY_MS;
  const traces = Array.from({ length: traceCount }, (_, index) => {
    const suffix = index.toString().padStart(3, "0");
    return {
      runId: `run-${prefix}-${suffix}`,
      eventId: `event-${prefix}-${suffix}`,
      chunkId: `chunk-${prefix}-${suffix}`,
      marker: `operational-marker-${prefix}-${suffix}`,
    };
  });
  const statements = traces.flatMap((trace) => {
    const statementsForTrace = [
      env.DB.prepare(
        `INSERT INTO usage_operation_runs
           (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
            device_serial, source_ip, source_paths_json, source_urls_json, client_version,
            started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
            final_sequence, trace_complete, trace_loss_reason, credential_redactions_json)
         VALUES (?, 7, 'User 7', 2, 'fastboot_flash', ?, 'success', ?, '203.0.113.45', ?, ?,
                 '1.4.0', ?, ?, 2500, 'OperationError', 'FAILED', ?, 1, 0, NULL, ?)`,
      ).bind(
        trace.runId,
        `Summary ${trace.runId}`,
        trace.marker,
        JSON.stringify([`C:\\private\\${trace.marker}.img`]),
        JSON.stringify([`https://example.invalid/${trace.marker}`]),
        startedAtMs,
        startedAtMs + 2_500,
        trace.marker,
        JSON.stringify([{ kind: trace.marker, count: 1 }]),
      ),
      env.DB.prepare(
        `INSERT INTO usage_operation_events
           (event_id, run_id, sequence, event_kind, step_name, partition_name, status,
            started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
            command_line, working_directory, paths_json, urls_json, serial, exit_code,
            stdout_chunks, stderr_chunks, verification, device_state, retry_safe, remedies_json,
            error_class, error_code, error_message, credential_redactions_json)
         VALUES (?, ?, 1, 'command', ?, 'boot_a', 'success', ?, ?, 2000, ?, ?, ?, ?, ?, ?, ?, 0,
                 1, 0, ?, ?, 1, ?, 'CommandError', 'FAILED', ?, ?)`,
      ).bind(
        trace.eventId,
        trace.runId,
        `Step ${trace.runId}`,
        startedAtMs,
        startedAtMs + 2_000,
        trace.marker,
        JSON.stringify(["flash", trace.marker]),
        trace.marker,
        trace.marker,
        JSON.stringify([`C:\\private\\${trace.marker}.img`]),
        JSON.stringify([`https://example.invalid/${trace.marker}`]),
        trace.marker,
        trace.marker,
        trace.marker,
        JSON.stringify([trace.marker]),
        trace.marker,
        JSON.stringify([{ kind: trace.marker, count: 1 }]),
      ),
      env.DB.prepare(
        `INSERT INTO usage_output_chunks
           (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256, credential_redactions_json)
         VALUES (?, ?, 'stdout', 0, ?, ?, ?, ?)`,
      ).bind(
        trace.chunkId,
        trace.eventId,
        trace.marker,
        new TextEncoder().encode(trace.marker).byteLength,
        "a".repeat(64),
        JSON.stringify([{ kind: trace.marker, count: 1 }]),
      ),
    ];
    if (includeProjection) {
      statementsForTrace.push(
        env.DB.prepare(
          "UPDATE usage_operation_runs SET trace_complete = 1 WHERE run_id = ?",
        ).bind(trace.runId),
        env.DB.prepare(
          `INSERT INTO usage_logs
             (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
              source_schema, trace_run_id)
           VALUES (7, 'User 7', 'fastboot_flash', ?, 'success', ?, ?, 2, ?)`,
        ).bind(
          `Projected ${trace.runId}`,
          trace.runId,
          Math.floor(startedAtMs / 1_000),
          trace.runId,
        ),
      );
    }
    return statementsForTrace;
  });
  for (let offset = 0; offset < statements.length; offset += 80) {
    await env.DB.batch(statements.slice(offset, offset + 80));
  }
  return traces;
}

async function seedScheduledCleanupRows(nowSeconds: number): Promise<void> {
  for (const [sessionId, timestamp] of [
    ["stale-session", nowSeconds - 121],
    ["fresh-session", nowSeconds],
  ] as const) {
    await env.DB.prepare(
      `INSERT INTO online_sessions
         (session_id, user_id, user_name, client_version, ip, connected_at, last_seen_at)
       VALUES (?, 7, 'User 7', '1.4.0', '203.0.113.45', ?, ?)`,
    ).bind(sessionId, timestamp, timestamp).run();
    await env.DB.prepare(
      `INSERT INTO session_leases
         (session_id, user_id, username, client_version, build_id, process_nonce,
          sequence, last_heartbeat_at, created_at, updated_at)
       VALUES (?, 7, 'user-7', '1.4.0', 'build-7', ?, 1, ?, ?, ?)`,
    ).bind(sessionId, `nonce-${sessionId}`, timestamp, timestamp, timestamp).run();
  }
  await env.DB.prepare(
    `INSERT INTO integrity_rate_limits (ip_hash, window_start, count, last_event_id)
     VALUES ('stale-ip-hash', ?, 1, 'stale-event'), ('fresh-ip-hash', ?, 1, 'fresh-event')`,
  ).bind(nowSeconds - 121, nowSeconds).run();
}

async function seedCleanRetentionHistory(rowCount: number, ageDays: number): Promise<void> {
  const startedAtMs = FIXED_NOW_MS - ageDays * DAY_MS;
  const statements = Array.from({ length: rowCount }, (_, index) => {
    const suffix = index.toString().padStart(4, "0");
    const runId = `run-clean-history-${suffix}`;
    const eventId = `event-clean-history-${suffix}`;
    return [
      env.DB.prepare(
        `INSERT INTO usage_operation_runs
           (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
            client_version, started_at_ms, trace_complete)
         VALUES (?, 7, 'User 7', 2, 'clean', 'Clean history', 'running', '1.4.0', ?, 0)`,
      ).bind(runId, startedAtMs),
      env.DB.prepare(
        `INSERT INTO usage_operation_events
           (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms,
            stdout_chunks, stderr_chunks)
         VALUES (?, ?, 1, 'stage', 'Clean history', 'success', ?, 0, 0)`,
      ).bind(eventId, runId, startedAtMs),
    ];
  }).flat();
  for (let offset = 0; offset < statements.length; offset += 80) {
    await env.DB.batch(statements.slice(offset, offset + 80));
  }
  await env.DB.prepare(
    `UPDATE usage_operation_events SET retention_detail_cleared = 1
     WHERE event_id LIKE 'event-clean-history-%'`,
  ).run();
  await env.DB.prepare(
    `UPDATE usage_operation_runs SET retention_detail_cleared = 1
     WHERE run_id LIKE 'run-clean-history-%'`,
  ).run();
}

async function seedYoungTraceCorpus(rowCount: number, startedAtMs: number): Promise<void> {
  const numbers = `WITH digits(value) AS (
      VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)
    ), numbers(value) AS (
      SELECT d0.value + 10*d1.value + 100*d2.value + 1000*d3.value + 10000*d4.value
      FROM digits AS d0
      CROSS JOIN digits AS d1
      CROSS JOIN digits AS d2
      CROSS JOIN digits AS d3
      CROSS JOIN digits AS d4
    )`;
  await env.DB.prepare(
    `${numbers}
     INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        client_version, started_at_ms, trace_complete)
     SELECT printf('run-young-%05d', value), 7, 'User 7', 2, 'young', 'Young trace',
            'running', '1.4.0', ?, 0
     FROM numbers WHERE value < ?`,
  ).bind(startedAtMs, rowCount).run();
  await env.DB.prepare(
    `INSERT INTO usage_operation_events
       (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms,
        stdout_chunks, stderr_chunks)
     SELECT replace(run_id, 'run-', 'event-'), run_id, 1, 'stage', 'Young event',
            'started', started_at_ms, 1, 0
     FROM usage_operation_runs WHERE run_id LIKE 'run-young-%'`,
  ).run();
  await env.DB.prepare(
    `INSERT INTO usage_output_chunks
       (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256)
     SELECT replace(event_id, 'event-', 'chunk-'), event_id, 'stdout', 0, '', 0, ?
     FROM usage_operation_events WHERE event_id LIKE 'event-young-%'`,
  ).bind("a".repeat(64)).run();
}

function recordingDatabase(
  preparedSql: string[],
  batchResults?: D1Result<unknown>[],
): D1Database {
  return {
    prepare(query: string): D1PreparedStatement {
      preparedSql.push(query);
      return env.DB.prepare(query);
    },
    async batch<T = unknown>(statements: D1PreparedStatement[]): Promise<D1Result<T>[]> {
      const results = await env.DB.batch<T>(statements);
      batchResults?.push(...results as D1Result<unknown>[]);
      return results;
    },
  } as D1Database;
}

async function queryPlan(query: string, ...bindings: unknown[]): Promise<string> {
  const result = await env.DB.prepare(`EXPLAIN QUERY PLAN ${query}`)
    .bind(...bindings)
    .all<{ detail: string }>();
  return result.results.map((row) => row.detail).join("\n");
}

async function count(table: string): Promise<number> {
  const row = await env.DB.prepare(`SELECT COUNT(*) AS value FROM ${table}`).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function rowCount(table: string, idColumn: string, id: string): Promise<number> {
  const row = await env.DB.prepare(
    `SELECT COUNT(*) AS value FROM ${table} WHERE ${idColumn} = ?`,
  ).bind(id).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function operationalRunCount(): Promise<number> {
  return scalarCount(
    `SELECT COUNT(*) AS value
     FROM usage_operation_runs
     WHERE device_serial IS NOT NULL
        OR source_ip IS NOT NULL
        OR source_paths_json <> '[]'
        OR source_urls_json <> '[]'
        OR error_message IS NOT NULL
        OR credential_redactions_json <> '[]'`,
  );
}

async function operationalEventCount(): Promise<number> {
  return scalarCount(
    `SELECT COUNT(*) AS value
     FROM usage_operation_events
     WHERE command_program IS NOT NULL
        OR command_argv_json IS NOT NULL
        OR command_line IS NOT NULL
        OR working_directory IS NOT NULL
        OR paths_json <> '[]'
        OR urls_json <> '[]'
        OR serial IS NOT NULL
        OR verification IS NOT NULL
        OR device_state IS NOT NULL
        OR remedies_json <> '[]'
        OR error_message IS NOT NULL
        OR credential_redactions_json <> '[]'`,
  );
}

async function projectedLogCount(runIdPrefix: string): Promise<number> {
  return scalarCount(
    `SELECT COUNT(*) AS value
     FROM usage_logs
     WHERE event_key LIKE ?`,
    `${runIdPrefix}%`,
  );
}

async function scalarCount(query: string, ...bindings: unknown[]): Promise<number> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function runRow(runId: string): Promise<Record<string, unknown> | null> {
  return env.DB.prepare(
    `SELECT title, outcome, device_serial, source_ip, source_paths_json, source_urls_json,
            error_message, credential_redactions_json
     FROM usage_operation_runs WHERE run_id = ?`,
  ).bind(runId).first<Record<string, unknown>>();
}

async function eventRow(eventId: string): Promise<Record<string, unknown> | null> {
  return env.DB.prepare(
    `SELECT step_name, partition_name, status, command_program, command_argv_json, command_line,
            working_directory, paths_json, urls_json, serial, verification, device_state,
            remedies_json, error_message, credential_redactions_json
     FROM usage_operation_events WHERE event_id = ?`,
  ).bind(eventId).first<Record<string, unknown>>();
}

async function databaseContains(marker: string): Promise<boolean> {
  const runMatches = await env.DB.prepare(
    `SELECT COUNT(*) AS value FROM usage_operation_runs
     WHERE device_serial LIKE ? OR source_ip LIKE ? OR source_paths_json LIKE ?
        OR source_urls_json LIKE ? OR error_message LIKE ? OR credential_redactions_json LIKE ?`,
  ).bind(...Array(6).fill(`%${marker}%`)).first<{ value: number }>();
  const eventMatches = await env.DB.prepare(
    `SELECT COUNT(*) AS value FROM usage_operation_events
     WHERE command_program LIKE ? OR command_argv_json LIKE ? OR command_line LIKE ?
        OR working_directory LIKE ? OR paths_json LIKE ? OR urls_json LIKE ? OR serial LIKE ?
        OR verification LIKE ? OR device_state LIKE ? OR remedies_json LIKE ?
        OR error_message LIKE ? OR credential_redactions_json LIKE ?`,
  ).bind(...Array(12).fill(`%${marker}%`)).first<{ value: number }>();
  const chunkMatches = await env.DB.prepare(
    `SELECT COUNT(*) AS value FROM usage_output_chunks
     WHERE text LIKE ? OR credential_redactions_json LIKE ?`,
  ).bind(`%${marker}%`, `%${marker}%`).first<{ value: number }>();
  return Number(runMatches?.value ?? 0) + Number(eventMatches?.value ?? 0) + Number(chunkMatches?.value ?? 0) > 0;
}
