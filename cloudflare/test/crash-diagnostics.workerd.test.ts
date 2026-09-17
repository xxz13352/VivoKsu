import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import apiWorker, { type Env as WorkerEnv } from "../src/index";
import { purgeExpiredCrashData } from "../src/crash-diagnostics";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const FIXED_NOW_MS = 1_787_444_800_000;
const PASSWORD = "correct horse";
const SALT = "00112233445566778899aabbccddeeff";

beforeEach(async () => {
  vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS);
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

afterEach(async () => vi.restoreAllMocks());

describe("POST /api/diagnostics/crash", () => {
  it("accepts an anonymous crash report and stores it without a user", async () => {
    const response = await postCrash("crash-anon-1", undefined, "203.0.113.80");

    expect(response.status).toBe(202);
    expect(await response.json()).toEqual({ ok: true, accepted: true });
    const row = await env.DB.prepare(
      "SELECT api_user_id, trusted, panic_message, backtrace, client_version, build_id, session_id FROM crash_reports WHERE event_id = ?",
    ).bind("crash-anon-1").first<{
      api_user_id: number | null;
      trusted: number;
      panic_message: string;
      backtrace: string;
      client_version: string;
      build_id: string;
      session_id: string;
    }>();
    expect(row).toMatchObject({
      api_user_id: null,
      trusted: 0,
      panic_message: "panicked at src/main.rs:42",
      backtrace: "0: nwflash::main",
      client_version: "1.4.0",
      build_id: "build-workerd",
      session_id: "session-workerd",
    });
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_report_claims")).toBe(0);
    expect(await scalar("SELECT count AS value FROM crash_report_rate_limits")).toBe(1);
  });

  it("binds a trusted report to the authenticated user", async () => {
    const token = "workerd-crash-token";
    await seedUser(token);

    const response = await postCrash("crash-auth-1", token, "203.0.113.81");

    expect(response.status).toBe(202);
    expect(await scalar("SELECT api_user_id AS value FROM crash_reports WHERE event_id = 'crash-auth-1'")).toBe(7);
    expect(await scalar("SELECT trusted AS value FROM crash_reports WHERE event_id = 'crash-auth-1'")).toBe(1);
  });

  it("returns 401 when an Authorization header carries an invalid token", async () => {
    const response = await postCrash("crash-bad-auth-1", "invalid-token", "203.0.113.82");

    expect(response.status).toBe(401);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports")).toBe(0);
  });

  it("is idempotent for a concurrent duplicate event id", async () => {
    const eventId = "crash-duplicate-1";
    const responses = await Promise.all([
      postCrash(eventId, undefined, "203.0.113.83"),
      postCrash(eventId, undefined, "203.0.113.83"),
      postCrash(eventId, undefined, "203.0.113.83"),
    ]);

    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<Record<string, unknown>>));
    expect(responses.filter((response) => response.status === 202)).toHaveLength(1);
    expect(responses.filter((response) => response.status === 200)).toHaveLength(2);
    expect(bodies.filter((body) => body.duplicate === true)).toHaveLength(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports WHERE event_id = ?", eventId)).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_report_claims")).toBe(0);
    expect(await scalar("SELECT COALESCE(SUM(count), 0) AS value FROM crash_report_rate_limits")).toBe(1);
  });

  it("rejects over-quota reports with 429 and persists no event or claim", async () => {
    const ip = "203.0.113.84";
    const windowStart = currentCrashWindow();
    const ipHash = await sha256Base64Url(ip);
    await env.DB.prepare(
      "INSERT INTO crash_report_rate_limits (ip_hash, window_start, count) VALUES (?, ?, 5)",
    ).bind(ipHash, windowStart).run();

    const response = await postCrash("crash-over-quota-1", undefined, ip);

    expect(response.status).toBe(429);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_report_claims")).toBe(0);
  });

  it("rejects an invalid payload with 400 and leaves nothing behind", async () => {
    const extra = await exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/diagnostics/crash", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        event_id: "crash-extra-field",
        client_version: "1.4.0",
        build_id: "build-workerd",
        session_id: "session-workerd",
        panic_message: "boom",
        backtrace: "",
        occurred_at: Math.floor(FIXED_NOW_MS / 1000),
        password: "must-not-be-accepted",
      }),
    }), env);

    const missing = await exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/diagnostics/crash", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ event_id: "crash-missing-field" }),
    }), env);

    const emptyPanic = await postCrash("crash-empty-panic", undefined, "203.0.113.85", { panic_message: "" });

    expect(extra.status).toBe(400);
    expect(missing.status).toBe(400);
    expect(emptyPanic.status).toBe(400);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_report_claims")).toBe(0);
  });

  it("rejects an oversized body with 413 before parsing", async () => {
    const oversized = await exports.default.fetch(new Request("https://api.nwflash.cc.cd/api/diagnostics/crash", {
      method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": String(70_000) },
      body: "x".repeat(70_000),
    }), env);

    expect(oversized.status).toBe(413);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports")).toBe(0);
  });

  it("purges expired rate rows and old reports via the cron helper", async () => {
    const oldIpHash = await sha256Base64Url("203.0.113.86");
    const windowStart = currentCrashWindow();
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO crash_reports (event_id, client_version, build_id, session_id, panic_message, occurred_at, created_at) VALUES ('crash-old', '1.4.0', 'build-workerd', 'session-workerd', 'old', 100, 100)",
      ),
      env.DB.prepare(
        "INSERT INTO crash_report_rate_limits (ip_hash, window_start, count) VALUES (?, ?, 1)",
      ).bind(oldIpHash, windowStart - 3_000),
    ]);

    await purgeExpiredCrashData(env.DB, FIXED_NOW_MS);

    expect(await scalar("SELECT COUNT(*) AS value FROM crash_reports")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM crash_report_rate_limits")).toBe(0);
  });
});

