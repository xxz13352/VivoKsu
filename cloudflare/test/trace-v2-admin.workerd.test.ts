import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import adminRunSuccessFixture from "../contracts/trace-v2/admin-run-success.json";
import { decodeTraceCursorV2 } from "../src/trace-v2-contract";
import { purgeExpiredTraceData } from "../src/trace-v2-retention";
import type {
  KeysetPageV2,
  RomLogAdminRowV2,
  TraceOutputPageV2,
  TraceRunDetailV2,
  TraceRunSummaryV2,
  TraceUserSummaryV2,
} from "../src/trace-v2-contract";
import adminWorker from "../web/src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv {
    DB: D1Database;
    TEST_MIGRATIONS: D1Migration[];
  }
}

const ADMIN_SESSION_TOKEN = "trace-v2-admin-session";
const SAME_STARTED_AT_MS = 1_787_500_000_000;
const FIXED_NOW_MS = 1_787_548_645_000;
const SUCCESS_RUN_ID = "019d9c40-7b3c-7000-8000-000000000002";
const SUCCESS_EVENT_ID = "019d9c40-7b3c-7000-8000-000000000003";
const OUTPUT_EVENT_ID = "019d9c40-7b3c-7000-8000-000000000005";

beforeEach(async () => {
  vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS);
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO admins (id, username, salt, password_hash) VALUES (11, 'reviewer', 'unused', 'unused')",
    ),
    env.DB.prepare(
      "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (11, ?, '2999-01-01T00:00:00.000Z')",
    ).bind(ADMIN_SESSION_TOKEN),
    env.DB.prepare(
      "INSERT INTO api_users (id, username, name, token) VALUES (7, 'alice', 'Alice Zhang', 'trace-user-token')",
    ),
  ]);
});

afterEach(() => vi.restoreAllMocks());

