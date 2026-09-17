import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import successAckFixture from "../contracts/trace-v2/upload-ack.success.json";
import chunkOnlyFixture from "../contracts/trace-v2/upload.chunk-only.json";
import eventOnlyFixture from "../contracts/trace-v2/upload.event-only.json";
import finalizeOnlyFixture from "../contracts/trace-v2/upload.finalize-only.json";
import openFixture from "../contracts/trace-v2/upload.open.json";
import successFixture from "../contracts/trace-v2/upload.success.json";
import worker, { type Env as WorkerEnv } from "../src/index";
import { encodePersistedRunSnapshotsForGuard, ingestTraceUploadV2 } from "../src/trace-v2-ingest";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

describe("POST /api/usage/traces/v2", () => {
  it("rejects finalization after scheduled retention seals an open run", async () => {
    const nowMs = Date.UTC(2026, 7, 27, 12, 0, 0);
    const startedAtMs = nowMs - 31 * 24 * 60 * 60 * 1_000;
    const sensitive = "sealed-terminal-sensitive-marker";
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      device_serial: sensitive,
      source_paths: [`C:\\private\\${sensitive}.img`],
      source_urls: [`https://example.invalid/${sensitive}`],
      started_at_ms: startedAtMs,
      ended_at_ms: null,
      duration_ms: null,
      error_class: null,
      error_code: null,
      error_message: sensitive,
      final_sequence: null,
      trace_complete: false,
    };
    const persistedEvent = {
      ...canonical.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004801",
      run_id: openRun.run_id,
      started_at_ms: startedAtMs,
      ended_at_ms: startedAtMs + 1,
      duration_ms: 1,
      verification: sensitive,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(persistedEvent);
    vi.spyOn(Date, "now").mockReturnValue(nowMs);
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await worker.scheduled({} as ScheduledEvent, env, {} as ExecutionContext);

    expect(await scalar(
      "SELECT retention_detail_cleared AS value FROM usage_operation_runs WHERE run_id = ?",
      openRun.run_id,
    )).toBe(1);
    const terminalRun = {
      ...openRun,
      outcome: "failed",
      device_serial: null,
      source_paths: [],
      source_urls: [],
      ended_at_ms: startedAtMs + 2,
      duration_ms: 2,
      error_class: "TerminalError",
      error_code: "SEALED_ATTEMPT",
      error_message: sensitive,
      final_sequence: 1,
      trace_complete: true,
    };
    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004802",
      runs: [terminalRun],
      events: [],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { runs: [] },
      rejected: [{
        entity: "run",
        id: openRun.run_id,
        code: "invalid",
        message: expect.stringMatching(/retention_expired.*detail sealed/i),
      }],
    });
    expect(await scalar(
      "SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?",
      openRun.run_id,
    )).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", openRun.run_id)).toBe(0);
    expect(await text(
      "SELECT error_message AS value FROM usage_operation_runs WHERE run_id = ?",
      openRun.run_id,
    )).toBeNull();
    expect(await text(
      "SELECT source_paths_json AS value FROM usage_operation_runs WHERE run_id = ?",
      openRun.run_id,
    )).toBe("[]");
  });

  it("rejects fresh and duplicate child items of a sealed open run", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const persistedEvent = {
      ...canonical.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004811",
      run_id: openRun.run_id,
      stdout_chunks: 2,
    };
    const persistedChunk = {
      ...canonical.output_chunks[1],
      chunk_id: "019d9c40-7b3c-7000-8000-000000004812",
      event_id: persistedEvent.event_id,
      stream: "stdout",
      chunk_index: 0,
    };
    const freshEvent = {
      ...canonical.events[2],
      event_id: "019d9c40-7b3c-7000-8000-000000004813",
      run_id: openRun.run_id,
      sequence: 2,
    };
    const freshChunk = {
      ...persistedChunk,
      chunk_id: "019d9c40-7b3c-7000-8000-000000004814",
      chunk_index: 1,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(persistedEvent);
    await seedChunkFromPayload(persistedChunk);
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = ?",
    ).bind(openRun.run_id).run();

    const requests = [
      { entity: "event", id: persistedEvent.event_id, events: [persistedEvent], output_chunks: [] },
      { entity: "event", id: freshEvent.event_id, events: [freshEvent], output_chunks: [] },
      { entity: "output_chunk", id: persistedChunk.chunk_id, events: [], output_chunks: [persistedChunk] },
      { entity: "output_chunk", id: freshChunk.chunk_id, events: [], output_chunks: [freshChunk] },
    ] as const;
    for (const [index, candidate] of requests.entries()) {
      const response = await postTrace({
        schema_version: 2,
        upload_id: `019d9c40-7b3c-7000-8000-${(4_820 + index).toString().padStart(12, "0")}`,
        runs: [],
        events: candidate.events,
        output_chunks: candidate.output_chunks,
      }, "trace-bearer", "203.0.113.45");
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({
        accepted: candidate.entity === "event" ? { events: [] } : { output_chunks: [] },
        rejected: [{
          entity: candidate.entity,
          id: candidate.id,
          code: "invalid",
          message: expect.stringMatching(/retention_expired.*detail sealed/i),
        }],
      });
    }
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events WHERE run_id = ?", openRun.run_id)).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks WHERE event_id = ?", persistedEvent.event_id)).toBe(1);
  });

  it("reclassifies a retention seal race as an item rejection", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const freshEvent = {
      ...canonical.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004831",
      run_id: openRun.run_id,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt !== 1) return;
      await env.DB.prepare(
        "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = ?",
      ).bind(openRun.run_id).run();
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest({
        schema_version: 2,
        upload_id: "019d9c40-7b3c-7000-8000-000000004832",
        runs: [],
        events: [freshEvent],
        output_chunks: [],
      }, "trace-bearer", "203.0.113.45"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{
        entity: "event",
        id: freshEvent.event_id,
        code: "invalid",
        message: expect.stringMatching(/retention_expired.*detail sealed/i),
      }],
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE event_id = ?",
      freshEvent.event_id,
    )).toBe(0);
  });

  it("keeps completed marker-one exact retries idempotent", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    await env.DB.prepare(
      "UPDATE usage_operation_events SET retention_detail_cleared = 1 WHERE run_id = ?",
    ).bind(successFixture.runs[0].run_id).run();
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = ?",
    ).bind(successFixture.runs[0].run_id).run();

    const retry = await postTrace(successFixture, "trace-bearer", "198.51.100.77");

    expect(retry.status).toBe(200);
    expect(await retry.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", successFixture.runs[0].run_id)).toBe(1);
  });

  it("acks the canonical upload and projects one terminal V1 summary", async () => {
    await seedUser("trace-bearer", 7);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await response.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs WHERE api_user_id = 7")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", successFixture.runs[0].run_id)).toBe(1);
    expect(await text("SELECT source_ip AS value FROM usage_operation_runs WHERE run_id = ?", successFixture.runs[0].run_id)).toBe("203.0.113.45");
  });

  it("keeps a cross-user V1 event key while creating an owner-bound V2 projection", async () => {
    await seedUser("trace-bearer", 7);
    const runId = successFixture.runs[0].run_id;
    await env.DB.prepare(
      `INSERT INTO usage_logs
         (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at)
       VALUES (8, 'Legacy user', 'legacy', 'Unrelated V1 UUID key', 'failed', ?, ?)`,
    ).bind(runId, Math.floor(successFixture.runs[0].started_at_ms / 1_000)).run();

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");
    const rows = await env.DB.prepare(
      `SELECT api_user_id, source_schema, trace_run_id
       FROM usage_logs WHERE event_key = ? ORDER BY source_schema`,
    ).bind(runId).all<{ api_user_id: number; source_schema: number; trace_run_id: string | null }>();

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(rows.results).toEqual([
      { api_user_id: 8, source_schema: 1, trace_run_id: null },
      { api_user_id: 7, source_schema: 2, trace_run_id: runId },
    ]);
  });

  it("keeps legacy usage-log retries idempotent with the V1 partial unique index", async () => {
    await seedUser("trace-bearer", 7);
    const eventKey = "019d9c40-7b3c-7000-8000-000000000089";
    const payload = {
      logs: [{
        operation: "legacy",
        title: "Legacy idempotent retry",
        status: "success",
        event_id: eventKey,
        started_at: 1_787_500_000,
      }],
    };

    const first = await postLegacyUsage(payload, "trace-bearer");
    const retry = await postLegacyUsage(payload, "trace-bearer");

    expect([first.status, retry.status]).toEqual([200, 200]);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ? AND source_schema = 1",
      eventKey,
    )).toBe(1);
  });

  it("persists the frozen open to child-only to finalize-only fixture chain", async () => {
    await seedUser("trace-bearer", 7);

    const responses = [];
    for (const fixture of [openFixture, eventOnlyFixture, chunkOnlyFixture, finalizeOnlyFixture]) {
      responses.push(await postTrace(fixture, "trace-bearer", "203.0.113.45"));
    }

    expect(responses.map((response) => response.status)).toEqual([200, 200, 200, 200]);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", openFixture.runs[0].run_id)).toBe(1);
  });

  it("accepts an event-only upload that references a same-user persisted run", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const runResponse = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000100",
      runs: [openRun],
      events: [],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    expect(runResponse.status).toBe(200);
    const event = canonical.events[0];

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000101",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      ok: true,
      accepted: { runs: [], events: [event.event_id], output_chunks: [] },
      rejected: [],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
  });

  it("rejects event metadata that would exceed the persisted run storage quota", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const remedies = Array.from({ length: 57 }, () => "x".repeat(16_384));
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    for (let sequence = 1; sequence <= 8; sequence += 1) {
      await seedEventFromPayload({
        ...canonical.events[0],
        event_id: `019d9c40-7b3c-7000-8000-${(4_400 + sequence).toString().padStart(12, "0")}`,
        sequence,
        remedies,
      });
    }
    const event = {
      ...canonical.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004409",
      sequence: 9,
      remedies,
    };
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004410",
      runs: [],
      events: [event],
      output_chunks: [],
    };
    expect(new TextEncoder().encode(JSON.stringify(payload)).byteLength).toBeLessThan(1_048_576);

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: event.event_id, code: "invalid" }],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(8);
  });

  it("accepts the exact eight MiB persisted-plus-current event metadata boundary", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const run = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004510");
    const event = quotaEvent(
      canonical.events[0],
      run.run_id,
      "019d9c40-7b3c-7000-8000-000000004511",
      10,
    );
    const probeRun = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004512");
    const probeEvent = quotaEvent(
      canonical.events[0],
      probeRun.run_id,
      "019d9c40-7b3c-7000-8000-000000004513",
      10,
    );
    await seedRunFromPayload(7, probeRun, "203.0.113.45");
    await seedEventFromPayload(probeEvent);
    const freshEventBytes = await eventStorageBytes(probeRun.run_id);

    await seedRunFromPayload(7, run, "203.0.113.45");
    await seedRunToEventStorageBytes(run.run_id, 8_388_608 - freshEventBytes, canonical.events[0], 4_520);

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004514",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [event.event_id] },
      rejected: [],
    });
    expect(await eventStorageBytes(run.run_id)).toBe(8_388_608);
  });

  it("item-rejects one byte beyond the eight MiB persisted-plus-current boundary", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const run = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004610");
    const event = quotaEvent(
      canonical.events[0],
      run.run_id,
      "019d9c40-7b3c-7000-8000-000000004611",
      10,
    );
    const probeRun = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004612");
    const probeEvent = quotaEvent(
      canonical.events[0],
      probeRun.run_id,
      "019d9c40-7b3c-7000-8000-000000004613",
      10,
    );
    await seedRunFromPayload(7, probeRun, "203.0.113.45");
    await seedEventFromPayload(probeEvent);
    const freshEventBytes = await eventStorageBytes(probeRun.run_id);

    await seedRunFromPayload(7, run, "203.0.113.45");
    await seedRunToEventStorageBytes(run.run_id, 8_388_608 - freshEventBytes + 1, canonical.events[0], 4_620);

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004614",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: event.event_id, code: "invalid" }],
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE run_id = ?",
      run.run_id,
    )).toBe(9);
  });

  it("reclassifies an atomic event-storage race as an item-level quota rejection", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const run = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004710");
    const event = quotaEvent(
      canonical.events[0],
      run.run_id,
      "019d9c40-7b3c-7000-8000-000000004711",
      10,
    );
    const racingEvent = quotaEvent(
      canonical.events[0],
      run.run_id,
      "019d9c40-7b3c-7000-8000-000000004712",
      11,
    );
    const probeRun = openRunForQuota(canonical.runs[0], "019d9c40-7b3c-7000-8000-000000004713");
    await seedRunFromPayload(7, probeRun, "203.0.113.45");
    await seedEventFromPayload({
      ...event,
      event_id: "019d9c40-7b3c-7000-8000-000000004715",
      run_id: probeRun.run_id,
    });
    const freshEventBytes = await eventStorageBytes(probeRun.run_id);

    await seedRunFromPayload(7, run, "203.0.113.45");
    await seedRunToEventStorageBytes(run.run_id, 8_388_608 - freshEventBytes, canonical.events[0], 4_720);
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt === 1) await seedEventFromPayload(racingEvent);
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest({
        schema_version: 2,
        upload_id: "019d9c40-7b3c-7000-8000-000000004714",
        runs: [],
        events: [event],
        output_chunks: [],
      }, "trace-bearer", "203.0.113.45"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: event.event_id, code: "invalid" }],
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE event_id = ?",
      event.event_id,
    )).toBe(0);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE event_id = ?",
      racingEvent.event_id,
    )).toBe(1);
  });

  it("rejects an event-only sequence beyond a persisted open run's known final bound", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: 1,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], sequence: 2, stdout_chunks: 1 };
    const chunk = { ...canonical.output_chunks[0], event_id: event.event_id };
    await seedRunFromPayload(7, openRun, "203.0.113.45");

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004201",
      runs: [],
      events: [event],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [], output_chunks: [] },
      rejected: [
        {
          entity: "event",
          id: event.event_id,
          code: "invalid",
          message: "事件序号超出运行声明的最终边界。",
        },
        { entity: "output_chunk", id: chunk.chunk_id, code: "missing_parent" },
      ],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("accepts a chunk-only upload that references a same-user persisted event", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 1 };
    const chunk = { ...canonical.output_chunks[0], event_id: event.event_id };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(event);

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000102",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      ok: true,
      accepted: { runs: [], events: [], output_chunks: [chunk.chunk_id] },
      rejected: [],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
  });

  it("accepts a gapped partial chunk batch while its run remains open", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 3 };
    const chunk = {
      ...canonical.output_chunks[0],
      event_id: event.event_id,
      chunk_index: 1,
    };

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000110",
      runs: [openRun],
      events: [event],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { runs: [openRun.run_id], events: [event.event_id], output_chunks: [chunk.chunk_id] },
      rejected: [],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs")).toBe(0);
  });

  it("accepts two hundred micro chunks in one bounded request", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 200 };
    const textValue = "x";
    const bytes = new TextEncoder().encode(textValue);
    const hash = await sha256Hex(bytes);
    const chunks = Array.from({ length: 200 }, (_, index) => ({
      chunk_id: `019d9c40-7b3c-7000-8000-${(1_000 + index).toString().padStart(12, "0")}`,
      event_id: event.event_id,
      stream: "stdout",
      chunk_index: index,
      text: textValue,
      byte_count: bytes.byteLength,
      sha256: hash,
    }));
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000001200",
      runs: [openRun],
      events: [event],
      output_chunks: chunks,
    };
    expect(new TextEncoder().encode(JSON.stringify(payload)).byteLength).toBeLessThan(1_048_576);

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { output_chunks: chunks.map((chunk) => chunk.chunk_id) },
      rejected: [],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(200);
  });

  it("reassembles more than one MiB of logical output uploaded across bounded child-only requests", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 40 };
    const texts = Array.from({ length: 40 }, (_, index) => (
      `${index.toString().padStart(2, "0")}:${"x".repeat(32_765)}`
    ));
    const chunks = await Promise.all(texts.map(async (textValue, index) => {
      const bytes = new TextEncoder().encode(textValue);
      return {
        chunk_id: `019d9c40-7b3c-7000-8000-${(2_000 + index).toString().padStart(12, "0")}`,
        event_id: event.event_id,
        stream: "stdout",
        chunk_index: index,
        text: textValue,
        byte_count: bytes.byteLength,
        sha256: await sha256Hex(bytes),
      };
    }));
    expect(texts.reduce((total, textValue) => total + textValue.length, 0)).toBeGreaterThan(1_048_576);
    const requests = Array.from({ length: 4 }, (_, batchIndex) => ({
      schema_version: 2,
      upload_id: `019d9c40-7b3c-7000-8000-${(2_100 + batchIndex).toString().padStart(12, "0")}`,
      runs: batchIndex === 0 ? [openRun] : [],
      events: batchIndex === 0 ? [event] : [],
      output_chunks: chunks.slice(batchIndex * 10, batchIndex * 10 + 10),
    }));
    for (const requestPayload of requests) {
      expect(new TextEncoder().encode(JSON.stringify(requestPayload)).byteLength).toBeLessThan(1_048_576);
      const response = await postTrace(requestPayload, "trace-bearer", "203.0.113.45");
      expect(response.status).toBe(200);
      expect((await response.json() as any).accepted.output_chunks).toEqual(
        requestPayload.output_chunks.map((chunk) => chunk.chunk_id),
      );
    }

    const retry = await postTrace(requests[2], "trace-bearer", "203.0.113.45");
    expect(retry.status).toBe(200);
    expect((await retry.json() as any).accepted.output_chunks).toEqual(
      requests[2].output_chunks.map((chunk) => chunk.chunk_id),
    );
    const terminalRun = { ...canonical.runs[0], final_sequence: 1 };
    const completion = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000002104",
      runs: [terminalRun],
      events: [],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    expect(completion.status).toBe(200);
    expect((await completion.json() as any).accepted.runs).toEqual([terminalRun.run_id]);

    const persisted = await env.DB.prepare(
      `SELECT text, byte_count, sha256 FROM usage_output_chunks
       WHERE event_id = ? AND stream = 'stdout' ORDER BY chunk_index`,
    ).bind(event.event_id).all<{ text: string; byte_count: number; sha256: string }>();
    expect(persisted.results.map((row) => row.text).join("")).toBe(texts.join(""));
    expect(persisted.results.map((row) => row.byte_count)).toEqual(chunks.map((chunk) => chunk.byte_count));
    expect(persisted.results.map((row) => row.sha256)).toEqual(chunks.map((chunk) => chunk.sha256));
  });

  it("rejects a chunk index outside its persisted event's declared total", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 2 };
    const chunk = {
      ...canonical.output_chunks[0],
      event_id: event.event_id,
      chunk_index: 2,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(event);

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000111",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { output_chunks: [] },
      rejected: [{
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "invalid",
        message: "输出分块索引超出事件声明总数。",
      }],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("fails closed when event-only or chunk-only uploads reference another user's parent", async () => {
    await seedUser("trace-bearer", 7);
    const foreignRunId = "019d9c40-7b3c-7000-8000-000000000103";
    const foreignEventId = "019d9c40-7b3c-7000-8000-000000000104";
    await seedRunOwnedBy(8, foreignRunId);
    await seedEvent(foreignEventId, foreignRunId, 1, 1, 0);
    const canonical = copySuccess();
    const event = { ...canonical.events[0], run_id: foreignRunId };
    const chunk = { ...canonical.output_chunks[0], event_id: foreignEventId };

    const eventResponse = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000105",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    const chunkResponse = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000106",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect([eventResponse.status, chunkResponse.status]).toEqual([409, 409]);
    expect(await eventResponse.json()).toMatchObject({ ok: false, error: { code: "TRACE_OWNERSHIP_CONFLICT" } });
    expect(await chunkResponse.json()).toMatchObject({ ok: false, error: { code: "TRACE_OWNERSHIP_CONFLICT" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("returns item-level missing_parent for unknown event and chunk parents", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const missingRunId = "019d9c40-7b3c-7000-8000-000000000112";
    const missingEventId = "019d9c40-7b3c-7000-8000-000000000113";
    const event = { ...canonical.events[0], run_id: missingRunId };
    const chunk = {
      ...canonical.output_chunks[0],
      event_id: missingEventId,
      chunk_index: Number.MAX_SAFE_INTEGER,
    };

    const eventResponse = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000114",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    const chunkResponse = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000115",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(await eventResponse.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: event.event_id, code: "missing_parent" }],
    });
    expect(await chunkResponse.json()).toMatchObject({
      accepted: { output_chunks: [] },
      rejected: [{ entity: "output_chunk", id: chunk.chunk_id, code: "missing_parent" }],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("fails closed when a persisted parent changes owner after pre-read", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    const event = canonical.events[0];
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000107",
      runs: [],
      events: [event],
      output_chunks: [],
    };
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt !== 1) return;
      await env.DB.prepare("DELETE FROM usage_operation_runs WHERE run_id = ?").bind(openRun.run_id).run();
      await seedRunOwnedBy(8, openRun.run_id);
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.45"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_OWNERSHIP_CONFLICT" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
  });

  it("rejects an event-only append when its persisted run completes after pre-read", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const persistedEvent = {
      ...canonical.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000000108",
    };
    const appendedEvent = { ...canonical.events[2], sequence: 2 };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(persistedEvent);
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000109",
      runs: [],
      events: [appendedEvent],
      output_chunks: [],
    };
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt !== 1) return;
      await env.DB.prepare(
        `UPDATE usage_operation_runs
         SET outcome = 'success', ended_at_ms = started_at_ms, duration_ms = 0,
             final_sequence = 1, trace_complete = 1
         WHERE run_id = ?`,
      ).bind(openRun.run_id).run();
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.45"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: appendedEvent.event_id, code: "sequence_conflict" }],
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE event_id = ?",
      appendedEvent.event_id,
    )).toBe(0);
  });

  it("rolls back a cross-user global ID conflict", async () => {
    await seedUser("trace-bearer", 7);
    await seedRunOwnedBy(8, successFixture.runs[0].run_id);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_OWNERSHIP_CONFLICT" },
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("rejects a sequence collision and every chunk whose event parent was rejected", async () => {
    await seedUser("trace-bearer", 7);
    const payload = copySuccess();
    payload.runs[0].outcome = "running";
    payload.runs[0].trace_complete = false;
    await seedRunFromPayload(7, payload.runs[0], "203.0.113.45");
    await seedEvent(
      "019d9c40-7b3c-7000-8000-000000000099",
      successFixture.runs[0].run_id,
      2,
    );

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted).toEqual({
      runs: [successFixture.runs[0].run_id],
      events: [successFixture.events[0].event_id, successFixture.events[2].event_id],
      output_chunks: [],
    });
    expect(body.rejected).toEqual([
      {
        entity: "event",
        id: successFixture.events[1].event_id,
        code: "sequence_conflict",
        message: "同一运行序号已由其他事件占用。",
      },
      ...successFixture.output_chunks.map((chunk) => ({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的事件父项。",
      })),
    ]);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("returns 422 and writes nothing when a complete trace has a sequence gap", async () => {
    await seedUser("trace-bearer", 7);
    const payload = copySuccess();
    payload.events.splice(1, 1);
    payload.output_chunks = [];

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(422);
    expect(await response.json()).toMatchObject({
      ok: false,
      error: {
        code: "TRACE_INCOMPLETE",
        details: [{
          entity: "run",
          id: successFixture.runs[0].run_id,
          code: "incomplete_trace",
        }],
      },
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("returns 422 and writes nothing when a declared output stream starts at index one", async () => {
    await seedUser("trace-bearer", 7);
    const payload = copySuccess();
    payload.output_chunks[0].chunk_index = 1;

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(422);
    expect(await response.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_INCOMPLETE" },
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("rejects a sealed run before its same-request descendants", async () => {
    await seedUser("trace-bearer", 7);
    const openRun = {
      ...copySuccess().runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEvent(
      successFixture.events[1].event_id,
      successFixture.runs[0].run_id,
      2,
      successFixture.events[1].stdout_chunks,
      successFixture.events[1].stderr_chunks,
    );
    for (const chunk of successFixture.output_chunks) {
      await seedChunk(chunk.chunk_id, chunk.event_id, chunk.stream, chunk.chunk_index);
    }
    await env.DB.prepare(
      "UPDATE usage_operation_runs SET retention_detail_cleared = 1 WHERE run_id = ?",
    ).bind(openRun.run_id).run();
    const payload = copySuccess();
    payload.events.splice(1, 1);
    payload.output_chunks = [];

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      accepted: { runs: [], events: [], output_chunks: [] },
      rejected: [
        {
          entity: "run",
          id: successFixture.runs[0].run_id,
          code: "invalid",
          message: expect.stringMatching(/retention_expired.*detail sealed/i),
        },
        {
          entity: "event",
          id: successFixture.events[0].event_id,
          code: "missing_parent",
        },
        {
          entity: "event",
          id: successFixture.events[2].event_id,
          code: "missing_parent",
        },
      ],
    });
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?", successFixture.runs[0].run_id)).toBe(0);
    expect(await scalar("SELECT retention_detail_cleared AS value FROM usage_operation_runs WHERE run_id = ?", successFixture.runs[0].run_id)).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", successFixture.runs[0].run_id)).toBe(0);
  });

  it("finalizes twenty persisted open runs in one bounded request", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const terminalRuns: any[] = [];
    for (let index = 0; index < 20; index += 1) {
      const runId = `019d9c40-7b3c-7000-8000-${(4_000 + index).toString().padStart(12, "0")}`;
      const eventId = `019d9c40-7b3c-7000-8000-${(4_100 + index).toString().padStart(12, "0")}`;
      const terminalRun = { ...canonical.runs[0], run_id: runId, final_sequence: 1 };
      const openRun = {
        ...terminalRun,
        outcome: "running",
        ended_at_ms: null,
        duration_ms: null,
        final_sequence: null,
        trace_complete: false,
      };
      await seedRunFromPayload(7, openRun, "203.0.113.45");
      await seedEventFromPayload({ ...canonical.events[0], event_id: eventId, run_id: runId });
      terminalRuns.push(terminalRun);
    }
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004200",
      runs: terminalRuns,
      events: [],
      output_chunks: [],
    };

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect((await response.json() as any).accepted.runs).toEqual(terminalRuns.map((run) => run.run_id));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs WHERE trace_complete = 1")).toBe(20);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(20);
  });

  it("finalizes a high-escape run without double-encoding its persisted CAS snapshot", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    let remaining = 523_000;
    const sourcePaths: string[] = [];
    while (remaining > 0) {
      const length = Math.min(16_384, remaining);
      sourcePaths.push("\\".repeat(length));
      remaining -= length;
    }
    const terminalRun = {
      ...canonical.runs[0],
      source_paths: sourcePaths,
      final_sequence: 1,
    };
    const openRun = {
      ...terminalRun,
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const openPayload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004308",
      runs: [openRun],
      events: [],
      output_chunks: [],
    };
    const finalPayload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004309",
      runs: [terminalRun],
      events: [],
      output_chunks: [],
    };
    expect(new TextEncoder().encode(JSON.stringify(openPayload)).byteLength).toBeLessThan(1_048_576);
    expect(new TextEncoder().encode(JSON.stringify(finalPayload)).byteLength).toBeLessThan(1_048_576);
    const storedPathsJson = JSON.stringify(sourcePaths);
    expect(new TextEncoder().encode(storedPathsJson).byteLength).toBeLessThan(2_000_000);
    expect(new TextEncoder().encode(JSON.stringify({ source_paths_json: storedPathsJson })).byteLength)
      .toBeGreaterThan(2_090_000);
    expect(new TextEncoder().encode(JSON.stringify({ source_paths_json: sourcePaths })).byteLength)
      .toBeLessThan(1_048_576);
    const encodedSnapshot = encodePersistedRunSnapshotsForGuard([{
      source_paths_json: storedPathsJson,
      source_urls_json: "[]",
      credential_redactions_json: "[]",
    } as any]);
    expect(new TextEncoder().encode(encodedSnapshot).byteLength).toBeLessThan(1_048_576);
    expect(JSON.parse(encodedSnapshot)[0].source_paths_json).toEqual(sourcePaths);

    const openResponse = await postTrace(openPayload, "trace-bearer", "203.0.113.45");
    expect(openResponse.status).toBe(200);
    await seedEventFromPayload({ ...canonical.events[0], run_id: terminalRun.run_id });
    const finalResponse = await postTrace(finalPayload, "trace-bearer", "203.0.113.45");

    expect(finalResponse.status).toBe(200);
    expect((await finalResponse.json() as any).accepted.runs).toEqual([terminalRun.run_id]);
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs")).toBe(1);
  });

  it("acknowledges only the durable winner of concurrent same-sequence uploads", async () => {
    await seedUser("trace-bearer", 7);
    const payloads = Array.from({ length: 8 }, (_, index) => concurrentPayload(index));

    const responses = await Promise.all(
      payloads.map((payload) => postTrace(payload, "trace-bearer", "203.0.113.45")),
    );
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<any>));

    expect(responses.every((response) => response.status === 200)).toBe(true);
    expect(bodies.flatMap((body) => body.accepted.events)).toHaveLength(1);
    expect(bodies.flatMap((body) => body.rejected).filter((item: any) =>
      item.entity === "event" && item.code === "sequence_conflict"
    )).toHaveLength(7);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
  });

  it("returns 409 to the losing user when two users concurrently claim the same global IDs", async () => {
    await seedUser("alice-trace-bearer", 7);
    await seedUser("bob-trace-bearer", 8);

    const responses = await Promise.all([
      postTrace(successFixture, "alice-trace-bearer", "203.0.113.47"),
      postTrace(successFixture, "bob-trace-bearer", "203.0.113.48"),
    ]);
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<any>));

    expect(responses.map((response) => response.status).sort()).toEqual([200, 409]);
    expect(bodies.filter((body) => body.ok === true)).toEqual([successAckFixture]);
    expect(bodies.filter((body) => body.ok === false)).toHaveLength(1);
    expect(bodies.find((body) => body.ok === false)).toMatchObject({
      error: { code: "TRACE_OWNERSHIP_CONFLICT" },
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(1);
    expect(await scalar(
      `SELECT COUNT(*) AS value
       FROM usage_logs AS legacy
       JOIN usage_operation_runs AS run ON run.run_id = legacy.event_key
       WHERE legacy.api_user_id = run.api_user_id`,
    )).toBe(1);
  });

  it("never commits both completion and a concurrently appended event", async () => {
    await seedUser("trace-bearer", 7);
    const appended = appendEventPayload();

    const [completionResponse, appendResponse] = await Promise.all([
      postTrace(successFixture, "trace-bearer", "203.0.113.49"),
      postTrace(appended, "trace-bearer", "203.0.113.50"),
    ]);
    const completionBody = await completionResponse.json() as any;
    const appendBody = await appendResponse.json() as any;
    const complete = await scalar(
      "SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?",
      successFixture.runs[0].run_id,
    );
    const appendedCount = await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE event_id = ?",
      appended.events[0].event_id,
    );

    expect(appendResponse.status).toBe(200);
    expect([200, 422]).toContain(completionResponse.status);
    expect(complete === 1 && appendedCount === 1).toBe(false);
    if (complete === 1) {
      expect(completionResponse.status).toBe(200);
      expect(appendedCount).toBe(0);
      expect(appendBody.rejected).toContainEqual(expect.objectContaining({
        entity: "event",
        id: appended.events[0].event_id,
        code: "missing_parent",
      }));
    } else {
      expect(completionResponse.status).toBe(422);
      expect(completionBody).toMatchObject({ ok: false, error: { code: "TRACE_INCOMPLETE" } });
      expect(appendedCount).toBe(1);
      expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    }
  });

  it("acks only one of two terminal semantics prepared from the same open run", async () => {
    await seedUser("trace-bearer", 7);
    const openRun = {
      ...copySuccess().runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    for (const event of successFixture.events) await seedEventFromPayload(event);
    for (const chunk of successFixture.output_chunks) await seedChunkFromPayload(chunk);
    const successPayload = copySuccess();
    const failedPayload = copySuccess();
    failedPayload.upload_id = "019d9c40-7b3c-7000-8000-000000000095";
    failedPayload.runs[0].outcome = "failed";
    failedPayload.runs[0].error_class = "CompetingFailure";
    failedPayload.runs[0].error_code = "COMPETING_FAILURE";
    failedPayload.runs[0].error_message = "The competing terminal semantic won";
    const databases = pairedBatchBarrierDatabases();
    const payloads = [successPayload, failedPayload];

    const responses = await Promise.all(payloads.map((payload, index) => ingestTraceUploadV2(
      { DB: databases[index] },
      traceRequest(payload, "trace-bearer", "203.0.113.45"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    )));
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<any>));
    const ackedIndexes = bodies
      .map((body, index) => body.accepted.runs.includes(successFixture.runs[0].run_id) ? index : -1)
      .filter((index) => index >= 0);

    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(ackedIndexes).toHaveLength(1);
    expect(bodies).toEqual(expect.arrayContaining([expect.objectContaining({
      rejected: expect.arrayContaining([{
        entity: "run",
        id: successFixture.runs[0].run_id,
        code: "invalid",
        message: "运行标识与已持久化内容不一致。",
      }]),
    })]));
    expect(await text(
      "SELECT outcome AS value FROM usage_operation_runs WHERE run_id = ?",
      successFixture.runs[0].run_id,
    )).toBe(payloads[ackedIndexes[0]].runs[0].outcome);
    expect(await text(
      "SELECT status AS value FROM usage_logs WHERE event_key = ?",
      successFixture.runs[0].run_id,
    )).toBe(payloads[ackedIndexes[0]].runs[0].outcome);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
  });

  it("rejects a direct event append after a run is complete", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);

    await expect(seedEvent(
      "019d9c40-7b3c-7000-8000-000000000098",
      successFixture.runs[0].run_id,
      4,
    )).rejects.toThrow(/complete/i);

    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
  });

  it("returns missing_parent for a fresh event whose completed run item is rejected", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const payload = copySuccess();
    payload.runs[0].title = "Conflicting completed run item";
    payload.events = [{
      ...payload.events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004315",
      sequence: 4,
    }];
    payload.output_chunks = [];

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(body.rejected).toEqual(expect.arrayContaining([
      expect.objectContaining({ entity: "run", id: payload.runs[0].run_id, code: "invalid" }),
      {
        entity: "event",
        id: payload.events[0].event_id,
        code: "missing_parent",
        message: "事件缺少已接受的运行父项。",
      },
    ]));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
  });

  it("returns sequence_conflict for a child-only event referencing a persisted completed run", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const event = {
      ...copySuccess().events[0],
      event_id: "019d9c40-7b3c-7000-8000-000000004316",
      sequence: 4,
    };

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004317",
      runs: [],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");

    expect(await response.json()).toMatchObject({
      accepted: { events: [] },
      rejected: [{ entity: "event", id: event.event_id, code: "sequence_conflict" }],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
  });

  it("rejects a direct output append after a run is complete", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);

    await expect(seedChunk(
      "019d9c40-7b3c-7000-8000-000000000098",
      successFixture.events[1].event_id,
      "stdout",
      1,
    )).rejects.toThrow(/complete/i);

    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
  });

  it("prioritizes completed-run rejection over an out-of-range fresh chunk", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const chunk = {
      ...copySuccess().output_chunks[0],
      chunk_id: "019d9c40-7b3c-7000-8000-000000004310",
      chunk_index: 99,
    };

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004311",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, "trace-bearer", "203.0.113.45");

    expect(await response.json()).toMatchObject({
      accepted: { output_chunks: [] },
      rejected: [{ entity: "output_chunk", id: chunk.chunk_id, code: "sequence_conflict" }],
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
  });

  it("prioritizes completed-run rejection over cross-boundary credentials", async () => {
    const bearer = "completed-boundary-bearer-123456";
    await seedUser(bearer, 7);
    const canonical = copySuccess();
    canonical.runs[0].final_sequence = 1;
    canonical.events = [{ ...canonical.events[0], stdout_chunks: 2 }];
    canonical.output_chunks = [
      { ...canonical.output_chunks[0], event_id: canonical.events[0].event_id, text: "safe-0\n", chunk_index: 0 },
      { ...canonical.output_chunks[1], event_id: canonical.events[0].event_id, stream: "stdout", text: "safe-1\n", chunk_index: 1 },
    ];
    for (const chunk of canonical.output_chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }
    expect((await postTrace(canonical, bearer, "203.0.113.45")).status).toBe(200);
    const chunks = [
      { ...canonical.output_chunks[0], chunk_id: "019d9c40-7b3c-7000-8000-000000004312", text: "Authorization: Bearer completed-boundary-" },
      { ...canonical.output_chunks[1], chunk_id: "019d9c40-7b3c-7000-8000-000000004313", text: "bearer-123456\n" },
    ];
    for (const chunk of chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004314",
      runs: [],
      events: [],
      output_chunks: chunks,
    }, bearer, "203.0.113.45");
    const body = await response.json() as any;

    expect(body.accepted.output_chunks).toEqual([]);
    expect(body.rejected).toEqual(expect.arrayContaining(chunks.map((chunk) => expect.objectContaining({
      entity: "output_chunk",
      id: chunk.chunk_id,
      code: "sequence_conflict",
    }))));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
  });

  it("returns a durable run ack plus item rejections after every retry loses a natural-key race", async () => {
    await seedUser("trace-bearer", 7);
    const payload = retryExhaustionPayload();
    await seedRunFromPayload(7, payload.runs[0], "203.0.113.51");
    const db = collisionPerBatchDatabase(async (attempt) => {
      await seedEvent(
        `019d9c40-7b3c-7000-8000-00000000009${attempt}`,
        payload.runs[0].run_id,
        attempt,
      );
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.51"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      ok: true,
      accepted: { runs: [payload.runs[0].run_id], events: [], output_chunks: [] },
      rejected: payload.events.map((event: any) => ({
        entity: "event",
        id: event.event_id,
        code: "sequence_conflict",
        message: "同一运行序号已由其他事件占用。",
      })),
    });
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_operation_events
       WHERE event_id IN (?, ?, ?)`,
      ...payload.events.map((event: any) => event.event_id),
    )).toBe(0);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE run_id = ?",
      payload.runs[0].run_id,
    )).toBe(3);
    expect(await text("SELECT title AS value FROM usage_operation_runs WHERE run_id = ?", payload.runs[0].run_id)).toBe(payload.runs[0].title);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_trace_ingest_guards")).toBe(0);
  });

  it("returns 409 after the final retry reveals a cross-user ID owner", async () => {
    await seedUser("trace-bearer", 7);
    const foreignRunId = "019d9c40-7b3c-7000-8000-000000000080";
    await seedRunOwnedBy(8, foreignRunId);
    const payload = retryExhaustionPayload();
    await seedRunFromPayload(7, payload.runs[0], "203.0.113.52");
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt < 3) {
        await seedEvent(
          `019d9c40-7b3c-7000-8000-00000000009${attempt}`,
          payload.runs[0].run_id,
          attempt,
        );
        return;
      }
      await seedEvent(payload.events[2].event_id, foreignRunId, 1);
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.52"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({
      ok: false,
      error: { code: "TRACE_OWNERSHIP_CONFLICT" },
    });
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE run_id = ?",
      payload.runs[0].run_id,
    )).toBe(2);
    expect(await text("SELECT title AS value FROM usage_operation_runs WHERE run_id = ?", payload.runs[0].run_id)).toBe(payload.runs[0].title);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_trace_ingest_guards")).toBe(0);
  });

  it("acks exact run event and chunk IDs made durable across all exhausted retries", async () => {
    await seedUser("trace-bearer", 7);
    const payload = exactRetryExhaustionPayload();
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt === 1) {
        await seedRunFromPayload(7, payload.runs[0], "203.0.113.53");
        await seedEventFromPayload(payload.events[0]);
        return;
      }
      if (attempt === 2) {
        await seedEventFromPayload(payload.events[1]);
        return;
      }
      await seedEventFromPayload(payload.events[2]);
      for (const chunk of payload.output_chunks) {
        await seedChunkFromPayload(chunk);
      }
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.53"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await text("SELECT title AS value FROM usage_operation_runs")).toBe(payload.runs[0].title);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_trace_ingest_guards")).toBe(0);
  });

  it("returns durable duplicate IDs together with a final natural-key rejection", async () => {
    await seedUser("trace-bearer", 7);
    const payload = retryExhaustionPayload();
    const db = collisionPerBatchDatabase(async (attempt) => {
      if (attempt === 1) {
        await seedRunFromPayload(7, payload.runs[0], "203.0.113.54");
        await seedEventFromPayload(payload.events[0]);
        return;
      }
      if (attempt === 2) {
        await seedEvent(
          "019d9c40-7b3c-7000-8000-000000000097",
          payload.runs[0].run_id,
          2,
        );
        return;
      }
      await seedEventFromPayload(payload.events[2]);
    });

    const response = await ingestTraceUploadV2(
      { DB: db },
      traceRequest(payload, "trace-bearer", "203.0.113.54"),
      { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" },
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      ok: true,
      accepted: {
        runs: [payload.runs[0].run_id],
        events: [payload.events[0].event_id, payload.events[2].event_id],
        output_chunks: [],
      },
      rejected: [{
        entity: "event",
        id: payload.events[1].event_id,
        code: "sequence_conflict",
        message: "同一运行序号已由其他事件占用。",
      }],
    });
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_operation_events
       WHERE event_id IN (?, ?)`,
      payload.events[0].event_id,
      payload.events[2].event_id,
    )).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_trace_ingest_guards")).toBe(0);
  });

  it("returns 500 when exhausted retries leave completion and V1 projection pending", async () => {
    await seedUser("trace-bearer", 7);
    const incomplete = copySuccess();
    incomplete.runs[0].trace_complete = false;
    expect((await postTrace(incomplete, "trace-bearer", "203.0.113.55")).status).toBe(200);
    expect(await scalar(
      "SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?",
      successFixture.runs[0].run_id,
    )).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    await env.DB.prepare(
      `CREATE TRIGGER force_trace_projection_failure
       BEFORE INSERT ON usage_logs
       WHEN NEW.event_key = '${successFixture.runs[0].run_id}'
       BEGIN SELECT RAISE(ABORT, 'forced trace projection failure'); END`,
    ).run();

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.55");

    expect(response.status).toBe(500);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_INTERNAL" } });
    expect(await scalar(
      "SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?",
      successFixture.runs[0].run_id,
    )).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_trace_ingest_guards")).toBe(0);
  });

  it("acks same-user duplicate IDs across source IP changes without duplicating persisted rows", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.46");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(1);
    expect(await text("SELECT source_ip AS value FROM usage_operation_runs")).toBe("203.0.113.45");
  });

  it("acks an exact duplicate after the user display name changes", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    await env.DB.prepare("UPDATE api_users SET name = 'Renamed User' WHERE id = 7").run();

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(await text("SELECT api_user_name AS value FROM usage_operation_runs")).toBe("User 7");
    expect(await text("SELECT api_user_name AS value FROM usage_logs")).toBe("User 7");
  });

  it("finalizes an open run after name and IP changes while preserving its initial server fields", async () => {
    await seedUser("trace-bearer", 7);
    const openRun = {
      ...copySuccess().runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    for (const event of successFixture.events) await seedEventFromPayload(event);
    for (const chunk of successFixture.output_chunks) await seedChunkFromPayload(chunk);
    await env.DB.prepare("UPDATE api_users SET name = 'Renamed User' WHERE id = 7").run();

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.46");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs")).toBe(1);
    expect(await text("SELECT api_user_name AS value FROM usage_operation_runs")).toBe("User 7");
    expect(await text("SELECT source_ip AS value FROM usage_operation_runs")).toBe("203.0.113.45");
    expect(await text("SELECT api_user_name AS value FROM usage_logs")).toBe("User 7");
  });

  it("rejects a same-user completed run ID whose persisted summary differs", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const payload = copySuccess();
    payload.runs[0].title = "Conflicting completed summary";

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.runs).toEqual([]);
    expect(body.rejected).toContainEqual({
      entity: "run",
      id: successFixture.runs[0].run_id,
      code: "invalid",
      message: "运行标识与已持久化内容不一致。",
    });
    expect(await text(
      "SELECT title AS value FROM usage_operation_runs WHERE run_id = ?",
      successFixture.runs[0].run_id,
    )).toBe(successFixture.runs[0].title);
  });

  it("rejects fresh children when their run item is rejected", async () => {
    await seedUser("trace-bearer", 7);
    const openRun = {
      ...copySuccess().runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    const payload = copySuccess();
    payload.runs[0] = { ...openRun, title: "Rejected run mutation" };

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted).toEqual({ runs: [], events: [], output_chunks: [] });
    expect(body.rejected).toEqual(expect.arrayContaining([
      {
        entity: "run",
        id: payload.runs[0].run_id,
        code: "invalid",
        message: "运行标识与已持久化内容不一致。",
      },
      ...payload.events.map((event: any) => ({
        entity: "event",
        id: event.event_id,
        code: "missing_parent",
        message: "事件缺少已接受的运行父项。",
      })),
      ...payload.output_chunks.map((chunk: any) => ({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的事件父项。",
      })),
    ]));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("rejects a fresh chunk whose exact persisted event belongs to a rejected run item", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const persistedEvent = {
      ...canonical.events[0],
      stdout_chunks: 1,
    };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(persistedEvent);
    const freshChunk = {
      ...canonical.output_chunks[0],
      event_id: persistedEvent.event_id,
      chunk_index: Number.MAX_SAFE_INTEGER,
    };
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000096",
      runs: [{ ...openRun, title: "Rejected immutable run mutation" }],
      events: [persistedEvent],
      output_chunks: [freshChunk],
    };

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted).toEqual({
      runs: [],
      events: [persistedEvent.event_id],
      output_chunks: [],
    });
    expect(body.rejected).toContainEqual({
      entity: "output_chunk",
      id: freshChunk.chunk_id,
      code: "missing_parent",
      message: "输出分块缺少已接受的运行祖先。",
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("prioritizes missing_parent for credential-bearing descendants of a rejected run item", async () => {
    const bearer = "parent-boundary-bearer-123456";
    await seedUser(bearer, 7);
    const canonical = copySuccess();
    const openRun = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 2 };
    await seedRunFromPayload(7, openRun, "203.0.113.45");
    await seedEventFromPayload(event);
    const chunks = [
      { ...canonical.output_chunks[0], event_id: event.event_id, text: "Authorization: Bearer parent-boundary-" },
      { ...canonical.output_chunks[1], event_id: event.event_id, stream: "stdout", chunk_index: 1, text: "bearer-123456\n" },
    ];
    for (const chunk of chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004304",
      runs: [{ ...openRun, title: "Rejected mutation" }],
      events: [event],
      output_chunks: chunks,
    }, bearer, "203.0.113.45");
    const body = await response.json() as any;

    expect(body.accepted.output_chunks).toEqual([]);
    expect(body.rejected).toEqual(expect.arrayContaining(chunks.map((chunk) => ({
      entity: "output_chunk",
      id: chunk.chunk_id,
      code: "missing_parent",
      message: "输出分块缺少已接受的运行祖先。",
    }))));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("rejects a same-user event ID that points at a different run", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const payload = copySuccess();
    const alternateRunId = "019d9c40-7b3c-7000-8000-000000000090";
    payload.runs.push({
      ...payload.runs[0],
      run_id: alternateRunId,
      title: "Alternate running trace",
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    });
    payload.events[0].run_id = alternateRunId;

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.events).not.toContain(successFixture.events[0].event_id);
    expect(body.rejected).toContainEqual({
      entity: "event",
      id: successFixture.events[0].event_id,
      code: "invalid",
      message: "事件标识与已持久化内容不一致。",
    });
    expect(await text(
      "SELECT run_id AS value FROM usage_operation_events WHERE event_id = ?",
      successFixture.events[0].event_id,
    )).toBe(successFixture.runs[0].run_id);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_operation_events WHERE run_id = ?",
      alternateRunId,
    )).toBe(0);
  });

  it("rejects same-user event IDs whose natural keys or metadata differ", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const payload = copySuccess();
    [payload.events[0].sequence, payload.events[2].sequence] = [
      payload.events[2].sequence,
      payload.events[0].sequence,
    ];
    payload.events[1].verification = "Conflicting persisted verification";

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.events).toEqual([]);
    expect(body.rejected).toEqual(expect.arrayContaining(payload.events.map((event: any) => ({
      entity: "event",
      id: event.event_id,
      code: "invalid",
      message: "事件标识与已持久化内容不一致。",
    }))));
    expect(await scalar(
      "SELECT sequence AS value FROM usage_operation_events WHERE event_id = ?",
      successFixture.events[0].event_id,
    )).toBe(successFixture.events[0].sequence);
    expect(await text(
      "SELECT verification AS value FROM usage_operation_events WHERE event_id = ?",
      successFixture.events[1].event_id,
    )).toBe(successFixture.events[1].verification);
  });

  it("rejects a same-user chunk ID that points at a different event", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const canonical = copySuccess();
    const alternateRunId = "019d9c40-7b3c-7000-8000-000000000092";
    const alternateEventId = "019d9c40-7b3c-7000-8000-000000000093";
    const payload = {
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000000094",
      runs: [{
        ...canonical.runs[0],
        run_id: alternateRunId,
        title: "Alternate output parent",
        outcome: "running",
        ended_at_ms: null,
        duration_ms: null,
        final_sequence: null,
        trace_complete: false,
      }],
      events: [{
        ...canonical.events[0],
        event_id: alternateEventId,
        run_id: alternateRunId,
        stdout_chunks: 1,
      }],
      output_chunks: [{
        ...canonical.output_chunks[0],
        event_id: alternateEventId,
      }],
    };

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.output_chunks).toEqual([]);
    expect(body.rejected).toContainEqual({
      entity: "output_chunk",
      id: successFixture.output_chunks[0].chunk_id,
      code: "invalid",
      message: "输出分块标识与已持久化内容不一致。",
    });
    expect(await text(
      "SELECT event_id AS value FROM usage_output_chunks WHERE chunk_id = ?",
      successFixture.output_chunks[0].chunk_id,
    )).toBe(successFixture.events[1].event_id);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_output_chunks WHERE event_id = ?",
      alternateEventId,
    )).toBe(0);
  });

  it("rejects same-user chunk IDs whose natural keys or content hashes differ", async () => {
    await seedUser("trace-bearer", 7);
    expect((await postTrace(successFixture, "trace-bearer", "203.0.113.45")).status).toBe(200);
    const payload = copySuccess();
    [payload.output_chunks[0].stream, payload.output_chunks[1].stream] = [
      payload.output_chunks[1].stream,
      payload.output_chunks[0].stream,
    ];
    payload.output_chunks[0].text = "Conflicting output\n";
    const bytes = new TextEncoder().encode(payload.output_chunks[0].text);
    payload.output_chunks[0].byte_count = bytes.byteLength;
    payload.output_chunks[0].sha256 = await sha256Hex(bytes);

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.output_chunks).toEqual([]);
    expect(body.rejected).toEqual(expect.arrayContaining(payload.output_chunks.map((chunk: any) => ({
      entity: "output_chunk",
      id: chunk.chunk_id,
      code: "invalid",
      message: "输出分块标识与已持久化内容不一致。",
    }))));
    expect(await text(
      "SELECT text AS value FROM usage_output_chunks WHERE chunk_id = ?",
      successFixture.output_chunks[0].chunk_id,
    )).toBe(successFixture.output_chunks[0].text);
  });

  it("rejects a conflicting output stream index without acknowledging its uncommitted ID", async () => {
    await seedUser("trace-bearer", 7);
    const payload = copySuccess();
    payload.runs[0].outcome = "running";
    payload.runs[0].trace_complete = false;
    await seedRunFromPayload(7, payload.runs[0], "203.0.113.45");
    await seedEventFromPayload(payload.events[1]);
    await seedChunk(
      "019d9c40-7b3c-7000-8000-000000000099",
      successFixture.events[1].event_id,
      "stdout",
      0,
    );

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.output_chunks).toEqual([successFixture.output_chunks[1].chunk_id]);
    expect(body.rejected).toContainEqual({
      entity: "output_chunk",
      id: successFixture.output_chunks[0].chunk_id,
      code: "sequence_conflict",
      message: "同一输出流序号已由其他分块占用。",
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
  });

  it("rolls back when an event ID is owned by another user", async () => {
    await seedUser("trace-bearer", 7);
    const foreignRunId = "019d9c40-7b3c-7000-8000-000000000090";
    await seedRunOwnedBy(8, foreignRunId);
    await seedEvent(successFixture.events[0].event_id, foreignRunId, 1);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_OWNERSHIP_CONFLICT" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("rolls back when an output chunk ID is owned by another user", async () => {
    await seedUser("trace-bearer", 7);
    const foreignRunId = "019d9c40-7b3c-7000-8000-000000000090";
    const foreignEventId = "019d9c40-7b3c-7000-8000-000000000091";
    await seedRunOwnedBy(8, foreignRunId);
    await seedEvent(foreignEventId, foreignRunId, 1, 1, 0);
    await seedChunk(successFixture.output_chunks[0].chunk_id, foreignEventId, "stdout", 0);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_OWNERSHIP_CONFLICT" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("requires a bearer user before parsing or writing a trace", async () => {
    const response = await postTrace(successFixture, null, "203.0.113.45");

    expect(response.status).toBe(401);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expectTraceApiError(await response.json(), "TRACE_UNAUTHORIZED", "请先登录。");
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("rejects a disabled bearer user", async () => {
    await seedUser("disabled-bearer", 7, 0, 0);

    const response = await postTrace(successFixture, "disabled-bearer", "203.0.113.45");

    expect(response.status).toBe(401);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expectTraceApiError(await response.json(), "TRACE_UNAUTHORIZED", "API token 无效或已停用。");
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("rejects a banned bearer user", async () => {
    await seedUser("banned-bearer", 7, 1, 1);

    const response = await postTrace(successFixture, "banned-bearer", "203.0.113.45");

    expect(response.status).toBe(403);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expectTraceApiError(await response.json(), "TRACE_FORBIDDEN", "账号已被封禁。");
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("applies the app-version gate before ingestion", async () => {
    await seedUser("trace-bearer", 7);
    await env.DB.prepare(
      `INSERT INTO app_versions (version, min_version, download_url, enabled)
       VALUES ('2.0.0', '2.0.0', 'https://download.example/nwflash', 1)`,
    ).run();

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(426);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("rejects a client-supplied IP field instead of persisting it", async () => {
    await seedUser("trace-bearer", 7);
    const payload = { ...copySuccess(), source_ip: "198.51.100.99" };

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_INVALID" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("returns 400 before D1 when trace_complete still has a running outcome", async () => {
    await seedUser("trace-bearer", 7);
    const payload = copySuccess();
    payload.runs[0].outcome = "running";

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ ok: false, error: { code: "TRACE_INVALID" } });
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("removes the bearer credential from every persisted and returned trace field", async () => {
    const bearer = "trace-secret-token-123";
    await seedUser(bearer, 7);
    const payload = copySuccess();
    payload.runs[0].title = `Flash with ${bearer}`;
    payload.events[0].verification = `Authorization: Bearer ${bearer}`;
    payload.output_chunks[0].text = `token=${bearer}\n`;
    const bytes = new TextEncoder().encode(payload.output_chunks[0].text);
    payload.output_chunks[0].byte_count = bytes.byteLength;
    payload.output_chunks[0].sha256 = await sha256Hex(bytes);

    const response = await postTrace(payload, bearer, "203.0.113.45");
    const responseText = await response.text();

    expect(response.status).toBe(200);
    expect(responseText).not.toContain(bearer);
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_operation_runs
       WHERE title LIKE ? OR source_paths_json LIKE ? OR source_urls_json LIKE ?`,
      `%${bearer}%`, `%${bearer}%`, `%${bearer}%`,
    )).toBe(0);
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_operation_events
       WHERE verification LIKE ? OR error_message LIKE ? OR command_line LIKE ?`,
      `%${bearer}%`, `%${bearer}%`, `%${bearer}%`,
    )).toBe(0);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_output_chunks WHERE text LIKE ?",
      `%${bearer}%`,
    )).toBe(0);
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_operation_runs
       WHERE credential_redactions_json LIKE '%exact%'`,
    )).toBe(1);
  });

  it("keeps one owner-bound projection for concurrent same-user terminal retries", async () => {
    await seedUser("trace-bearer", 7);
    const [firstDb, secondDb] = pairedBatchBarrierDatabases();
    const user = { id: 7, username: "user-7", name: "User 7", bearer_token: "trace-bearer" };

    const responses = await Promise.all([
      ingestTraceUploadV2(
        { DB: firstDb },
        traceRequest(successFixture, "trace-bearer", "203.0.113.61"),
        user,
      ),
      ingestTraceUploadV2(
        { DB: secondDb },
        traceRequest(successFixture, "trace-bearer", "203.0.113.62"),
        user,
      ),
    ]);
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<any>));

    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(bodies).toEqual([successAckFixture, successAckFixture]);
    expect(await scalar(
      `SELECT COUNT(*) AS value FROM usage_logs
       WHERE source_schema = 2 AND trace_run_id = ? AND api_user_id = 7`,
      successFixture.runs[0].run_id,
    )).toBe(1);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_logs WHERE source_schema = 1 AND event_key = ?",
      successFixture.runs[0].run_id,
    )).toBe(0);
  });

  it("does not persist plaintext appended to redaction markers", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const run = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const sentinels = ["basic-sentinel", "cookie-sentinel", "password-sentinel", "token-sentinel"];
    const event = {
      ...canonical.events[0],
      verification: [
        `Authorization: Basic [REDACTED]${sentinels[0]}`,
        `Cookie: a=[REDACTED]; b=${sentinels[1]}`,
        `password=[REDACTED]${sentinels[2]}`,
        `token=[CREDENTIAL_REMOVED:TOKEN]${sentinels[3]}`,
      ].join("\n"),
    };

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004318",
      runs: [run],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    const responseText = await response.text();
    const stored = await text(
      "SELECT verification AS value FROM usage_operation_events WHERE event_id = ?",
      event.event_id,
    );

    expect(response.status).toBe(200);
    for (const sentinel of sentinels) {
      expect(responseText).not.toContain(sentinel);
      expect(stored).not.toContain(sentinel);
    }
  });

  it("persists one server-prioritized capped event redaction array", async () => {
    await seedUser("trace-bearer", 7);
    const canonical = copySuccess();
    const run = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = {
      ...canonical.events[0],
      error_message: "password=server-count-sentinel",
      credential_redactions: Array.from({ length: 100 }, (_, index) => ({ kind: `client-${index}`, count: 1 })),
    };

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004319",
      runs: [run],
      events: [event],
      output_chunks: [],
    }, "trace-bearer", "203.0.113.45");
    const stored = JSON.parse((await text(
      "SELECT credential_redactions_json AS value FROM usage_operation_events WHERE event_id = ?",
      event.event_id,
    ))!);

    expect(response.status).toBe(200);
    expect(stored).toHaveLength(100);
    expect(stored[0]).toEqual({ kind: "password", count: 1 });
    expect(stored).not.toContainEqual({ kind: "client-99", count: 1 });
  });

  it("rejects a known bearer that crosses adjacent raw chunk boundaries", async () => {
    const bearer = "cross-boundary-bearer-123456";
    await seedUser(bearer, 7);
    const payload = copySuccess();
    payload.runs[0].outcome = "running";
    payload.runs[0].ended_at_ms = null;
    payload.runs[0].duration_ms = null;
    payload.runs[0].final_sequence = null;
    payload.runs[0].trace_complete = false;
    payload.events[1].stdout_chunks = 2;
    payload.events[1].stderr_chunks = 0;
    payload.output_chunks[0] = {
      ...payload.output_chunks[0],
      text: "Authorization: Bearer cross-boundary-",
      chunk_index: 0,
    };
    payload.output_chunks[1] = {
      ...payload.output_chunks[1],
      stream: "stdout",
      text: "bearer-123456\n",
      chunk_index: 1,
    };
    for (const chunk of payload.output_chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }

    const response = await postTrace(payload, bearer, "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(200);
    expect(body.accepted.output_chunks).toEqual([]);
    expect(body.rejected).toEqual(expect.arrayContaining(payload.output_chunks.map((chunk: any) => ({
      entity: "output_chunk",
      id: chunk.chunk_id,
      code: "credential_rejected",
      message: "凭据跨越原始输出分块边界，请客户端先对完整逻辑流脱敏后重试。",
    }))));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
  });

  it("persists an operational chunk exactly equal to a known bearer prefix", async () => {
    const bearer = "prefix-valid-bearer-123456";
    await seedUser(bearer, 7);
    const canonical = copySuccess();
    const run = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 1 };
    const chunk = { ...canonical.output_chunks[0], event_id: event.event_id, text: "prefix-valid-" };
    const bytes = new TextEncoder().encode(chunk.text);
    chunk.byte_count = bytes.byteLength;
    chunk.sha256 = await sha256Hex(bytes);

    const response = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004305",
      runs: [run],
      events: [event],
      output_chunks: [chunk],
    }, bearer, "203.0.113.45");

    expect((await response.json() as any).accepted.output_chunks).toEqual([chunk.chunk_id]);
    expect(await text("SELECT text AS value FROM usage_output_chunks WHERE chunk_id = ?", chunk.chunk_id)).toBe(chunk.text);
  });

  it("acks a durable canonical chunk when regrouped with a fresh cross-boundary chunk", async () => {
    const bearer = "regroup-boundary-bearer-123456";
    await seedUser(bearer, 7);
    const canonical = copySuccess();
    const run = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 2 };
    const chunks = [
      { ...canonical.output_chunks[0], event_id: event.event_id, text: "Authorization: Bearer regroup-boundary-" },
      { ...canonical.output_chunks[1], event_id: event.event_id, stream: "stdout", chunk_index: 1, text: "bearer-123456\n" },
    ];
    for (const chunk of chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }
    const first = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004306",
      runs: [run],
      events: [event],
      output_chunks: [chunks[0]],
    }, bearer, "203.0.113.45");
    expect((await first.json() as any).accepted.output_chunks).toEqual([chunks[0].chunk_id]);

    const regrouped = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004307",
      runs: [],
      events: [],
      output_chunks: chunks,
    }, bearer, "203.0.113.45");
    const body = await regrouped.json() as any;

    expect(body.accepted.output_chunks).toEqual([chunks[0].chunk_id]);
    expect(body.rejected).toContainEqual(expect.objectContaining({
      entity: "output_chunk",
      id: chunks[1].chunk_id,
      code: "credential_rejected",
    }));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
    expect(await scalar(
      "SELECT COUNT(*) AS value FROM usage_output_chunks WHERE chunk_id = ?",
      chunks[1].chunk_id,
    )).toBe(0);
  });

  it("returns credential rejection details without projecting an incomplete terminal upload", async () => {
    const bearer = "terminal-boundary-bearer-123456";
    await seedUser(bearer, 7);
    const payload = copySuccess();
    payload.events[1].stdout_chunks = 2;
    payload.events[1].stderr_chunks = 0;
    payload.output_chunks[0] = {
      ...payload.output_chunks[0],
      text: "Authorization: Bearer terminal-boundary-",
      chunk_index: 0,
    };
    payload.output_chunks[1] = {
      ...payload.output_chunks[1],
      stream: "stdout",
      text: "bearer-123456\n",
      chunk_index: 1,
    };
    for (const chunk of payload.output_chunks) {
      const bytes = new TextEncoder().encode(chunk.text);
      chunk.byte_count = bytes.byteLength;
      chunk.sha256 = await sha256Hex(bytes);
    }

    const response = await postTrace(payload, bearer, "203.0.113.45");
    const body = await response.json() as any;

    expect(response.status).toBe(422);
    expect(body.error.code).toBe("TRACE_INCOMPLETE");
    expect(body.error.details).toEqual(expect.arrayContaining(payload.output_chunks.map((chunk: any) => ({
      entity: "output_chunk",
      id: chunk.chunk_id,
      code: "credential_rejected",
      message: "凭据跨越原始输出分块边界，请客户端先对完整逻辑流脱敏后重试。",
    }))));
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs")).toBe(0);
  });

  it("acks an intra-chunk redacted chunk when the same raw ID is retried alone", async () => {
    const bearer = "intra-chunk-bearer-654321";
    await seedUser(bearer, 7);
    const canonical = copySuccess();
    const run = {
      ...canonical.runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    };
    const event = { ...canonical.events[0], stdout_chunks: 1 };
    const chunk = {
      ...canonical.output_chunks[0],
      event_id: event.event_id,
      text: `Authorization: Bearer ${bearer}\n`,
    };
    const bytes = new TextEncoder().encode(chunk.text);
    chunk.byte_count = bytes.byteLength;
    chunk.sha256 = await sha256Hex(bytes);

    const first = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004302",
      runs: [run],
      events: [event],
      output_chunks: [chunk],
    }, bearer, "203.0.113.45");
    const retry = await postTrace({
      schema_version: 2,
      upload_id: "019d9c40-7b3c-7000-8000-000000004303",
      runs: [],
      events: [],
      output_chunks: [chunk],
    }, bearer, "203.0.113.45");

    expect((await first.json() as any).accepted.output_chunks).toEqual([chunk.chunk_id]);
    expect((await retry.json() as any).accepted.output_chunks).toEqual([chunk.chunk_id]);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks WHERE text LIKE ?", `%${bearer}%`)).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(1);
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

function copySuccess(): any {
  return JSON.parse(JSON.stringify(successFixture));
}

function expectTraceApiError(body: unknown, code: string, message: string): void {
  expect(body).toEqual({
    ok: false,
    error: {
      code,
      message,
      request_id: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
    },
  });
}

function concurrentPayload(index: number): any {
  const uploadSuffix = (100 + index).toString().padStart(12, "0");
  const eventSuffix = (200 + index).toString().padStart(12, "0");
  return {
    schema_version: 2,
    upload_id: `019d9c40-7b3c-7000-8000-${uploadSuffix}`,
    runs: [{
      ...copySuccess().runs[0],
      outcome: "running",
      ended_at_ms: null,
      duration_ms: null,
      final_sequence: null,
      trace_complete: false,
    }],
    events: [{
      ...copySuccess().events[0],
      event_id: `019d9c40-7b3c-7000-8000-${eventSuffix}`,
      status: "started",
      ended_at_ms: null,
      duration_ms: null,
    }],
    output_chunks: [],
  };
}

function appendEventPayload(): any {
  const payload = copySuccess();
  payload.upload_id = "019d9c40-7b3c-7000-8000-000000000088";
  payload.runs[0].outcome = "running";
  payload.runs[0].ended_at_ms = null;
  payload.runs[0].duration_ms = null;
  payload.runs[0].final_sequence = null;
  payload.runs[0].trace_complete = false;
  payload.events = [{
    ...payload.events[0],
    event_id: "019d9c40-7b3c-7000-8000-000000000089",
    sequence: 4,
    status: "started",
    ended_at_ms: null,
    duration_ms: null,
  }];
  payload.output_chunks = [];
  return payload;
}

function retryExhaustionPayload(): any {
  const payload = copySuccess();
  payload.upload_id = "019d9c40-7b3c-7000-8000-000000000078";
  payload.runs[0].outcome = "running";
  payload.runs[0].ended_at_ms = null;
  payload.runs[0].duration_ms = null;
  payload.runs[0].final_sequence = null;
  payload.runs[0].trace_complete = false;
  payload.events = payload.events.map((event: any) => ({
    ...event,
    stdout_chunks: 0,
    stderr_chunks: 0,
  }));
  payload.output_chunks = [];
  return payload;
}

function exactRetryExhaustionPayload(): any {
  const payload = copySuccess();
  payload.upload_id = "019d9c40-7b3c-7000-8000-000000000077";
  payload.runs[0].outcome = "running";
  payload.runs[0].ended_at_ms = null;
  payload.runs[0].duration_ms = null;
  payload.runs[0].final_sequence = null;
  payload.runs[0].trace_complete = false;
  return payload;
}

function collisionPerBatchDatabase(
  beforeBatch: (attempt: number) => Promise<void>,
): D1Database {
  let attempt = 0;
  return {
    prepare(query: string) {
      return env.DB.prepare(query);
    },
    async batch<T = unknown>(statements: D1PreparedStatement[]) {
      attempt += 1;
      await beforeBatch(attempt);
      return env.DB.batch<T>(statements);
    },
  } as D1Database;
}

function pairedBatchBarrierDatabases(): [D1Database, D1Database] {
  let arrivals = 0;
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  const wrap = (): D1Database => {
    let firstBatch = true;
    return {
      prepare(query: string) {
        return env.DB.prepare(query);
      },
      async batch<T = unknown>(statements: D1PreparedStatement[]) {
        if (firstBatch) {
          firstBatch = false;
          arrivals += 1;
          if (arrivals === 2) release();
          await ready;
        }
        return env.DB.batch<T>(statements);
      },
    } as D1Database;
  };
  return [wrap(), wrap()];
}

async function seedUser(token: string, id: number, enabled = 1, banned = 0): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO api_users (id, username, name, token, enabled, banned)
     VALUES (?, ?, ?, ?, ?, ?)`,
  ).bind(id, `user-${id}`, `User ${id}`, token, enabled, banned).run();
}

async function seedRunOwnedBy(userId: number, runId: string): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        client_version, started_at_ms, trace_complete)
     VALUES (?, ?, ?, 2, 'seed', 'Seed run', 'running', '1.4.0', 1, 0)`,
  ).bind(runId, userId, `User ${userId}`).run();
}

async function seedRunFromPayload(userId: number, run: any, sourceIp: string): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_operation_runs
       (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
        device_serial, source_ip, source_paths_json, source_urls_json, client_version,
        started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
        final_sequence, trace_complete, trace_loss_reason, credential_redactions_json)
     VALUES (?, ?, ?, 2, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]')`,
  ).bind(
    run.run_id,
    userId,
    `User ${userId}`,
    run.operation_kind,
    run.title,
    run.outcome,
    run.device_serial,
    sourceIp,
    JSON.stringify(run.source_paths),
    JSON.stringify(run.source_urls),
    run.client_version,
    run.started_at_ms,
    run.ended_at_ms,
    run.duration_ms,
    run.error_class,
    run.error_code,
    run.error_message,
    run.final_sequence,
    run.trace_complete ? 1 : 0,
    run.trace_loss_reason,
  ).run();
}

async function seedEventFromPayload(event: any): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_operation_events
       (event_id, run_id, sequence, event_kind, step_name, partition_name, status,
        started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
        command_line, working_directory, paths_json, urls_json, serial, exit_code,
        stdout_chunks, stderr_chunks, verification, device_state, retry_safe, remedies_json,
        error_class, error_code, error_message, credential_redactions_json)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
  ).bind(
    event.event_id,
    event.run_id,
    event.sequence,
    event.kind,
    event.step_name,
    event.partition_name,
    event.status,
    event.started_at_ms,
    event.ended_at_ms,
    event.duration_ms,
    event.command?.program ?? null,
    event.command === null ? null : JSON.stringify(event.command.argv),
    event.command?.display_command ?? null,
    event.command?.working_directory ?? null,
    JSON.stringify(event.command?.paths ?? []),
    JSON.stringify(event.command?.urls ?? []),
    event.command?.serial ?? null,
    event.exit_code,
    event.stdout_chunks,
    event.stderr_chunks,
    event.verification,
    event.device_state,
    event.retry_safe === null ? null : event.retry_safe ? 1 : 0,
    JSON.stringify(event.remedies),
    event.error_class,
    event.error_code,
    event.error_message,
    JSON.stringify(event.credential_redactions),
  ).run();
}