async function seedUser(token: string): Promise<void> {
  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey("raw", encoder.encode(PASSWORD), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    {
      name: "PBKDF2",
      salt: Uint8Array.from((SALT.match(/.{2}/g) ?? []).map((pair) => Number.parseInt(pair, 16))),
      iterations: 100_000,
      hash: "SHA-256",
    },
    key,
    256,
  );
  const hash = [...new Uint8Array(bits)].map((b) => b.toString(16).padStart(2, "0")).join("");
  await env.DB.prepare(
    `INSERT INTO api_users
       (id, username, name, token, password, salt, enabled, banned)
     VALUES (7, 'alice', 'Alice', ?, ?, ?, 1, 0)`,
  ).bind(token, hash, SALT).run();
}

async function postCrash(
  eventId: string,
  token: string | undefined,
  ip: string,
  overrides: Record<string, unknown> = {},
): Promise<Response> {
  const headers: Record<string, string> = { "Content-Type": "application/json", "CF-Connecting-IP": ip };
  if (token !== undefined) headers.Authorization = `Bearer ${token}`;
  return apiWorker.fetch(new Request("https://api.nwflash.cc.cd/api/diagnostics/crash", {
    method: "POST",
    headers,
    body: JSON.stringify({
      event_id: eventId,
      client_version: "1.4.0",
      build_id: "build-workerd",
      session_id: "session-workerd",
      panic_message: "panicked at src/main.rs:42",
      backtrace: "0: nwflash::main",
      occurred_at: Math.floor(FIXED_NOW_MS / 1000),
      ...overrides,
    }),
  }), env);
}

function currentCrashWindow(): number {
  const now = Math.floor(FIXED_NOW_MS / 1000);
  return Math.floor(now / 600) * 600;
}

async function sha256Base64Url(ip: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(ip));
  return btoa(String.fromCharCode(...new Uint8Array(digest)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

async function scalar(sql: string, ...params: unknown[]): Promise<unknown> {
  const row = await env.DB.prepare(sql.replace("AS value", "AS value")).bind(...params).first<{ value: unknown }>();
  return row?.value;
}