describe("administrator trace V2 API", () => {
  it("exposes the authenticated V2 administrator routes", async () => {
    const response = await adminGet("/api/usage-logs/v2/users");

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("strict-transport-security")).toContain("max-age=31536000");
    expect(response.headers.get("x-frame-options")).toBe("DENY");
  });

  it("paginates equal timestamps without duplicates or gaps", async () => {
    await seedRunsAtSameTimestamp(75, SAME_STARTED_AT_MS);

    const first = await adminGet("/api/usage-logs/v2/runs?limit=50");
    expect(first.status).toBe(200);
    const page1 = await first.json() as KeysetPageV2<TraceRunSummaryV2>;
    expect(page1.items).toHaveLength(50);
    expect(page1.next_cursor).not.toBeNull();

    const second = await adminGet(
      `/api/usage-logs/v2/runs?limit=50&cursor=${encodeURIComponent(page1.next_cursor!)}`,
    );
    expect(second.status).toBe(200);
    const page2 = await second.json() as KeysetPageV2<TraceRunSummaryV2>;
    expect(page2.items).toHaveLength(25);
    expect(page2.next_cursor).toBeNull();
    expect(new Set([...page1.items, ...page2.items].map((item) => item.trace_ref)).size).toBe(75);
  });

  it("paginates a colliding V1 synthetic key and legal V2 UUID without losing either source", async () => {
    const collidingRunId = "00000000-0000-7000-8000-00000000002a";
    await seedSimpleRun(collidingRunId, "success", SAME_STARTED_AT_MS);
    await env.DB.prepare(
      `INSERT INTO usage_logs
         (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
       VALUES (42, 7, 'Alice Zhang', 'legacy', 'Colliding legacy run', 'failed', 'legacy-42', ?)`,
    ).bind(Math.floor(SAME_STARTED_AT_MS / 1000)).run();

    const first = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=1");
    expect(first.items).toHaveLength(1);
    expect(first.next_cursor).not.toBeNull();
    const second = await readPage<TraceRunSummaryV2>(
      `/api/usage-logs/v2/runs?limit=1&cursor=${encodeURIComponent(first.next_cursor!)}`,
    );

    expect(second.items).toHaveLength(1);
    expect(second.next_cursor).toBeNull();
    expect(new Set([...first.items, ...second.items].map((item) => item.trace_ref))).toEqual(new Set([
      "v1:42",
      `v2:${collidingRunId}`,
    ]));
  });

  it("keeps a maximum-safe actual timestamp in the opaque cursor without losing a limit-one page", async () => {
    const highStartedAtMs = 4_503_599_627_370_496;
    const firstRunId = "00000000-0000-7000-8000-000000000010";
    const secondRunId = "00000000-0000-7000-8000-000000000011";
    await seedSimpleRun(firstRunId, "success", highStartedAtMs);
    await seedSimpleRun(secondRunId, "failed", highStartedAtMs);
    const before = await env.DB.prepare(
      "SELECT rowid AS identity, run_id FROM usage_operation_runs ORDER BY rowid",
    ).all<{ identity: number; run_id: string }>();
    expect(before.results.map((row) => row.identity).every((identity) => Number.isSafeInteger(identity) && identity > 0)).toBe(true);
    expect(new Set(before.results.map((row) => row.identity)).size).toBe(2);
    await env.DB.prepare("UPDATE usage_operation_runs SET title = 'Updated title' WHERE run_id = ?")
      .bind(firstRunId).run();
    const after = await env.DB.prepare(
      "SELECT rowid AS identity, run_id FROM usage_operation_runs ORDER BY rowid",
    ).all<{ identity: number; run_id: string }>();
    expect(after.results).toEqual(before.results);

    const firstResponse = await adminGet("/api/usage-logs/v2/runs?limit=1");
    expect(firstResponse.status).toBe(200);
    const first = await firstResponse.json() as KeysetPageV2<TraceRunSummaryV2>;
    expect(first.items).toHaveLength(1);
    expect(first.items[0].started_at_ms).toBe(highStartedAtMs);
    expect(first.next_cursor).not.toBeNull();
    expect(decodeTraceCursorV2(first.next_cursor!).started_at_ms).toBe(highStartedAtMs);

    const second = await readPage<TraceRunSummaryV2>(
      `/api/usage-logs/v2/runs?limit=1&cursor=${encodeURIComponent(first.next_cursor!)}`,
    );
    expect(second.items).toHaveLength(1);
    expect(second.items[0].started_at_ms).toBe(highStartedAtMs);
    expect(second.next_cursor).toBeNull();
    expect(new Set([...first.items, ...second.items].map((item) => item.trace_ref)).size).toBe(2);
  });

  it("uses a default limit of 50 and caps an explicit limit at 200", async () => {
    await seedRunsAtSameTimestamp(205, SAME_STARTED_AT_MS);

    const defaultPage = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs");
    const cappedPage = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=999");

    expect(defaultPage.items).toHaveLength(50);
    expect(cappedPage.items).toHaveLength(200);
    expect(defaultPage.next_cursor).not.toBeNull();
    expect(cappedPage.next_cursor).not.toBeNull();
  });

  it("returns explicit legacy detail unavailability", async () => {
    await env.DB.prepare(
      `INSERT INTO usage_logs
         (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
       VALUES (42, 7, 'Alice Zhang', 'fastboot_flash', 'Legacy flash', 'failed', 'legacy-42', ?)` ,
    ).bind(Math.floor(SAME_STARTED_AT_MS / 1000)).run();

    const response = await adminGet("/api/usage-logs/v2/runs/v1%3A42");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      source_schema: 1,
      detail_available: false,
      detail_unavailable_reason: "legacy_client_no_step_data",
      run: { source_schema: 1, trace_ref: "v1:42", legacy_id: 42 },
      events: [],
    });
  });

  it("suppresses projected V1 duplicates and aggregates dual-read user summaries", async () => {
    await seedSuccessTrace();
    await env.DB.batch([
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (41, 7, 'Alice Zhang', 'fastboot_flash', 'Projected copy', 'success', ?, ?, 2, ?)`,
      ).bind(SUCCESS_RUN_ID, Math.floor(SAME_STARTED_AT_MS / 1000), SUCCESS_RUN_ID),
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
         VALUES (42, 7, 'Alice Zhang', 'legacy_reboot', 'Legacy reboot', 'failed', 'legacy-42', ?)`,
      ).bind(Math.floor((SAME_STARTED_AT_MS - 5_000) / 1000)),
    ]);

    const runs = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=10");
    const users = await readPage<TraceUserSummaryV2>("/api/usage-logs/v2/users?status=failed&q=legacy_reboot");

    expect(runs.items.map((item) => item.trace_ref).sort()).toEqual([
      "v1:42",
      `v2:${SUCCESS_RUN_ID}`,
    ]);
    expect(users.items).toHaveLength(1);
    expect(users.items[0]).toMatchObject({
      user_id: 7,
      username: "alice",
      name: "Alice Zhang",
      operation_count: 1,
      failed_count: 1,
      last_operation: { trace_ref: "v1:42" },
    });
  });

  it("keeps a colliding V1 row visible beside its separately tagged V2 run", async () => {
    const runId = "019d9c40-7b3c-7000-8000-000000000088";
    await env.DB.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          client_version, started_at_ms, trace_complete)
       VALUES (?, 7, 'Alice Zhang', 2, 'simple', 'Simple run', 'success', '1.4.0', ?, 1)`,
    ).bind(runId, SAME_STARTED_AT_MS).run();
    await env.DB.batch([
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (81, 8, 'Legacy user', 'legacy', 'Colliding V1 history', 'failed', ?, ?, 1, NULL)`,
      ).bind(runId, Math.floor((SAME_STARTED_AT_MS - 1_000) / 1_000)),
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (82, 7, 'Alice Zhang', 'simple', 'Projected V2 copy', 'success', ?, ?, 2, ?)`,
      ).bind(runId, Math.floor(SAME_STARTED_AT_MS / 1_000), runId),
    ]);

    const runs = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=10");

    expect(runs.items.map((item) => item.trace_ref).sort()).toEqual([
      "v1:81",
      `v2:${runId}`,
    ]);
    expect(runs.items.find((item) => item.trace_ref === "v1:81")).toMatchObject({
      user_id: 8,
      title: "Colliding V1 history",
      source_schema: 1,
    });
  });

  it("does not resurrect an expired V2 projection as V1 after retention", async () => {
    const expiredRunId = "019d9c40-7b3c-7000-8000-000000000099";
    const expiredStartedAtMs = FIXED_NOW_MS - 181 * 24 * 60 * 60 * 1_000;
    await seedSimpleRun(expiredRunId, "success", expiredStartedAtMs, null, 1);
    await env.DB.batch([
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at,
            source_schema, trace_run_id)
         VALUES (71, 7, 'Alice Zhang', 'simple', 'Projected V2 copy', 'success', ?, ?, 2, ?)`,
      ).bind(expiredRunId, Math.floor(expiredStartedAtMs / 1_000), expiredRunId),
      env.DB.prepare(
        `INSERT INTO usage_logs
           (id, api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
         VALUES (72, 7, 'Alice Zhang', 'legacy', 'Unrelated legacy run', 'failed', 'legacy-72', ?)`,
      ).bind(Math.floor(expiredStartedAtMs / 1_000)),
    ]);
    const overviewPath = `/api/usage-logs/v2/overview?from=${expiredStartedAtMs - 1_000}&to=${expiredStartedAtMs + 1_000}&bucket=hour`;

    const beforeRuns = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=10");
    const beforeUsers = await readPage<TraceUserSummaryV2>("/api/usage-logs/v2/users?limit=10");
    const beforeOverview = await (await adminGet(overviewPath)).json() as any;
    expect(beforeRuns.items.map((item) => item.trace_ref).sort()).toEqual([
      "v1:72",
      `v2:${expiredRunId}`,
    ]);
    expect(beforeUsers.items[0]).toMatchObject({ operation_count: 2, failed_count: 1 });
    expect(beforeOverview.totals).toMatchObject({ operations: 2, failed: 1 });

    await purgeExpiredTraceData(env.DB, FIXED_NOW_MS);

    const afterRuns = await readPage<TraceRunSummaryV2>("/api/usage-logs/v2/runs?limit=10");
    const afterUsers = await readPage<TraceUserSummaryV2>("/api/usage-logs/v2/users?limit=10");
    const afterOverview = await (await adminGet(overviewPath)).json() as any;
    expect(afterRuns.items.map((item) => item.trace_ref)).toEqual(["v1:72"]);
    expect(afterUsers.items[0]).toMatchObject({ operation_count: 1, failed_count: 1 });
    expect(afterOverview.totals).toMatchObject({ operations: 1, failed: 1 });
  });

  it("searches complete run and event operational fields and applies exact filters", async () => {
    await seedRichTrace();
    const searches = [
      "alice",
      "Alice Zhang",
      SUCCESS_RUN_ID,
      SUCCESS_EVENT_ID,
      "boot_a",
      "LOCKED_DEVICE",
      "9A7F23BC10D4",
      "C:\\source-only\\vbmeta.img",
      "downloads.example/boot.img",
      "VIVO line flash",
      "fastboot_flash",
    ];

    for (const q of searches) {
      const page = await readPage<TraceRunSummaryV2>(
        `/api/usage-logs/v2/runs?q=${encodeURIComponent(q)}`,
      );
      expect(page.items.map((item) => item.trace_ref), q).toContain(`v2:${SUCCESS_RUN_ID}`);
    }

    const filtered = await readPage<TraceRunSummaryV2>(
      `/api/usage-logs/v2/runs?userId=7&kind=fastboot_flash&status=failed&partition=boot_a&errorCode=LOCKED_DEVICE&from=${SAME_STARTED_AT_MS - 1}&to=${SAME_STARTED_AT_MS + 3_000}`,
    );
    expect(filtered.items.map((item) => item.trace_ref)).toEqual([`v2:${SUCCESS_RUN_ID}`]);
  });

  it("returns stable V2 run and event detail contracts", async () => {
    await seedRichTrace();

    const detailResponse = await adminGet(`/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}`);
    const detail = await detailResponse.json() as TraceRunDetailV2;
    const eventResponse = await adminGet(
      `/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}/events/${SUCCESS_EVENT_ID}`,
    );

    expect(detailResponse.status).toBe(200);
    expect(detail).toMatchObject({
      source_schema: adminRunSuccessFixture.source_schema,
      detail_available: adminRunSuccessFixture.detail_available,
      detail_unavailable_reason: adminRunSuccessFixture.detail_unavailable_reason,
      run: { ...adminRunSuccessFixture.run, outcome: "failed" },
    });
    expect(detail.events[0]).toMatchObject({
      event_id: SUCCESS_EVENT_ID,
      command: {
        program: "fastboot.exe",
        argv: ["flash", "boot_a", "C:\\firmware\\boot.img"],
        display_command: "fastboot flash boot_a C:\\firmware\\boot.img",
        working_directory: "C:/nwflash",
        paths: ["C:\\firmware\\boot.img"],
        urls: ["https://downloads.example/boot.img"],
        serial: "9A7F23BC10D4",
      },
    });
    expect(eventResponse.status).toBe(200);
    expect(await eventResponse.json()).toMatchObject({
      run: { trace_ref: `v2:${SUCCESS_RUN_ID}` },
      event: { event_id: SUCCESS_EVENT_ID, partition_name: "boot_a" },
    });
  });

  it("round-trips at most one hundred deterministic event redaction counts", async () => {
    await seedRichTrace();
    const counts = [
      { kind: "password", count: 1 },
      ...Array.from({ length: 99 }, (_, index) => ({ kind: `client-${index}`, count: 1 })),
    ];
    await env.DB.prepare(
      "UPDATE usage_operation_events SET credential_redactions_json = ? WHERE event_id = ?",
    ).bind(JSON.stringify(counts), SUCCESS_EVENT_ID).run();

    const response = await adminGet(
      `/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}/events/${SUCCESS_EVENT_ID}`,
    );
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.event.credential_redactions).toEqual(counts);
    expect(body.event.credential_redactions).toHaveLength(100);
  });

  it("pages complete output and records an audit row before every raw output response", async () => {
    await seedOutputTrace();

    const firstResponse = await adminGet(
      `/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}/events/${OUTPUT_EVENT_ID}/output?stream=stdout&limit=2`,
    );
    const first = await firstResponse.json() as TraceOutputPageV2;
    const secondResponse = await adminGet(
      `/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}/events/${OUTPUT_EVENT_ID}/output?stream=stdout&afterChunk=1&limit=2`,
    );
    const second = await secondResponse.json() as TraceOutputPageV2;

    expect(firstResponse.status).toBe(200);
    expect(first.chunks.map((chunk) => chunk.chunk_index)).toEqual([0, 1]);
    expect(first.next_after_chunk).toBe(1);
    expect(first.output_complete).toBe(false);
    expect(secondResponse.status).toBe(200);
    expect(second.chunks.map((chunk) => chunk.chunk_index)).toEqual([2]);
    expect(second.next_after_chunk).toBeNull();
    expect(second.output_complete).toBe(true);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'view_trace_output'",
    )).toBe(2);
  });

  it("does not return output when its mandatory audit write fails", async () => {
    await seedOutputTrace();
    await env.DB.prepare(
      `CREATE TRIGGER reject_output_audit
       BEFORE INSERT ON admin_audit_log
       WHEN NEW.action = 'view_trace_output'
       BEGIN SELECT RAISE(ABORT, 'forced output audit failure'); END`,
    ).run();

    const response = await adminGet(
      `/api/usage-logs/v2/runs/v2%3A${SUCCESS_RUN_ID}/events/${OUTPUT_EVENT_ID}/output?stream=stdout`,
    );
    const body = await response.json() as any;

    expect(response.status).toBe(500);
    expect(body).toMatchObject({ ok: false, error: { code: "TRACE_INTERNAL" } });
    expect(JSON.stringify(body)).not.toContain("stdout line");
  });

  it("exports the current run filters as audited NDJSON", async () => {
    await seedRichTrace();
    await seedSimpleRun("019d9c40-7b3c-7000-8000-000000000099", "success", SAME_STARTED_AT_MS - 1);

    const response = await adminGet(
      "/api/usage-logs/v2/export?status=failed&partition=boot_a&errorCode=LOCKED_DEVICE",
    );
    const exportText = new TextDecoder().decode(await response.arrayBuffer());
    const lines = exportText.trim().split("\n").map((line) => JSON.parse(line));

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toContain("application/x-ndjson");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(lines).toEqual([expect.objectContaining({ trace_ref: `v2:${SUCCESS_RUN_ID}` })]);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'export_trace'",
    )).toBe(1);
  });

  it("writes the export audit before attempting to read matching trace rows", async () => {
    await env.DB.exec("DROP TABLE usage_logs");

    const response = await adminGet("/api/usage-logs/v2/export?status=failed");

    expect(response.status).toBe(500);
    expect(response.headers.get("content-type")).toContain("application/json");
    expect(await response.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_INTERNAL" },
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'export_trace'",
    )).toBe(1);
  });

  it("does not return NDJSON when the mandatory export audit write fails", async () => {
    await seedSimpleRun(SUCCESS_RUN_ID, "failed", SAME_STARTED_AT_MS);
    await env.DB.prepare(
      `CREATE TRIGGER reject_export_audit
       BEFORE INSERT ON admin_audit_log
       WHEN NEW.action = 'export_trace'
       BEGIN SELECT RAISE(ABORT, 'forced export audit failure'); END`,
    ).run();

    const response = await adminGet("/api/usage-logs/v2/export?status=failed");
    const body = await response.json() as any;

    expect(response.status).toBe(500);
    expect(response.headers.get("content-type")).toContain("application/json");
    expect(body).toMatchObject({ ok: false, error: { code: "TRACE_INTERNAL" } });
    expect(JSON.stringify(body)).not.toContain(SUCCESS_RUN_ID);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'export_trace'",
    )).toBe(0);
  });

  it("streams exports across bounded keyset batches without gaps or duplicate runs", async () => {
    await seedRunsAtSameTimestamp(205, SAME_STARTED_AT_MS);

    const response = await adminGet("/api/usage-logs/v2/export");
    const text = new TextDecoder().decode(await response.arrayBuffer());
    const rows = text.trim().split("\n").map((line) => JSON.parse(line) as TraceRunSummaryV2);

    expect(response.status).toBe(200);
    expect(rows).toHaveLength(205);
    expect(new Set(rows.map((row) => row.trace_ref)).size).toBe(205);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'export_trace'",
    )).toBe(1);
  });

  it("returns authoritative overview and UTC-day app version summary", async () => {
    const dayStart = utcDayStartMs(FIXED_NOW_MS);
    await seedSimpleRun(SUCCESS_RUN_ID, "failed", dayStart + 3_600_000, "UPDATE_REQUIRED");
    await seedSimpleRun("019d9c40-7b3c-7000-8000-000000000020", "success", dayStart + 7_200_000);
    await seedSimpleRun("019d9c40-7b3c-7000-8000-000000000021", "failed", dayStart - 1, "UPDATE_REQUIRED");
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO app_versions (version, min_version, enabled) VALUES ('1.4.0', '1.2.0', 1)",
      ),
      env.DB.prepare(
        "INSERT INTO app_versions (version, min_version, enabled) VALUES ('2.0.0', '1.5.0', 1)",
      ),
      env.DB.prepare(
        "INSERT INTO app_versions (version, min_version, enabled) VALUES ('3.0.0', '3.0.0', 0)",
      ),
      env.DB.prepare(
        `INSERT INTO online_sessions
           (session_id, user_id, user_name, connected_at, last_seen_at)
         VALUES ('active', 7, 'Alice Zhang', ?, ?)`,
      ).bind(Math.floor(FIXED_NOW_MS / 1000) - 60, Math.floor(FIXED_NOW_MS / 1000) - 5),
      env.DB.prepare(
        `INSERT INTO online_sessions
           (session_id, user_id, user_name, connected_at, last_seen_at)
         VALUES ('stale', 7, 'Alice Zhang', ?, ?)`,
      ).bind(Math.floor(FIXED_NOW_MS / 1000) - 600, Math.floor(FIXED_NOW_MS / 1000) - 600),
    ]);

    const overviewResponse = await adminGet(
      `/api/usage-logs/v2/overview?from=${dayStart}&to=${dayStart + 86_400_000 - 1}&bucket=hour`,
    );
    const overview = await overviewResponse.json() as any;
    const summaryResponse = await adminGet("/api/app-versions/summary");
    const summary = await summaryResponse.json() as any;

    expect(overviewResponse.status).toBe(200);
    expect(overview.totals).toEqual({ api_users: 1, online_sessions: 1, operations: 2, failed: 1 });
    expect(overview.trend).toEqual([
      { bucket_start_ms: dayStart + 3_600_000, operations: 1, failed: 1 },
      { bucket_start_ms: dayStart + 7_200_000, operations: 1, failed: 0 },
    ]);
    expect(overview.recent_failures[0]).toMatchObject({ trace_ref: `v2:${SUCCESS_RUN_ID}` });
    expect(summaryResponse.status).toBe(200);
    expect(summary).toEqual({
      current_version: "2.0.0",
      minimum_version: "1.5.0",
      supported_versions: ["2.0.0", "1.4.0"],
      today_426: 1,
      as_of_ms: FIXED_NOW_MS,
    });
  });

  it("uses the admission policy's numeric version precedence for equivalent version spellings", async () => {
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO app_versions (version, min_version, enabled) VALUES ('2.0', '1.0.0', 1)",
      ),
      env.DB.prepare(
        "INSERT INTO app_versions (version, min_version, enabled) VALUES ('2.0.0', '1.5.0', 1)",
      ),
    ]);

    const response = await adminGet("/api/app-versions/summary");
    const summary = await response.json() as any;

    expect(response.status).toBe(200);
    expect(summary.current_version).toBe("2.0");
    expect(summary.minimum_version).toBe("1.0.0");
    expect(summary.supported_versions).toEqual(["2.0", "2.0.0"]);
  });

  it("rejects summary PUT and DELETE with the frozen envelope without mutating versions", async () => {
    await env.DB.prepare(
      "INSERT INTO app_versions (id, version, min_version, enabled) VALUES (21, '2.0.0', '1.5.0', 1)",
    ).run();

    for (const method of ["PUT", "DELETE"] as const) {
      const response = await adminRequest("/api/app-versions/summary", {
        method,
        headers: { "X-Requested-With": "XMLHttpRequest", "Content-Type": "application/json" },
        body: method === "PUT" ? JSON.stringify({ enabled: false }) : undefined,
      });

      expect(response.status, method).toBe(405);
      expect(response.headers.get("cache-control"), method).toBe("no-store");
      expect(await response.json(), method).toMatchObject({
        ok: false,
        error: { code: "TRACE_INVALID", request_id: expect.any(String) },
      });
      expect(await scalar("SELECT COUNT(*) AS value FROM app_versions WHERE id = 21 AND enabled = 1"), method).toBe(1);
    }
  });

  it("returns keyset-paged ROM logs with complete URLs and explicit legacy failure degradation", async () => {
    const createdAt = "2026-08-26 12:00:00";
    await env.DB.batch([
      seedRomLog(1, 500, "PD2405", "1.0.0", "https://rom.example/full/path?token=public", createdAt),
      seedRomLog(2, 200, "PD2405", "1.0.0", "https://rom.example/success", createdAt),
      seedRomLog(3, 404, "PD9999", "2.0.0", null, createdAt),
    ]);

    const first = await readPage<RomLogAdminRowV2>(
      "/api/rom-logs/v2?userId=7&pd=PD2405&version=1.0.0&q=rom.example&limit=1",
    );
    const second = await readPage<RomLogAdminRowV2>(
      `/api/rom-logs/v2?userId=7&pd=PD2405&version=1.0.0&q=rom.example&limit=1&cursor=${encodeURIComponent(first.next_cursor!)}`,
    );
    const failure = [...first.items, ...second.items].find((item) => item.status === 500)!;
    const success = [...first.items, ...second.items].find((item) => item.status === 200)!;

    expect(new Set([...first.items, ...second.items].map((item) => item.id)).size).toBe(2);
    expect(first.next_cursor).not.toBeNull();
    expect(second.next_cursor).toBeNull();
    expect(failure.url).toBe("https://rom.example/full/path?token=public");
    expect(failure.failure_reason).toBeNull();
    expect(failure.detail_unavailable_reason).toBe("legacy_record_no_failure_reason");
    expect(success.detail_unavailable_reason).toBeNull();
  });

  it("uses the frozen no-store error envelope for auth and validation failures", async () => {
    const unauthorized = await adminWorker.fetch(
      new Request("https://web.nwflash.cc.cd/api/usage-logs/v2/runs"),
      env,
    );
    const invalid = await adminGet("/api/usage-logs/v2/runs?cursor=not-base64url");

    expect(unauthorized.status).toBe(401);
    expect(unauthorized.headers.get("cache-control")).toBe("no-store");
    expect(await unauthorized.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_UNAUTHORIZED", request_id: expect.any(String) },
    });
    expect(invalid.status).toBe(400);
    expect(await invalid.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_INVALID", request_id: expect.any(String) },
    });
  });

  it("uses the frozen envelope for CSRF failures and the exact V2 route root", async () => {
    const csrf = await adminWorker.fetch(
      new Request("https://web.nwflash.cc.cd/api/usage-logs/v2/runs", {
        method: "POST",
        headers: { Cookie: `nwflash_session=${ADMIN_SESSION_TOKEN}` },
      }),
      env,
    );
    const root = await adminWorker.fetch(
      new Request("https://web.nwflash.cc.cd/api/usage-logs/v2"),
      env,
    );

    expect(csrf.status).toBe(403);
    expect(await csrf.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_FORBIDDEN", request_id: expect.any(String) },
    });
    expect(root.status).toBe(401);
    expect(await root.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_UNAUTHORIZED", request_id: expect.any(String) },
    });
  });

  it("enforces the 50-byte escaped LIKE pattern boundary on every q endpoint", async () => {
    const escapedBoundary = "%_\\".repeat(8);
    const cases = [
      { route: "/api/usage-logs/v2/runs", accepted: "a".repeat(48), rejected: "a".repeat(49) },
      { route: "/api/usage-logs/v2/users", accepted: "查".repeat(16), rejected: "查".repeat(17) },
      { route: "/api/rom-logs/v2", accepted: escapedBoundary, rejected: `${escapedBoundary}%` },
      { route: "/api/usage-logs/v2/export", accepted: `${"b".repeat(46)}%`, rejected: `${"b".repeat(47)}%` },
    ];

    for (const testCase of cases) {
      expect(escapedLikePatternBytes(testCase.accepted), `${testCase.route} accepted bytes`).toBe(50);
      expect(escapedLikePatternBytes(testCase.rejected), `${testCase.route} rejected bytes`).toBeGreaterThan(50);

      const accepted = await adminGet(`${testCase.route}?q=${encodeURIComponent(testCase.accepted)}`);
      expect(accepted.status, `${testCase.route} accepted`).toBe(200);
      expect(accepted.headers.get("cache-control"), `${testCase.route} accepted`).toBe("no-store");

      const rejected = await adminGet(`${testCase.route}?q=${encodeURIComponent(testCase.rejected)}`);
      expect(rejected.status, `${testCase.route} rejected`).toBe(400);
      expect(rejected.headers.get("cache-control"), `${testCase.route} rejected`).toBe("no-store");
      expect(await rejected.json(), `${testCase.route} rejected`).toMatchObject({
        ok: false,
        error: { code: "TRACE_INVALID", request_id: expect.any(String) },
      });
    }

    expect(await scalar(
      "SELECT COUNT(*) AS value FROM admin_audit_log WHERE action = 'export_trace'",
    )).toBe(1);
  });
});

