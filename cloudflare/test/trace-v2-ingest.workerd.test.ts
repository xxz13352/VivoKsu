import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

import successAckFixture from "../contracts/trace-v2/upload-ack.success.json";
import successFixture from "../contracts/trace-v2/upload.success.json";
import type { Env as WorkerEnv } from "../src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
    TEST_TRACE_V2_MIGRATIONS: D1Migration[];
  }
}

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await applyD1Migrations(env.DB, env.TEST_TRACE_V2_MIGRATIONS);
});

describe("POST /api/usage/traces/v2", () => {
  it("acks the canonical upload and projects one terminal V1 summary", async () => {
    await seedUser("trace-bearer", 7);

    const response = await postTrace(successFixture, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(successAckFixture);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs WHERE api_user_id = 7")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", successFixture.runs[0].run_id)).toBe(1);
    expect(await text("SELECT source_ip AS value FROM usage_operation_runs WHERE run_id = ?", successFixture.runs[0].run_id)).toBe("203.0.113.45");
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
    await seedRunOwnedBy(7, successFixture.runs[0].run_id);
    await seedEvent(
      "019d9c40-7b3c-7000-8000-000000000099",
      successFixture.runs[0].run_id,
      2,
    );
    const payload = copySuccess();
    payload.runs[0].outcome = "running";
    payload.runs[0].trace_complete = false;

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

  it("finalizes an existing run from persisted and current evidence", async () => {
    await seedUser("trace-bearer", 7);
    await seedRunOwnedBy(7, successFixture.runs[0].run_id);
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
    const payload = copySuccess();
    payload.events.splice(1, 1);
    payload.output_chunks = [];

    const response = await postTrace(payload, "trace-bearer", "203.0.113.45");

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      ok: true,
      accepted: {
        runs: [successFixture.runs[0].run_id],
        events: [successFixture.events[0].event_id, successFixture.events[2].event_id],
        output_chunks: [],
      },
      rejected: [],
    });
    expect(await scalar("SELECT trace_complete AS value FROM usage_operation_runs WHERE run_id = ?", successFixture.runs[0].run_id)).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_events")).toBe(3);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_output_chunks")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key = ?", successFixture.runs[0].run_id)).toBe(1);
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

  it("acks same-user duplicate IDs without duplicating persisted rows", async () => {
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

  it("rejects a conflicting output stream index without acknowledging its uncommitted ID", async () => {
    await seedUser("trace-bearer", 7);
    await seedRunOwnedBy(7, successFixture.runs[0].run_id);
    await seedEvent(
      successFixture.events[1].event_id,
      successFixture.runs[0].run_id,
      2,
      successFixture.events[1].stdout_chunks,
      successFixture.events[1].stderr_chunks,
    );
    await seedChunk(
      "019d9c40-7b3c-7000-8000-000000000099",
      successFixture.events[1].event_id,
      "stdout",
      0,
    );
    const payload = copySuccess();
    payload.runs[0].outcome = "running";
    payload.runs[0].trace_complete = false;

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
    await seedEvent(foreignEventId, foreignRunId, 1);
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
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("rejects a disabled bearer user", async () => {
    await seedUser("disabled-bearer", 7, 0, 0);

    const response = await postTrace(successFixture, "disabled-bearer", "203.0.113.45");

    expect(response.status).toBe(401);
    expect(await scalar("SELECT COUNT(*) AS value FROM usage_operation_runs")).toBe(0);
  });

  it("rejects a banned bearer user", async () => {
    await seedUser("banned-bearer", 7, 1, 1);

    const response = await postTrace(successFixture, "banned-bearer", "203.0.113.45");

    expect(response.status).toBe(403);
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
});

function copySuccess(): any {
  return JSON.parse(JSON.stringify(successFixture));
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
