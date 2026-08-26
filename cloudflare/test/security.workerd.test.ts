import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Env as WorkerEnv } from "../src/index";
import adminWorker from "../web/src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const PASSWORD = "correct horse";
const SALT = "00112233445566778899aabbccddeeff";
const FIXED_NOW_MS = 1_787_444_800_000;
const ADMIN_SESSION_TOKEN = "workerd-admin-session";

beforeEach(async () => {
  vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS);
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

afterEach(() => vi.restoreAllMocks());

describe("actual Worker route with Workerd D1", () => {
  it("atomically exchanges one revoked token for concurrent signed logins", async () => {
    const supersededToken = "workerd-superseded-token";
    const revokedMarker = `revoked:${"ab".repeat(32)}`;
    await seedUser(supersededToken);
    await env.DB.prepare("UPDATE api_users SET token = ? WHERE id = 7 AND token = ?")
      .bind(revokedMarker, supersededToken)
      .run();
    await env.DB.batch([
      env.DB.prepare("CREATE TABLE token_exchange_audit (token TEXT NOT NULL)"),
      env.DB.prepare(
        `CREATE TRIGGER audit_token_exchange
         AFTER UPDATE OF token ON api_users
         WHEN OLD.token LIKE 'revoked:%'
         BEGIN INSERT INTO token_exchange_audit (token) VALUES (NEW.token); END`,
      ),
    ]);

    const responses = await Promise.all([
      postLogin("workerd-revived-session-a"),
      postLogin("workerd-revived-session-b"),
    ]);
    const bodies = await Promise.all(
      responses.map((response) => response.json() as Promise<Record<string, unknown>>),
    );
    const tokens = bodies.map((body) => String(body.token));
    const activeToken = tokens[0];
    const stored = await env.DB.prepare("SELECT token FROM api_users WHERE id = 7").first<{ token: string }>();

    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(new Set(tokens)).toEqual(new Set([activeToken]));
    expect(activeToken).toMatch(/^[0-9a-f]{64}$/);
    expect(stored?.token).toBe(activeToken);
    expect(await scalar("SELECT COUNT(*) AS value FROM token_exchange_audit")).toBe(1);
    expect(bodies.every((body) => !JSON.stringify(body).includes("revoked:"))).toBe(true);
    for (const [index, body] of bodies.entries()) {
      expect(decodeLeaseClaims(body)).toMatchObject({
        kind: "login",
        token_sha256: await sha256Base64Url(activeToken),
        session_id: index === 0 ? "workerd-revived-session-a" : "workerd-revived-session-b",
        sequence: 1,
      });
    }

    for (const rejectedToken of [supersededToken, revokedMarker]) {
      expect((await getMe(rejectedToken)).status).toBe(401);
      expect((await postHeartbeat(rejectedToken, "workerd-revived-session-a", 1)).status).toBe(401);
    }

    const heartbeat = await postHeartbeat(activeToken, "workerd-revived-session-a", 1);
    const heartbeatBody = await heartbeat.json() as Record<string, unknown>;
    expect(heartbeat.status).toBe(200);
    expect(decodeLeaseClaims(heartbeatBody)).toMatchObject({
      kind: "heartbeat",
      token_sha256: await sha256Base64Url(activeToken),
      sequence: 2,
    });
  });

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

    expect(responses.map((response) => response.status).sort()).toEqual([200, 409]);
    expect(bodies.filter((body) => "lease_payload" in body)).toHaveLength(1);
    expect(state?.sequence).toBe(2);
  });

  it("lets the production admin kick linearize before heartbeat CAS without advancing sequence", async () => {
    const token = "workerd-kick-first-token";
    const sessionId = "workerd-kick-first-session";
    await establishOnlineSession(token, sessionId);
    await seedAdminSession();
    vi.mocked(Date.now).mockReturnValue(FIXED_NOW_MS + 4_000);

    const kick = await postAdminKick(sessionId, "kick first");
    const response = await postHeartbeat(token, sessionId, 2);
    const body = await response.json() as Record<string, unknown>;

    expect(kick.status).toBe(200);
    expect(response.status).toBe(200);
    expect(body).toMatchObject({ ok: true, force_exit: true, reason: "kick first" });
    expect(body).not.toHaveProperty("lease_payload");
    expect(body).not.toHaveProperty("lease_signature");
    expect(await sessionSequence(sessionId)).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM admin_audit_log WHERE target_session_id = ?", sessionId)).toBe(1);
  });

  it("lets heartbeat CAS advance once before the production admin kick blocks the next heartbeat", async () => {
    const token = "workerd-cas-first-token";
    const sessionId = "workerd-cas-first-session";
    await establishOnlineSession(token, sessionId);
    await seedAdminSession();
    vi.mocked(Date.now).mockReturnValue(FIXED_NOW_MS + 4_000);

    const advanced = await postHeartbeat(token, sessionId, 2);
    const advancedBody = await advanced.json() as Record<string, unknown>;
    const kick = await postAdminKick(sessionId, "CAS first");
    const blocked = await postHeartbeat(token, sessionId, 3);
    const blockedBody = await blocked.json() as Record<string, unknown>;

    expect(advanced.status).toBe(200);
    expect(advancedBody).toHaveProperty("lease_payload");
    expect(kick.status).toBe(200);
    expect(blocked.status).toBe(200);
    expect(blockedBody).toMatchObject({ ok: true, force_exit: true, reason: "CAS first" });
    expect(blockedBody).not.toHaveProperty("lease_payload");
    expect(blockedBody).not.toHaveProperty("lease_signature");
    expect(await sessionSequence(sessionId)).toBe(3);
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

async function seedAdminSession(): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO admins (id, username, salt, password_hash) VALUES (11, 'reviewer', 'unused', 'unused')",
    ),
    env.DB.prepare(
      "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (11, ?, '2999-01-01T00:00:00.000Z')",
    ).bind(ADMIN_SESSION_TOKEN),
  ]);
}

async function establishOnlineSession(token: string, sessionId: string): Promise<void> {
  await seedUser(token);
  expect((await postLogin(sessionId)).status).toBe(200);
  expect((await postHeartbeat(token, sessionId, 1)).status).toBe(200);
  expect(await sessionSequence(sessionId)).toBe(2);
  expect(await scalar("SELECT COUNT(*) AS value FROM online_sessions WHERE session_id = ?", sessionId)).toBe(1);
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

async function getMe(token: string): Promise<Response> {
  return exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/me", {
    headers: {
      "X-Nwflash-Version": "1.4.0",
      Authorization: `Bearer ${token}`,
    },
  }), env);
}

async function postAdminKick(sessionId: string, reason: string): Promise<Response> {
  return adminWorker.fetch(new Request("https://web.nwflash.cc.cd/api/online/kick", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: `nwflash_session=${ADMIN_SESSION_TOKEN}`,
      "X-Requested-With": "XMLHttpRequest",
    },
    body: JSON.stringify({ sessionId, reason }),
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

function decodeLeaseClaims(body: Record<string, unknown>): Record<string, unknown> {
  const payload = String(body.lease_payload);
  const padded = payload.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(payload.length / 4) * 4, "=");
  return JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(padded), (character) => character.charCodeAt(0)))) as Record<string, unknown>;
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

async function sessionSequence(sessionId: string): Promise<number> {
  return scalar("SELECT sequence AS value FROM session_leases WHERE session_id = ?", sessionId);
}
