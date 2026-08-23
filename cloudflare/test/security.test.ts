import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import worker, { type Env } from "../src/index";
import {
  importSigningKey,
  signLease,
  type LeaseClaims,
} from "../src/security";

const FIXED_NOW_MS = 1_787_444_800_000;
const TEST_TOKEN = "test-bearer-token";
const TEST_SALT = "00112233445566778899aabbccddeeff";

interface TestUser {
  id: number;
  username: string;
  name: string;
  token: string;
  password: string;
  salt: string;
  enabled: number;
  banned: number;
}

interface StoredIntegrityEvent {
  eventId: string;
  userId: number | null;
  trusted: number;
  phase: string;
  reason: string;
  clientVersion: string;
  buildId: string;
  occurredAt: number;
}

class FakeD1Database {
  readonly users: TestUser[] = [];
  readonly sessions = new Map<string, {
    user_id: number;
    last_seen_at: number;
    force_exit_at: number | null;
    force_exit_reason: string | null;
  }>();
  readonly integrityEvents = new Map<string, StoredIntegrityEvent>();
  readonly rateLimits = new Map<string, number>();

  prepare(query: string): FakeD1PreparedStatement {
    return new FakeD1PreparedStatement(this, query);
  }

  async batch(statements: FakeD1PreparedStatement[]): Promise<unknown[]> {
    return Promise.all(statements.map((statement) => statement.run()));
  }
}

class FakeD1PreparedStatement {
  private values: unknown[] = [];

  constructor(
    private readonly db: FakeD1Database,
    private readonly query: string,
  ) {}

  bind(...values: unknown[]): this {
    this.values = values;
    return this;
  }

  async first<T>(): Promise<T | null> {
    const sql = normalizedSql(this.query);
    if (sql.includes("from api_users where username = ?")) {
      return (this.db.users.find((user) => user.username === this.values[0]) ?? null) as T | null;
    }
    if (sql.includes("from api_users where token = ?")) {
      const user = this.db.users.find((candidate) => candidate.token === this.values[0]);
      return (user ? {
        id: user.id,
        username: user.username,
        name: user.name,
        enabled: user.enabled,
        banned: user.banned,
      } : null) as T | null;
    }
    if (sql.includes("from online_sessions where session_id = ?")) {
      return (this.db.sessions.get(String(this.values[0])) ?? null) as T | null;
    }
    if (sql.includes("from integrity_events where event_id = ?")) {
      const event = this.db.integrityEvents.get(String(this.values[0]));
      return (event ? { event_id: event.eventId } : null) as T | null;
    }
    if (sql.startsWith("insert into integrity_rate_limits")) {
      const key = `${String(this.values[0])}|${Number(this.values[1])}`;
      const count = (this.db.rateLimits.get(key) ?? 0) + 1;
      this.db.rateLimits.set(key, count);
      return { count } as T;
    }
    throw new Error(`Unhandled D1 first(): ${this.query}`);
  }

  async all<T>(): Promise<{ results: T[] }> {
    const sql = normalizedSql(this.query);
    if (sql.includes("from app_versions")) return { results: [] };
    throw new Error(`Unhandled D1 all(): ${this.query}`);
  }

  async run(): Promise<{ success: boolean; meta: { changes: number } }> {
    const sql = normalizedSql(this.query);
    if (sql.startsWith("insert into online_sessions")) {
      const [sessionId, userId, , , , , lastSeenAt] = this.values;
      const key = String(sessionId);
      const current = this.db.sessions.get(key);
      if (!current || current.user_id === Number(userId)) {
        this.db.sessions.set(key, {
          user_id: Number(userId),
          last_seen_at: Number(lastSeenAt),
          force_exit_at: current?.force_exit_at ?? null,
          force_exit_reason: current?.force_exit_reason ?? null,
        });
      }
      return changed();
    }
    if (sql.startsWith("update online_sessions")) {
      const sessionId = String(this.values[3]);
      const current = this.db.sessions.get(sessionId);
      if (current) current.last_seen_at = Number(this.values[0]);
      return changed(current ? 1 : 0);
    }
    if (sql.startsWith("delete from online_sessions where session_id = ? and user_id = ?")) {
      const sessionId = String(this.values[0]);
      const current = this.db.sessions.get(sessionId);
      if (current?.user_id === Number(this.values[1])) this.db.sessions.delete(sessionId);
      return changed(current ? 1 : 0);
    }
    if (sql.startsWith("delete from online_sessions")) return changed(0);
    if (sql.startsWith("insert into integrity_events")) {
      const [eventId, userId, trusted, phase, reason, clientVersion, buildId, occurredAt] = this.values;
      const key = String(eventId);
      if (this.db.integrityEvents.has(key)) return changed(0);
      this.db.integrityEvents.set(key, {
        eventId: key,
        userId: userId == null ? null : Number(userId),
        trusted: Number(trusted),
        phase: String(phase),
        reason: String(reason),
        clientVersion: String(clientVersion),
        buildId: String(buildId),
        occurredAt: Number(occurredAt),
      });
      return changed();
    }
    throw new Error(`Unhandled D1 run(): ${this.query}`);
  }
}

