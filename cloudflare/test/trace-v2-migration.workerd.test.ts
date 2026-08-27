import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

declare module "cloudflare:workers" {
  interface ProvidedEnv {
    TEST_MIGRATIONS: D1Migration[];
    TEST_TRACE_V2_MIGRATIONS: D1Migration[];
    TEST_TRACE_V2_UPGRADE_MIGRATIONS: D1Migration[];
  }
}

const legacyUsageLogsSql = "CREATE TABLE usage_logs (id INTEGER PRIMARY KEY AUTOINCREMENT, api_user_id INTEGER, api_user_name TEXT, operation_kind TEXT NOT NULL, title TEXT, status TEXT NOT NULL DEFAULT 'started', event_key TEXT, started_at INTEGER NOT NULL, ended_at INTEGER, duration_ms INTEGER, created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')));";

beforeEach(async () => {
  await reset();
  await env.DB.exec(legacyUsageLogsSql);
});

describe("usage trace V2 D1 migration", () => {
  it("applies the V2 migration twice and preserves V1 rows", async () => {
    await env.DB.prepare(
      "INSERT INTO usage_logs (operation_kind,status,event_key,started_at) VALUES ('Flashing','success','legacy-1',1)",
    ).run();
    await applyD1Migrations(env.DB, env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? []);
    await applyD1Migrations(env.DB, env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? []);

    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key='legacy-1'")).toBe(1);
    expect(await tableExists("usage_operation_runs")).toBe(true);
    expect(await tableExists("usage_operation_events")).toBe(true);
    expect(await tableExists("usage_output_chunks")).toBe(true);
  });

  it("upgrades legacy triggers through the idempotent P0 forward migration", async () => {
    await migrateTraceV2();
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM sqlite_master
       WHERE type = 'trigger'
         AND name IN ('trg_trace_events_reject_completed_run','trg_trace_chunks_reject_completed_run')
         AND sql LIKE '%parent missing%'`,
    )).toBe(2);
  });

  it("upgrades existing V2 rows through the one-time retention stage", async () => {
    const migrations = env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? [];
    await applyD1Migrations(env.DB, migrations.slice(0, -1));
    await seedDbRun("run-before-retention-stage");
    await seedDbEvent("event-before-retention-stage", "run-before-retention-stage", 1, 0, 0);
    expect(await columnExists("usage_operation_runs", "retention_detail_cleared")).toBe(false);
    expect(await columnExists("usage_operation_events", "retention_detail_cleared")).toBe(false);

    await applyD1Migrations(env.DB, migrations.slice(-1));

    expect(await retentionMarker("usage_operation_runs", "run_id", "run-before-retention-stage")).toBe(0);
    expect(await retentionMarker("usage_operation_events", "event_id", "event-before-retention-stage")).toBe(0);
    await assertRetentionStageShape();
  });

  it("creates the final retention marker shape in a fresh schema", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await seedDbRun("run-fresh-retention-shape");
    await seedDbEvent("event-fresh-retention-shape", "run-fresh-retention-shape", 1, 0, 0);

    expect(await retentionMarker("usage_operation_runs", "run_id", "run-fresh-retention-shape")).toBe(0);
    expect(await retentionMarker("usage_operation_events", "event_id", "event-fresh-retention-shape")).toBe(0);
    await assertRetentionStageShape();
  });

  it("rejects a direct event sequence above one hundred after P0 migration", async () => {
    await migrateTraceV2();
    await seedDbRun("run-p0-sequence-limit");

    await expect(seedDbEvent("event-p0-sequence-limit", "run-p0-sequence-limit", 101, 0, 0))
      .rejects.toThrow(/event sequence/i);
  });

  it("rejects a direct event sequence above one hundred in the base migration", async () => {
    await applyD1Migrations(env.DB, (env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? []).slice(0, 1));
    await seedDbRun("run-base-sequence-limit");

    await expect(seedDbEvent("event-base-sequence-limit", "run-base-sequence-limit", 101, 0, 0))
      .rejects.toThrow(/event sequence/i);
  });

  it("rejects a direct event sequence above one hundred in a fresh schema", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await seedDbRun("run-schema-sequence-limit");

    await expect(seedDbEvent("event-schema-sequence-limit", "run-schema-sequence-limit", 101, 0, 0))
      .rejects.toThrow(/event sequence/i);
  });

  it("keeps sequence one hundred legal and rejects a higher update in a fresh schema", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await seedDbRun("run-schema-sequence-insert");
    await expect(seedDbEvent(
      "event-schema-sequence-insert",
      "run-schema-sequence-insert",
      100,
      0,
      0,
    )).resolves.toBeDefined();
    await seedDbRun("run-schema-sequence-update");
    await seedDbEvent("event-schema-sequence-update", "run-schema-sequence-update", 1, 0, 0);

    await expect(env.DB.prepare(
      "UPDATE usage_operation_events SET sequence = 100 WHERE event_id = 'event-schema-sequence-update'",
    ).run()).resolves.toBeDefined();
    await expect(env.DB.prepare(
      "UPDATE usage_operation_events SET sequence = 101 WHERE event_id = 'event-schema-sequence-update'",
    ).run()).rejects.toThrow(/event sequence/i);
    expect(await scalar(
      "SELECT sequence AS value FROM usage_operation_events WHERE event_id = 'event-schema-sequence-update'",
    )).toBe(100);
  });

  it("preserves legal rows across base twice then P0 twice and rejects a higher update", async () => {
    const migrations = env.TEST_TRACE_V2_MIGRATIONS ?? [];
    await applyD1Migrations(env.DB, migrations.slice(0, 2));
    await seedDbRun("run-upgrade-sequence-update");
    await seedDbEvent("event-upgrade-sequence-update", "run-upgrade-sequence-update", 100, 0, 0);

    await applyD1Migrations(env.DB, migrations.slice(2));

    expect(await scalar(
      "SELECT sequence AS value FROM usage_operation_events WHERE event_id = 'event-upgrade-sequence-update'",
    )).toBe(100);
    await expect(env.DB.prepare(
      "UPDATE usage_operation_events SET sequence = 101 WHERE event_id = 'event-upgrade-sequence-update'",
    ).run()).rejects.toThrow(/event sequence/i);
    expect(await scalar(
      "SELECT sequence AS value FROM usage_operation_events WHERE event_id = 'event-upgrade-sequence-update'",
    )).toBe(100);
  });

  it("rejects a complete running run insert in a fresh schema and allows a terminal one", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);

    await expect(seedDbRunWithState("run-schema-complete-running", "running", 1))
      .rejects.toThrow(/terminal outcome/i);
    await expect(seedDbRunWithState("run-schema-complete-terminal", "success", 1))
      .resolves.toBeDefined();
  });

  it("preserves a legal run across base twice then P0 twice and rejects a complete running insert", async () => {
    const migrations = env.TEST_TRACE_V2_MIGRATIONS ?? [];
    await applyD1Migrations(env.DB, migrations.slice(0, 2));
    await seedDbRun("run-upgrade-legal-before-p0");

    await applyD1Migrations(env.DB, migrations.slice(2));

    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_runs WHERE run_id = 'run-upgrade-legal-before-p0'",
    )).toBe(1);
    await expect(seedDbRunWithState("run-upgrade-complete-running", "running", 1))
      .rejects.toThrow(/terminal outcome/i);
    await expect(seedDbRunWithState("run-upgrade-complete-terminal", "success", 1))
      .resolves.toBeDefined();
  });

  it("keeps P0 insert guards marker-free before stage and seals retained trace details afterward", async () => {
    const migrations = env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? [];
    await applyD1Migrations(env.DB, migrations.slice(0, 2));
    expect(await triggerSql("trg_trace_events_reject_completed_run"))
      .not.toMatch(/retention_detail_cleared|retention detail sealed/i);
    expect(await triggerSql("trg_trace_chunks_reject_completed_run"))
      .not.toMatch(/retention_detail_cleared|retention detail sealed/i);
    await seedDbRun("run-upgrade-open-sealed");
    await seedDbEvent("event-upgrade-open-sealed", "run-upgrade-open-sealed", 1, 1, 1);
    await expect(seedDbChunk("chunk-upgrade-before-stage", "event-upgrade-open-sealed", "stdout", 0))
      .resolves.toBeDefined();

    await applyD1Migrations(env.DB, migrations.slice(2));

    await expectSealedTrigger("trg_trace_events_reject_completed_run");
    await expectSealedTrigger("trg_trace_chunks_reject_completed_run");
    await expect(seedDbEvent(
      "event-upgrade-before-seal",
      "run-upgrade-open-sealed",
      2,
      0,
      0,
    )).resolves.toBeDefined();
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = 'run-upgrade-open-sealed'",
    ).run();
    await env.DB.prepare(
      "UPDATE usage_operation_events SET retention_detail_cleared = 1 WHERE event_id = 'event-upgrade-open-sealed'",
    ).run();

    await expect(seedDbEvent(
      "event-upgrade-after-seal",
      "run-upgrade-open-sealed",
      3,
      0,
      0,
    )).rejects.toThrow(/retention detail sealed/i);
    await expect(seedDbChunk(
      "chunk-upgrade-after-seal",
      "event-upgrade-open-sealed",
      "stderr",
      0,
    )).rejects.toThrow(/retention detail sealed/i);

    await seedDbRun("run-upgrade-complete-sealed");
    await seedDbEvent("event-upgrade-complete-sealed", "run-upgrade-complete-sealed", 1, 0, 0);
    await env.DB.prepare(
      `UPDATE usage_operation_runs
       SET outcome = 'success', final_sequence = 1, trace_complete = 1
       WHERE run_id = 'run-upgrade-complete-sealed'`,
    ).run();
    await env.DB.prepare(
      `UPDATE usage_operation_runs SET retention_detail_cleared = 1
       WHERE run_id = 'run-upgrade-complete-sealed'`,
    ).run();
    await env.DB.prepare(
      "UPDATE usage_operation_events SET retention_detail_cleared = 1 WHERE event_id = 'event-upgrade-complete-sealed'",
    ).run();

    await expect(seedDbEvent(
      "event-upgrade-complete-after-seal",
      "run-upgrade-complete-sealed",
      2,
      0,
      0,
    )).rejects.toThrow(/trace run is complete/i);
    await expect(seedDbChunk(
      "chunk-upgrade-complete-after-seal",
      "event-upgrade-complete-sealed",
      "stdout",
      0,
    )).rejects.toThrow(/trace run is complete/i);
  });

  it("seals direct event and chunk inserts in the fresh schema", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await expectSealedTrigger("trg_trace_events_reject_completed_run");
    await expectSealedTrigger("trg_trace_chunks_reject_completed_run");
    await seedDbRun("run-schema-open-sealed");
    await seedDbEvent("event-schema-open-sealed", "run-schema-open-sealed", 1, 1, 0);
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = 'run-schema-open-sealed'",
    ).run();
    await env.DB.prepare(
      "UPDATE usage_operation_events SET retention_detail_cleared = 1 WHERE event_id = 'event-schema-open-sealed'",
    ).run();

    await expect(seedDbEvent(
      "event-schema-after-seal",
      "run-schema-open-sealed",
      2,
      0,
      0,
    )).rejects.toThrow(/retention detail sealed/i);
    await expect(seedDbChunk(
      "chunk-schema-after-seal",
      "event-schema-open-sealed",
      "stdout",
      0,
    )).rejects.toThrow(/retention detail sealed/i);
  });

  it("rejects finalizing a sealed upgraded run before terminal and evidence checks", async () => {
    await migrateTraceV2();
    await seedDbRunWithCompleteEvidence("run-upgrade-finalize-sealed");
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = 'run-upgrade-finalize-sealed'",
    ).run();

    await expectCompletionTriggerSealedFirst();
    await expect(env.DB.prepare(
      `UPDATE usage_operation_runs
       SET outcome = 'success', final_sequence = 1, trace_complete = 1
       WHERE run_id = 'run-upgrade-finalize-sealed'`,
    ).run()).rejects.toThrow(/retention detail sealed/i);
    expect(await runCompletionState("run-upgrade-finalize-sealed")).toEqual({
      final_sequence: null,
      outcome: "running",
      trace_complete: 0,
    });
  });

  it("rejects finalizing a sealed fresh run before terminal and evidence checks", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await seedDbRunWithCompleteEvidence("run-schema-finalize-sealed");
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = 'run-schema-finalize-sealed'",
    ).run();

    await expectCompletionTriggerSealedFirst();
    await expect(env.DB.prepare(
      `UPDATE usage_operation_runs
       SET outcome = 'success', final_sequence = 1, trace_complete = 1
       WHERE run_id = 'run-schema-finalize-sealed'`,
    ).run()).rejects.toThrow(/retention detail sealed/i);
    expect(await runCompletionState("run-schema-finalize-sealed")).toEqual({
      final_sequence: null,
      outcome: "running",
      trace_complete: 0,
    });
  });

  it("rejects direct detail changes and marker reset on a sealed upgraded run", async () => {
    await migrateTraceV2();
    await assertSealedRunDetailUpdates("run-upgrade-detail-sealed");
  });

  it("rejects direct detail changes and marker reset on a sealed fresh run", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await assertSealedRunDetailUpdates("run-schema-detail-sealed");
  });

  it("rejects direct detail changes and marker reset on a sealed upgraded event", async () => {
    await migrateTraceV2();
    await assertSealedEventDetailUpdates(
      "run-upgrade-event-detail-sealed",
      "event-upgrade-detail-sealed",
    );
  });

  it("rejects direct detail changes and marker reset on a sealed fresh event", async () => {
    await applyD1Migrations(env.DB, env.TEST_MIGRATIONS ?? []);
    await assertSealedEventDetailUpdates(
      "run-schema-event-detail-sealed",
      "event-schema-detail-sealed",
    );
  });

  it("rejects an event whose run parent does not exist", async () => {
    await migrateTraceV2();

    await expect(seedDbEvent("event-orphan", "run-missing", 1, 0, 0))
      .rejects.toThrow(/event parent/i);
  });

  it("rejects a chunk whose event parent does not exist", async () => {
    await migrateTraceV2();

    await expect(seedDbChunk("chunk-orphan", "event-missing", "stdout", 0))
      .rejects.toThrow(/chunk parent/i);
  });

  it("rejects chunk_index equal to the event's declared total", async () => {
    await migrateTraceV2();
    await seedDbRun("run-bounds");
    await seedDbEvent("event-bounds", "run-bounds", 1, 1, 0);

    await expect(seedDbChunk("chunk-bounds", "event-bounds", "stdout", 1))
      .rejects.toThrow(/declared total/i);
  });

  it("rejects an event sequence beyond its open run's known final bound", async () => {
    await migrateTraceV2();
    await seedDbRun("run-final-bound");
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET final_sequence = 1 WHERE run_id = 'run-final-bound'",
    ).run();

    await expect(seedDbEvent("event-final-bound", "run-final-bound", 2, 0, 0))
      .rejects.toThrow(/final sequence/i);
  });

  it("allows an open trace to persist a legal chunk gap", async () => {
    await migrateTraceV2();
    await seedDbRun("run-gap");
    await seedDbEvent("event-gap", "run-gap", 1, 3, 0);

    await expect(seedDbChunk("chunk-gap", "event-gap", "stdout", 1)).resolves.toBeDefined();
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
  });

  it("rejects completion while declared chunk evidence remains incomplete", async () => {
    await migrateTraceV2();
    await seedDbRun("run-incomplete");
    await seedDbEvent("event-incomplete", "run-incomplete", 1, 3, 0);
    await seedDbChunk("chunk-incomplete", "event-incomplete", "stdout", 1);

    await expect(env.DB.prepare(
      "UPDATE usage_operation_runs SET outcome = 'success', final_sequence = 1, trace_complete = 1 WHERE run_id = 'run-incomplete'",
    ).run()).rejects.toThrow(/incomplete/i);
  });

  it("rejects completion while the run outcome is still running", async () => {
    await migrateTraceV2();
    await seedDbRun("run-running-complete");
    await seedDbEvent("event-running-complete", "run-running-complete", 1, 0, 0);

    await expect(env.DB.prepare(
      "UPDATE usage_operation_runs SET final_sequence = 1, trace_complete = 1 WHERE run_id = 'run-running-complete'",
    ).run()).rejects.toThrow(/terminal outcome/i);
  });

  it("rejects the one hundred first event for a run", async () => {
    await migrateTraceV2();
    await seedDbRun("run-event-count");
    for (let sequence = 1; sequence <= 100; sequence += 1) {
      await seedDbEvent(`event-count-${sequence}`, "run-event-count", sequence, 0, 0);
    }

    await expect(seedDbEvent("event-count-101", "run-event-count", 101, 0, 0))
      .rejects.toThrow(/event quota/i);
  });

  it("rejects event metadata that would exceed eight MiB for a run", async () => {
    await migrateTraceV2();
    await seedDbRun("run-event-storage");
    const remediesJson = JSON.stringify(["x".repeat(950_000)]);
    for (let sequence = 1; sequence <= 8; sequence += 1) {
      await seedDbEvent(`event-storage-${sequence}`, "run-event-storage", sequence, 0, 0, remediesJson);
    }

    await expect(seedDbEvent("event-storage-9", "run-event-storage", 9, 0, 0, remediesJson))
      .rejects.toThrow(/event storage/i);
  });

  it("keeps the exact eight MiB event metadata boundary and rejects the next byte", async () => {
    await migrateTraceV2();
    await seedDbRun("run-storage-boundary");
    const bulkRemedies = "x".repeat(950_000);
    for (let sequence = 1; sequence <= 8; sequence += 1) {
      await seedDbEvent(
        `event-storage-boundary-${sequence}`,
        "run-storage-boundary",
        sequence,
        0,
        0,
        bulkRemedies,
      );
    }
    await seedDbRun("run-storage-probe123");
    await seedDbEvent("event-storage-boundary-p", "run-storage-probe123", 1, 0, 0, "");
    const fixedBytes = await eventStorageBytes("run-storage-probe123");
    const currentBytes = await eventStorageBytes("run-storage-boundary");
    const remainingBytes = 8_388_608 - currentBytes - fixedBytes;
    expect(remainingBytes).toBeGreaterThan(0);

    await expect(seedDbEvent(
      "event-storage-boundary-9",
      "run-storage-boundary",
      9,
      0,
      0,
      JSON.stringify(["x".repeat(remainingBytes - 4)]),
    )).resolves.toBeDefined();
    expect(await eventStorageBytes("run-storage-boundary")).toBe(8_388_608);

    await expect(seedDbEvent("event-storage-boundary-10", "run-storage-boundary", 10, 0, 0))
      .rejects.toThrow(/event storage/i);
  });
});

async function migrateTraceV2(): Promise<void> {
  await applyD1Migrations(env.DB, env.TEST_TRACE_V2_UPGRADE_MIGRATIONS ?? []);
}

async function seedDbRun(runId: string): Promise<D1Result<unknown>> {
  return seedDbRunWithState(runId, "running", 0);
}

async function seedDbRunWithState(
  runId: string,
  outcome: "running" | "success",
  traceComplete: 0 | 1,
): Promise<D1Result<unknown>> {
  return env.DB.prepare(
    `INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        client_version, started_at_ms, trace_complete)
     VALUES (?, 7, 'User 7', 2, 'test', 'Test run', ?, '1.4.0', 1, ?)`,
  ).bind(runId, outcome, traceComplete).run();
}

async function seedDbEvent(
  eventId: string,
  runId: string,
  sequence: number,
  stdoutChunks: number,
  stderrChunks: number,
  remediesJson = "[]",
): Promise<D1Result<unknown>> {
  return env.DB.prepare(
    `INSERT INTO usage_operation_events
       (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms,
        stdout_chunks, stderr_chunks, remedies_json)
     VALUES (?, ?, ?, 'stage', 'Test event', 'started', 1, ?, ?, ?)`,
  ).bind(eventId, runId, sequence, stdoutChunks, stderrChunks, remediesJson).run();
}

async function seedDbChunk(
  chunkId: string,
  eventId: string,
  stream: "stdout" | "stderr",
  chunkIndex: number,
): Promise<D1Result<unknown>> {
  return env.DB.prepare(
    `INSERT INTO usage_output_chunks
       (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256)
     VALUES (?, ?, ?, ?, '', 0, ?)`,
  ).bind(
    chunkId,
    eventId,
    stream,
    chunkIndex,
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  ).run();
}

async function seedDbRunWithCompleteEvidence(runId: string): Promise<void> {
  const eventId = `${runId}-event`;
  await seedDbRun(runId);
  await seedDbEvent(eventId, runId, 1, 1, 0);
  await seedDbChunk(`${runId}-chunk`, eventId, "stdout", 0);
}

async function eventStorageBytes(runId: string): Promise<number> {
  const columns = [
    "event_id", "run_id", "event_kind", "step_name", "partition_name", "status",
    "command_program", "command_argv_json", "command_line", "working_directory",
    "paths_json", "urls_json", "serial", "verification", "device_state", "remedies_json",
    "error_class", "error_code", "error_message", "credential_redactions_json",
  ];
  const row = await env.DB.prepare(
    `SELECT COALESCE(SUM(${columns.map((column) =>
      `length(CAST(COALESCE(${column}, '') AS BLOB))`).join(" + ")}), 0) AS value
     FROM usage_operation_events WHERE run_id = ?`,
  ).bind(runId).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function columnExists(table: string, column: string): Promise<boolean> {
  const row = await env.DB.prepare(
    `SELECT COUNT(*) AS value FROM pragma_table_info(?) WHERE name = ?`,
  ).bind(table, column).first<{ value: number }>();
  return Number(row?.value ?? 0) === 1;
}

async function retentionMarker(table: string, idColumn: string, id: string): Promise<number> {
  const row = await env.DB.prepare(
    `SELECT retention_detail_cleared AS value FROM ${table} WHERE ${idColumn} = ?`,
  ).bind(id).first<{ value: number }>();
  return Number(row?.value ?? -1);
}

async function assertRetentionStageShape(): Promise<void> {
  expect(await retentionColumnDefinition("usage_operation_runs")).toEqual({
    dflt_value: "0",
    not_null: 1,
  });
  expect(await retentionColumnDefinition("usage_operation_events")).toEqual({
    dflt_value: "0",
    not_null: 1,
  });

  await expect(env.DB.prepare(
    "UPDATE usage_operation_runs SET retention_detail_cleared = 2",
  ).run()).rejects.toThrow(/check constraint/i);
  await expect(env.DB.prepare(
    "UPDATE usage_operation_events SET retention_detail_cleared = 2",
  ).run()).rejects.toThrow(/check constraint/i);

  expect(await indexDefinition("idx_trace_runs_retention_detail_pending")).toEqual({
    columns: "started_at_ms,run_id",
    partial: 1,
  });
  expect(await indexDefinition("idx_trace_events_retention_detail_pending")).toEqual({
    columns: "run_id,sequence,event_id",
    partial: 1,
  });
}

async function retentionColumnDefinition(table: string): Promise<{ dflt_value: string | null; not_null: number } | null> {
  return env.DB.prepare(
    "SELECT dflt_value, [notnull] AS not_null FROM pragma_table_info(?) WHERE name = 'retention_detail_cleared'",
  ).bind(table).first<{ dflt_value: string | null; not_null: number }>();
}

async function indexDefinition(name: string): Promise<{ columns: string; partial: number } | null> {
  return env.DB.prepare(
    `SELECT (
       SELECT group_concat(name, ',')
       FROM (SELECT name FROM pragma_index_info(?) ORDER BY seqno)
     ) AS columns,
     partial
     FROM pragma_index_list(CASE
       WHEN ? LIKE '%runs%' THEN 'usage_operation_runs'
       ELSE 'usage_operation_events'
     END)
     WHERE name = ?`,
  ).bind(name, name, name).first<{ columns: string; partial: number }>();
}

async function triggerSql(name: string): Promise<string> {
  const row = await env.DB.prepare(
    "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?",
  ).bind(name).first<{ sql: string }>();
  return row?.sql ?? "";
}

async function expectSealedTrigger(name: string): Promise<void> {
  const sql = await triggerSql(name);
  expect(sql).toMatch(/retention_detail_cleared/i);
  expect(sql).toMatch(/retention detail sealed/i);
}

async function expectCompletionTriggerSealedFirst(): Promise<void> {
  const sql = await triggerSql("trg_trace_runs_validate_completion");
  const sealed = sql.indexOf("trace retention detail sealed");
  expect(sealed).toBeGreaterThan(-1);
  expect(sealed).toBeLessThan(sql.indexOf("trace completion requires terminal outcome"));
  expect(sealed).toBeLessThan(sql.indexOf("trace run is incomplete"));
}

async function runCompletionState(runId: string): Promise<{
  final_sequence: number | null;
  outcome: string;
  trace_complete: number;
} | null> {
  return env.DB.prepare(
    `SELECT final_sequence, outcome, trace_complete
     FROM usage_operation_runs WHERE run_id = ?`,
  ).bind(runId).first<{
    final_sequence: number | null;
    outcome: string;
    trace_complete: number;
  }>();
}

async function assertSealedRunDetailUpdates(runId: string): Promise<void> {
  await seedDbRun(runId);
  await env.DB.prepare(
    "UPDATE usage_operation_runs SET error_message = 'secret' WHERE run_id = ?",
  ).bind(runId).run();
  await env.DB.prepare(
    `UPDATE usage_operation_runs
     SET error_message = NULL, retention_detail_cleared = 1,
         updated_at = updated_at + 1
     WHERE run_id = ?`,
  ).bind(runId).run();

  for (const assignment of [
    "api_user_id = 8",
    "title = 'Changed'",
    "outcome = 'success'",
    "ended_at_ms = 2",
    "duration_ms = 1",
    "final_sequence = 1",
    "trace_complete = 1",
    "error_message = 'revived'",
    "retention_detail_cleared = 0",
  ]) {
    await expect(env.DB.prepare(
      `UPDATE usage_operation_runs SET ${assignment} WHERE run_id = ?`,
    ).bind(runId).run()).rejects.toThrow(/retention detail sealed/i);
  }
  await expect(env.DB.prepare(
    "UPDATE usage_operation_runs SET updated_at = updated_at WHERE run_id = ?",
  ).bind(runId).run()).resolves.toBeDefined();
  expect(await env.DB.prepare(
    `SELECT error_message, retention_detail_cleared
     FROM usage_operation_runs WHERE run_id = ?`,
  ).bind(runId).first()).toMatchObject({
    error_message: null,
    retention_detail_cleared: 1,
  });
}

async function assertSealedEventDetailUpdates(runId: string, eventId: string): Promise<void> {
  await seedDbRun(runId);
  await seedDbEvent(eventId, runId, 1, 0, 0);
  await env.DB.prepare(
    "UPDATE usage_operation_events SET command_line = 'secret' WHERE event_id = ?",
  ).bind(eventId).run();
  await expect(env.DB.prepare(
    `UPDATE usage_operation_events
     SET command_line = NULL, retention_detail_cleared = 1
     WHERE event_id = ?`,
  ).bind(eventId).run()).resolves.toBeDefined();

  await expect(env.DB.prepare(
    "UPDATE usage_operation_events SET command_line = 'revived' WHERE event_id = ?",
  ).bind(eventId).run()).rejects.toThrow(/retention detail sealed/i);
  await expect(env.DB.prepare(
    "UPDATE usage_operation_events SET retention_detail_cleared = 0 WHERE event_id = ?",
  ).bind(eventId).run()).rejects.toThrow(/retention detail sealed/i);
  expect(await env.DB.prepare(
    `SELECT command_line, retention_detail_cleared
     FROM usage_operation_events WHERE event_id = ?`,
  ).bind(eventId).first()).toMatchObject({
    command_line: null,
    retention_detail_cleared: 1,
  });
}

async function scalar(query: string): Promise<number> {
  const row = await env.DB.prepare(query).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function tableExists(name: string): Promise<boolean> {
  return (await scalar(`SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' AND name = '${name}'`)) === 1;
}