function openRunForQuota(run: any, runId: string): any {
  return {
    ...run,
    run_id: runId,
    outcome: "running",
    ended_at_ms: null,
    duration_ms: null,
    final_sequence: null,
    trace_complete: false,
  };
}

function quotaEvent(event: any, runId: string, eventId: string, sequence: number): any {
  return {
    ...event,
    event_id: eventId,
    run_id: runId,
    sequence,
    verification: "quota boundary",
  };
}

async function seedRunToEventStorageBytes(
  runId: string,
  targetBytes: number,
  canonicalEvent: any,
  firstIdSuffix: number,
): Promise<void> {
  for (let sequence = 1; sequence <= 9; sequence += 1) {
    await seedEventFromPayload({
      ...quotaEvent(
        canonicalEvent,
        runId,
        `019d9c40-7b3c-7000-8000-${(firstIdSuffix + sequence).toString().padStart(12, "0")}`,
        sequence,
      ),
      remedies: sequence <= 8 ? ["x".repeat(950_000)] : [],
    });
  }
  const currentBytes = await eventStorageBytes(runId);
  const delta = targetBytes - currentBytes;
  expect(delta).toBeGreaterThan(2);
  const lastEventId = `019d9c40-7b3c-7000-8000-${(firstIdSuffix + 9).toString().padStart(12, "0")}`;
  await env.DB.prepare(
    "UPDATE usage_operation_events SET remedies_json = ? WHERE event_id = ?",
  ).bind(JSON.stringify(["x".repeat(delta - 2)]), lastEventId).run();
  expect(await eventStorageBytes(runId)).toBe(targetBytes);
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

async function seedChunkFromPayload(chunk: any): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_output_chunks
       (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256, credential_redactions_json)
     VALUES (?, ?, ?, ?, ?, ?, ?, '[]')`,
  ).bind(
    chunk.chunk_id,
    chunk.event_id,
    chunk.stream,
    chunk.chunk_index,
    chunk.text,
    chunk.byte_count,
    chunk.sha256,
  ).run();
}

async function seedEvent(
  eventId: string,
  runId: string,
  sequence: number,
  stdoutChunks = 0,
  stderrChunks = 0,
): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_operation_events
       (event_id, run_id, sequence, event_kind, step_name, status, started_at_ms,
        stdout_chunks, stderr_chunks)
     VALUES (?, ?, ?, 'stage', 'Persisted event', 'success', 1, ?, ?)`,
  ).bind(eventId, runId, sequence, stdoutChunks, stderrChunks).run();
}