function normalizedSql(sql: string): string {
  return sql.replace(/\s+/g, " ").trim().toLowerCase();
}

function changed(changes = 1): { success: boolean; meta: { changes: number } } {
  return { success: true, meta: { changes } };
}

function base64Url(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const byte of view) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function decodeBase64Url(value: string): Uint8Array {
  const padded = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
}

async function ephemeralSigningFixture(): Promise<{ secret: string; publicKey: CryptoKey }> {
  const generated = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const pkcs8 = await crypto.subtle.exportKey("pkcs8", generated.privateKey);
  const spki = await crypto.subtle.exportKey("spki", generated.publicKey);
  const publicKey = await crypto.subtle.importKey("spki", spki, { name: "Ed25519" }, false, ["verify"]);
  return { secret: base64Url(pkcs8), publicKey };
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

async function testEnv(options: { signingSecret?: boolean } = {}): Promise<{ env: Env; db: FakeD1Database; publicKey: CryptoKey }> {
  const fixture = await ephemeralSigningFixture();
  const db = new FakeD1Database();
  db.users.push({
    id: 7,
    username: "alice",
    name: "Alice",
    token: TEST_TOKEN,
    password: await passwordHash("correct horse", TEST_SALT),
    salt: TEST_SALT,
    enabled: 1,
    banned: 0,
  });
  return {
    env: {
      DB: db as unknown as D1Database,
      VOTA_API_TOKEN: "unused-in-tests",
      SESSION_SIGNING_PRIVATE_KEY_PKCS8: options.signingSecret === false ? undefined : fixture.secret,
    },
    db,
    publicKey: fixture.publicKey,
  };
}

async function fetchWorker(env: Env, path: string, init?: RequestInit): Promise<Response> {
  return worker.fetch(new Request(`https://api.nwflash.cc.cd${path}`, init), env);
}

async function verifyEnvelope(
  publicKey: CryptoKey,
  payload: string,
  signature: string,
): Promise<boolean> {
  return crypto.subtle.verify(
    { name: "Ed25519" },
    publicKey,
    decodeBase64Url(signature),
    new TextEncoder().encode(payload),
  );
}

function telemetry(eventId: string): Record<string, unknown> {
  return {
    event_id: eventId,
    phase: "startup",
    reason: "image_crc_invalid",
    client_version: "1.4.0",
    build_id: "build-2026-08-23",
    occurred_at: Math.floor(FIXED_NOW_MS / 1000),
  };
}

describe("Ed25519 security helpers", () => {
  it("imports the Env PKCS#8 signing key as non-extractable", async () => {
    const { secret } = await ephemeralSigningFixture();
    const key = await importSigningKey(secret);

    expect(key.extractable).toBe(false);
    await expect(crypto.subtle.exportKey("pkcs8", key)).rejects.toThrow();
  });

  it("signs the original unpadded base64url lease payload text", async () => {
    const { secret, publicKey } = await ephemeralSigningFixture();
    const claims: LeaseClaims = {
      version: 1,
      kind: "login",
      username: "alice",
      token_sha256: "V7uSjCctjUm1dtAtW7zeyJ4w8d2vJOZbIQQX8iSbhZs",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      session_id: "session-abc",
      sequence: 1,
      issued_at: 1_787_444_800,
      expires_at: 1_787_444_920,
    };

    const envelope = await signLease(claims, secret);

    expect(envelope.lease_payload).not.toContain("=");
    expect(envelope.lease_signature).not.toContain("=");
    expect(await verifyEnvelope(publicKey, envelope.lease_payload, envelope.lease_signature)).toBe(true);
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(envelope.lease_payload)))).toEqual(claims);
  });

  it("does not allow a signed lease field to be changed after signing", async () => {
    const { secret, publicKey } = await ephemeralSigningFixture();
    const claims: LeaseClaims = {
      version: 1,
      kind: "heartbeat",
      username: "alice",
      token_sha256: "V7uSjCctjUm1dtAtW7zeyJ4w8d2vJOZbIQQX8iSbhZs",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      session_id: "session-abc",
      sequence: 9,
      issued_at: 1_787_444_800,
      expires_at: 1_787_444_920,
    };
    const envelope = await signLease(claims, secret);
    const tampered = { ...claims, sequence: 10 };
    const tamperedPayload = base64Url(new TextEncoder().encode(JSON.stringify(tampered)));

    expect(await verifyEnvelope(publicKey, tamperedPayload, envelope.lease_signature)).toBe(false);
  });
});