async function adminGet(path: string): Promise<Response> {
  return adminRequest(path);
}

async function adminRequest(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  headers.set("Cookie", `nwflash_session=${ADMIN_SESSION_TOKEN}`);
  return adminWorker.fetch(new Request(`https://web.nwflash.cc.cd${path}`, { ...init, headers }), env);
}

async function readPage<T>(path: string): Promise<KeysetPageV2<T>> {
  const response = await adminGet(path);
  expect(response.status).toBe(200);
  expect(response.headers.get("cache-control")).toBe("no-store");
  return response.json() as Promise<KeysetPageV2<T>>;
}

async function seedRunsAtSameTimestamp(count: number, startedAtMs: number): Promise<void> {
  const statements = Array.from({ length: count }, (_, index) => {
    const suffix = (index + 1).toString().padStart(12, "0");
    return env.DB.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          client_version, started_at_ms, trace_complete)
       VALUES (?, 7, 'Alice Zhang', 2, 'fastboot_flash', ?, 'success', '1.4.0', ?, 0)`,
    ).bind(`019d9c40-7b3c-7000-8000-${suffix}`, `Run ${index + 1}`, startedAtMs);
  });
  for (let offset = 0; offset < statements.length; offset += 80) {
    await env.DB.batch(statements.slice(offset, offset + 80));
  }
}

async function seedSuccessTrace(): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          client_version, started_at_ms, ended_at_ms, duration_ms, final_sequence, trace_complete)
       VALUES (?, 7, 'Alice Zhang', 2, 'fastboot_flash', 'VIVO line flash', 'success',
               '1.4.0', 1787500000000, 1787500002500, 2500, 1, 0)`,
    ).bind(SUCCESS_RUN_ID),
    env.DB.prepare(
      `INSERT INTO usage_operation_events
         (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms, ended_at_ms,
          duration_ms, stdout_chunks, stderr_chunks, verification)
       VALUES (?, ?, 1, 'authorization', 'Authorization', 'success', 1787500000000,
               1787500000100, 100, 0, 0, 'Bearer authentication accepted')`,
    ).bind(SUCCESS_EVENT_ID, SUCCESS_RUN_ID),
    env.DB.prepare("UPDATE usage_operation_runs SET trace_complete = 1 WHERE run_id = ?").bind(SUCCESS_RUN_ID),
  ]);
}

