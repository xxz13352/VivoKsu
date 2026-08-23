import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

import type { Env as WorkerEnv } from "../src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const PASSWORD = "correct horse";
const SALT = "00112233445566778899aabbccddeeff";

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

describe("actual Worker route with Workerd D1", () => {
  it("allows only one signed lease for concurrent same-sequence heartbeats", async () => {
    const token = "workerd-heartbeat-token";
    await seedUser(token);
    expect((await postLogin("workerd-heartbeat-session")).status).toBe(200);

    const responses = await Promise.all([
      postHeartbeat(token, "workerd-heartbeat-session", 1),
      postHeartbeat(token, "workerd-heartbeat-session", 1),
    ]);
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<Record<string, unknown>>));
    const state = await env.DB.prepare(
      "SELECT sequence FROM session_leases WHERE session_id = ?",
    ).bind("workerd-heartbeat-session").first<{ sequence: number }>();

    expect(responses.filter((response) => response.status === 200)).toHaveLength(1);
    expect(responses.filter((response) => response.status === 409 || response.status === 429)).toHaveLength(1);
    expect(bodies.filter((body) => "lease_payload" in body)).toHaveLength(1);
    expect(state?.sequence).toBe(2);
  });

  it("does not return a lease or advance sequence when concurrent force-exit wins", async () => {
    const token = "workerd-force-exit-token";
    const sessionId = "workerd-force-exit-session";
    await seedUser(token);
    expect((await postLogin(sessionId)).status).toBe(200);
    const now = Math.floor(Date.now() / 1000);
    await env.DB.prepare(
      `INSERT INTO online_sessions
         (session_id, user_id, user_name, client_version, ip, connected_at, last_seen_at)
       VALUES (?, 7, 'Alice', '1.4.0', '', ?, ?)`,
    ).bind(sessionId, now, now).run();

    const forceExit = env.DB.prepare(
      "UPDATE online_sessions SET force_exit_at = ?, force_exit_reason = 'integration race' WHERE session_id = ?",
    ).bind(now + 1, sessionId).run();
    const heartbeat = postHeartbeat(token, sessionId, 1);
    const [, response] = await Promise.all([forceExit, heartbeat]);
    const body = await response.json() as Record<string, unknown>;
    const state = await env.DB.prepare(
      "SELECT sequence FROM session_leases WHERE session_id = ?",
    ).bind(sessionId).first<{ sequence: number }>();

    expect([200, 409]).toContain(response.status);
    expect(body).not.toHaveProperty("lease_payload");
    expect(body).not.toHaveProperty("lease_signature");
    expect(state?.sequence).toBe(1);
  });

  it("commits one accepted event and one quota charge for concurrent duplicates", async () => {
    const eventId = "workerd-accepted-duplicate";
    const ip = "203.0.113.70";
    const responses = await Promise.all(Array.from({ length: 8 }, () => postTelemetry(eventId, ip)));

    expect(responses.filter((response) => response.status === 202)).toHaveLength(1);
    expect(responses.filter((response) => response.status === 200)).toHaveLength(7);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_events WHERE event_id = ?", eventId)).toBe(1);
    expect(await scalar("SELECT COALESCE(SUM(count), 0) AS value FROM integrity_rate_limits")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_event_claims")).toBe(0);
  });

  it("returns 429 and leaves no events or claims for concurrent over-quota duplicates", async () => {
    const eventId = "workerd-over-quota-duplicate";
    const ip = "203.0.113.71";
    await seedRateLimit(ip, 20);

    const responses = await Promise.all([postTelemetry(eventId, ip), postTelemetry(eventId, ip)]);

    expect(responses.map((response) => response.status).sort()).toEqual([429, 429]);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_events WHERE event_id = ?", eventId)).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_event_claims")).toBe(0);
    expect(await scalar("SELECT COALESCE(SUM(count), 0) AS value FROM integrity_rate_limits")).toBe(21);
  });

  it("does not accumulate claims for many unique over-quota event IDs", async () => {
    const ip = "203.0.113.72";
    await seedRateLimit(ip, 20);

    const responses = await Promise.all(Array.from({ length: 32 }, (_, index) =>
      postTelemetry(`workerd-over-quota-${index}`, ip),
    ));

    expect(responses.every((response) => response.status === 429)).toBe(true);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_events")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_event_claims")).toBe(0);
  });

  it("rolls back the temporary claim when the real D1 rate update errors", async () => {
    const eventId = "workerd-rate-error";
    const ip = "203.0.113.73";
    const ipHash = await sha256Base64Url(ip);
    const windowStart = currentWindowStart();
    await env.DB.prepare(
      "INSERT INTO integrity_rate_limits (ip_hash, window_start, count) VALUES (?, ?, 0)",
    ).bind(ipHash, windowStart).run();
    await env.DB.prepare(
      `CREATE TRIGGER fail_test_rate_update
       BEFORE UPDATE ON integrity_rate_limits
       WHEN NEW.ip_hash = '${ipHash}'
       BEGIN SELECT RAISE(ABORT, 'forced rate update failure'); END`,
    ).run();

    const response = await postTelemetry(eventId, ip);

    expect(response.status).toBe(500);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_events WHERE event_id = ?", eventId)).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM integrity_event_claims")).toBe(0);
    expect(await scalar("SELECT count AS value FROM integrity_rate_limits WHERE ip_hash = ?", ipHash)).toBe(0);
  });
});