describe("signed lease routes", () => {
  beforeEach(() => vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS));
  afterEach(() => vi.restoreAllMocks());

  it("fails login closed when the signing secret is absent", async () => {
    const { env } = await testEnv({ signingSecret: false });
    const response = await fetchWorker(env, "/api/login", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Nwflash-Version": "1.4.0" },
      body: JSON.stringify({
        username: "alice",
        password: "correct horse",
        client_version: "1.4.0",
        build_id: "build-2026-08-23",
        process_nonce: "nonce-abc",
        session_id: "session-abc",
      }),
    });

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: "签名服务不可用。" });
  });

  it("returns a login lease bound to every request and account claim", async () => {
    const { env, publicKey } = await testEnv();
    const response = await fetchWorker(env, "/api/login", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Nwflash-Version": "1.4.0" },
      body: JSON.stringify({
        username: "alice",
        password: "correct horse",
        client_version: "1.4.0",
        build_id: "build-2026-08-23",
        process_nonce: "nonce-abc",
        session_id: "session-abc",
      }),
    });
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(body.token).toBe(TEST_TOKEN);
    expect(await verifyEnvelope(publicKey, String(body.lease_payload), String(body.lease_signature))).toBe(true);
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(String(body.lease_payload))))).toEqual({
      version: 1,
      kind: "login",
      username: "alice",
      token_sha256: "dJ_o7zu3up9yO1So8WAd_fjMF5j0FeXDSIjP7otxyBE",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      session_id: "session-abc",
      sequence: 1,
      issued_at: 1_787_444_800,
      expires_at: 1_787_444_920,
    });
  });

  it("returns a strictly larger signed heartbeat sequence", async () => {
    const { env, publicKey } = await testEnv();
    const response = await fetchWorker(env, "/api/heartbeat", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${TEST_TOKEN}`,
        "X-Nwflash-Version": "1.4.0",
      },
      body: JSON.stringify({
        sessionId: "session-heartbeat",
        clientVersion: "1.4.0",
        active: true,
        build_id: "build-2026-08-23",
        process_nonce: "nonce-abc",
        sequence: 41,
      }),
    });
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(await verifyEnvelope(publicKey, String(body.lease_payload), String(body.lease_signature))).toBe(true);
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(String(body.lease_payload))))).toMatchObject({
      kind: "heartbeat",
      username: "alice",
      session_id: "session-heartbeat",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      sequence: 42,
    });
  });

  it("keeps goodbye functional without a signing secret or new lease", async () => {
    const { env, db } = await testEnv({ signingSecret: false });
    db.sessions.set("session-goodbye", {
      user_id: 7,
      last_seen_at: Math.floor(FIXED_NOW_MS / 1000),
      force_exit_at: null,
      force_exit_reason: null,
    });
    const response = await fetchWorker(env, "/api/heartbeat", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${TEST_TOKEN}` },
      body: JSON.stringify({ sessionId: "session-goodbye", active: false }),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true, force_exit: false });
    expect(db.sessions.has("session-goodbye")).toBe(false);
  });

  it("returns a signed two-pin envelope for only the API host", async () => {
    const { env, publicKey } = await testEnv();
    const response = await fetchWorker(env, "/api/security/pins", { method: "GET" });
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(await verifyEnvelope(publicKey, String(body.pinset_payload), String(body.pinset_signature))).toBe(true);
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(String(body.pinset_payload))))).toEqual({
      version: 1,
      host: "api.nwflash.cc.cd",
      not_before: 1_787_444_740,
      expires_at: 1_788_049_600,
      primary_pin: "kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=",
      backup_pin: "kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4=",
    });
  });
});