async function seedRichTrace(): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          device_serial, source_ip, source_paths_json, source_urls_json, client_version,
          started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
          final_sequence, trace_complete)
       VALUES (?, 7, 'Alice Zhang', 2, 'fastboot_flash', 'VIVO line flash', 'failed',
               '9A7F23BC10D4', '203.0.113.45', '["C:\\\\source-only\\\\vbmeta.img"]',
               '["https://downloads.example/boot.img"]', '1.4.0', ?, ?, 2500,
               'fastboot_remote', 'LOCKED_DEVICE', 'Flashing is not allowed in Lock State', 1, 0)`,
    ).bind(SUCCESS_RUN_ID, SAME_STARTED_AT_MS, SAME_STARTED_AT_MS + 2_500),
    env.DB.prepare(
      `INSERT INTO usage_operation_events
         (event_id, run_id, sequence, event_kind, step_name, partition_name, status,
          started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
          command_line, working_directory, paths_json, urls_json, serial, exit_code,
          stdout_chunks, stderr_chunks, verification, device_state, retry_safe,
          remedies_json, error_class, error_code, error_message, credential_redactions_json)
       VALUES (?, ?, 1, 'command', 'Flash boot_a', 'boot_a', 'failed', ?, ?, 2500,
               'fastboot.exe', '["flash","boot_a","C:\\\\firmware\\\\boot.img"]',
               'fastboot flash boot_a C:\\firmware\\boot.img', 'C:/nwflash',
               '["C:\\\\firmware\\\\boot.img"]', '["https://downloads.example/boot.img"]',
               '9A7F23BC10D4', 1, 0, 0, 'locked device verification', 'fastboot', 1,
               '["Unlock the device before retrying the flash."]', 'fastboot_remote',
               'LOCKED_DEVICE', 'Flashing is not allowed in Lock State', '[]')`,
    ).bind(SUCCESS_EVENT_ID, SUCCESS_RUN_ID, SAME_STARTED_AT_MS, SAME_STARTED_AT_MS + 2_500),
    env.DB.prepare("UPDATE usage_operation_runs SET trace_complete = 1 WHERE run_id = ?").bind(SUCCESS_RUN_ID),
  ]);
}

async function seedOutputTrace(): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          client_version, started_at_ms, ended_at_ms, duration_ms, final_sequence, trace_complete)
       VALUES (?, 7, 'Alice Zhang', 2, 'command', 'Output trace', 'success',
               '1.4.0', ?, ?, 100, 1, 0)`,
    ).bind(SUCCESS_RUN_ID, SAME_STARTED_AT_MS, SAME_STARTED_AT_MS + 100),
    env.DB.prepare(
      `INSERT INTO usage_operation_events
         (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms, ended_at_ms,
          duration_ms, stdout_chunks, stderr_chunks)
       VALUES (?, ?, 1, 'command', 'Capture output', 'success', ?, ?, 100, 3, 0)`,
    ).bind(OUTPUT_EVENT_ID, SUCCESS_RUN_ID, SAME_STARTED_AT_MS, SAME_STARTED_AT_MS + 100),
    ...[0, 1, 2].map((index) => env.DB.prepare(
      `INSERT INTO usage_output_chunks
         (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256)
       VALUES (?, ?, 'stdout', ?, ?, ?, ?)`,
    ).bind(
      `019d9c40-7b3c-7000-8000-${(index + 30).toString().padStart(12, "0")}`,
      OUTPUT_EVENT_ID,
      index,
      `stdout line ${index}`,
      new TextEncoder().encode(`stdout line ${index}`).byteLength,
      "0".repeat(64),
    )),
    env.DB.prepare("UPDATE usage_operation_runs SET trace_complete = 1 WHERE run_id = ?").bind(SUCCESS_RUN_ID),
  ]);
}