async function seedChunk(
  chunkId: string,
  eventId: string,
  stream: string,
  chunkIndex: number,
): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO usage_output_chunks
       (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256)
     VALUES (?, ?, ?, ?, '', 0, ?)`,
  ).bind(chunkId, eventId, stream, chunkIndex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855").run();
}

async function postTrace(payload: unknown, token: string | null, ip: string): Promise<Response> {
  const headers = new Headers({
    "Content-Type": "application/json",
    "X-Nwflash-Version": "1.4.0",
    "CF-Connecting-IP": ip,
  });
  if (token !== null) headers.set("Authorization", `Bearer ${token}`);
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/usage/traces/v2", {
    method: "POST",
    headers,
    body: JSON.stringify(payload),
  }), env);
}

async function postLegacyUsage(payload: unknown, token: string): Promise<Response> {
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/usage/logs", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${token}`,
      "X-Nwflash-Version": "1.4.0",
    },
    body: JSON.stringify(payload),
  }), env);
}

function traceRequest(payload: unknown, token: string, ip: string): Request {
  return new Request("https://api.nwflash.cc.cd/api/usage/traces/v2", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${token}`,
      "CF-Connecting-IP": ip,
    },
    body: JSON.stringify(payload),
  });
}

async function scalar(query: string, ...bindings: unknown[]): Promise<number> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function text(query: string, ...bindings: unknown[]): Promise<string | null> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: string | null }>();
  return row?.value ?? null;
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