describe("integrity telemetry route", () => {
  beforeEach(() => vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS));
  afterEach(() => vi.restoreAllMocks());

  it("rejects an oversized streaming body before JSON parsing or D1 writes", async () => {
    const { env, db } = await testEnv();
    const response = await fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.5" },
      body: `{"event_id":"${"x".repeat(5_000)}`,
    });

    expect(response.status).toBe(413);
    expect(db.integrityEvents.size).toBe(0);
    expect(db.rateLimits.size).toBe(0);
  });

  it.each(["token", "password", "path", "url", "serial", "raw_output"])(
    "rejects the forbidden or unknown %s field without storing it",
    async (field) => {
      const { env, db } = await testEnv();
      const response = await fetchWorker(env, "/api/integrity/report", {
        method: "POST",
        headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.5" },
        body: JSON.stringify({ ...telemetry(`event-${field}`), [field]: `secret-${field}` }),
      });

      expect(response.status).toBe(400);
      expect(db.integrityEvents.size).toBe(0);
    },
  );

  it("rejects phase and reason values outside the closed enums", async () => {
    const { env, db } = await testEnv();
    const badPhase = await fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.5" },
      body: JSON.stringify({ ...telemetry("event-bad-phase"), phase: "arbitrary" }),
    });
    const badReason = await fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.5" },
      body: JSON.stringify({ ...telemetry("event-bad-reason"), reason: "raw-command-output" }),
    });

    expect(badPhase.status).toBe(400);
    expect(badReason.status).toBe(400);
    expect(db.integrityEvents.size).toBe(0);
  });

  it("stores anonymous events as untrusted and authenticated events as user-bound", async () => {
    const { env, db } = await testEnv();
    const anonymous = await fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.5" },
      body: JSON.stringify(telemetry("event-anonymous")),
    });
    const authenticated = await fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "CF-Connecting-IP": "203.0.113.6",
        Authorization: `Bearer ${TEST_TOKEN}`,
      },
      body: JSON.stringify(telemetry("event-authenticated")),
    });

    expect(anonymous.status).toBe(202);
    expect(authenticated.status).toBe(202);
    expect(db.integrityEvents.get("event-anonymous")).toMatchObject({ userId: null, trusted: 0 });
    expect(db.integrityEvents.get("event-authenticated")).toMatchObject({ userId: 7, trusted: 1 });
  });

  it("limits one IP to twenty accepted events per sixty-second window", async () => {
    const { env, db } = await testEnv();
    const statuses: number[] = [];
    for (let index = 1; index <= 21; index += 1) {
      const response = await fetchWorker(env, "/api/integrity/report", {
        method: "POST",
        headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.25" },
        body: JSON.stringify(telemetry(`event-rate-${index}`)),
      });
      statuses.push(response.status);
    }

    expect(statuses.slice(0, 20)).toEqual(Array(20).fill(202));
    expect(statuses[20]).toBe(429);
    expect(db.integrityEvents.size).toBe(20);
  });

  it("treats a repeated event ID as an idempotent success without a second row", async () => {
    const { env, db } = await testEnv();
    const request = () => fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.30" },
      body: JSON.stringify(telemetry("event-idempotent")),
    });

    const first = await request();
    const second = await request();

    expect(first.status).toBe(202);
    expect(second.status).toBe(200);
    expect(await second.json()).toEqual({ ok: true, duplicate: true });
    expect(db.integrityEvents.size).toBe(1);
  });
});