async function seedSimpleRun(
  runId: string,
  outcome: "success" | "failed",
  startedAtMs: number,
  errorCode: string | null = null,
  traceComplete = 0,
): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        client_version, started_at_ms, error_code, trace_complete)
     VALUES (?, 7, 'Alice Zhang', 2, 'simple', 'Simple run', ?, '1.4.0', ?, ?, ?)`,
  ).bind(runId, outcome, startedAtMs, errorCode, traceComplete).run();
}

function seedRomLog(
  id: number,
  status: number,
  pd: string,
  version: string,
  url: string | null,
  createdAt: string,
): D1PreparedStatement {
  return env.DB.prepare(
    `INSERT INTO access_logs (id, api_user_id, api_user_name, pd, version, url, status, created_at)
     VALUES (?, 7, 'Alice Zhang', ?, ?, ?, ?, ?)`,
  ).bind(id, pd, version, url, status, createdAt);
}

async function scalar(query: string, ...bindings: unknown[]): Promise<number> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

function utcDayStartMs(nowMs: number): number {
  const now = new Date(nowMs);
  return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
}

function escapedLikePatternBytes(value: string): number {
  const escaped = value.replace(/[\\%_]/g, (character) => `\\${character}`);
  return new TextEncoder().encode(`%${escaped}%`).byteLength;
}