async function seedUser(token: string): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO api_users
       (id, username, name, token, password, salt, enabled, banned)
     VALUES (7, 'alice', 'Alice', ?, ?, ?, 1, 0)`,
  ).bind(token, await passwordHash(PASSWORD, SALT), SALT).run();
}

async function postLogin(sessionId: string): Promise<Response> {
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/login", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Nwflash-Version": "1.4.0" },
    body: JSON.stringify({
      username: "alice",
      password: PASSWORD,
      client_version: "1.4.0",
      build_id: "build-workerd",
      process_nonce: "nonce-workerd",
      session_id: sessionId,
    }),
  }), env);
}

async function postHeartbeat(token: string, sessionId: string, sequence: number): Promise<Response> {
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/heartbeat", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Nwflash-Version": "1.4.0",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify({
      sessionId,
      clientVersion: "1.4.0",
      active: true,
      build_id: "build-workerd",
      process_nonce: "nonce-workerd",
      sequence,
    }),
  }), env);
}

async function postTelemetry(eventId: string, ip: string): Promise<Response> {
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/integrity/report", {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": ip },
    body: JSON.stringify({
      event_id: eventId,
      phase: "startup",
      reason: "image_crc_invalid",
      client_version: "1.4.0",
      build_id: "build-workerd",
      occurred_at: Math.floor(Date.now() / 1000),
    }),
  }), env);
}

async function seedRateLimit(ip: string, count: number): Promise<void> {
  await env.DB.prepare(
    "INSERT INTO integrity_rate_limits (ip_hash, window_start, count) VALUES (?, ?, ?)",
  ).bind(await sha256Base64Url(ip), currentWindowStart(), count).run();
}

function currentWindowStart(): number {
  const now = Math.floor(Date.now() / 1000);
  return Math.floor(now / 60) * 60;
}

async function sha256Base64Url(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  let binary = "";
  for (const byte of new Uint8Array(digest)) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

async function passwordHash(password: string, saltHex: string): Promise<string> {
  const salt = Uint8Array.from(saltHex.match(/.{2}/g) ?? [], (pair) => Number.parseInt(pair, 16));
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(password), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", salt, iterations: 100_000, hash: "SHA-256" },
    key,
    256,
  );
  return [...new Uint8Array(bits)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function scalar(query: string, ...bindings: unknown[]): Promise<number> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: number }>();
  return Number(row?.value ?? 0);
}
