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

interface StoredSessionLease {
  session_id: string;
  user_id: number;
  username: string;
  client_version: string;
  build_id: string;
  process_nonce: string;
  sequence: number;
  created_at: number;
  updated_at: number;
}

class FakeD1Database {
  readonly users: TestUser[] = [];
  readonly sessions = new Map<string, {
    user_id: number;
    last_seen_at: number;
    force_exit_at: number | null;
    force_exit_reason: string | null;
  }>();
  readonly sessionLeases = new Map<string, StoredSessionLease>();
  readonly integrityEvents = new Map<string, StoredIntegrityEvent>();
  readonly rateLimits = new Map<string, number>();
  private integritySelectTarget = 0;
  private integritySelectCount = 0;
  private releaseIntegritySelects: (() => void) | null = null;
  private integritySelectGate: Promise<void> = Promise.resolve();

  prepare(query: string): FakeD1PreparedStatement {
    return new FakeD1PreparedStatement(this, query);
  }

  async batch(statements: FakeD1PreparedStatement[]): Promise<unknown[]> {
    return Promise.all(statements.map((statement) => statement.run()));
  }

  synchronizeIntegritySelects(count: number): void {
    this.integritySelectTarget = count;
    this.integritySelectCount = 0;
    this.integritySelectGate = new Promise((resolve) => {
      this.releaseIntegritySelects = resolve;
    });
  }

  async waitForIntegritySelectBarrier(): Promise<void> {
    if (this.integritySelectTarget === 0) return;
    this.integritySelectCount += 1;
    if (this.integritySelectCount === this.integritySelectTarget) this.releaseIntegritySelects?.();
    await this.integritySelectGate;
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
    if (sql.includes("from session_leases where session_id = ?")) {
      return (this.db.sessionLeases.get(String(this.values[0])) ?? null) as T | null;
    }
    if (sql.includes("from integrity_events where event_id = ?")) {
      const event = this.db.integrityEvents.get(String(this.values[0]));
      await this.db.waitForIntegritySelectBarrier();
      return (event ? { event_id: event.eventId } : null) as T | null;
    }
    if (sql.startsWith("insert into session_leases")) {
      const [sessionId, userId, username, clientVersion, buildId, processNonce, createdAt, updatedAt] = this.values;
      const key = String(sessionId);
      if (this.db.sessionLeases.has(key)) return null;
      const lease: StoredSessionLease = {
        session_id: key,
        user_id: Number(userId),
        username: String(username),
        client_version: String(clientVersion),
        build_id: String(buildId),
        process_nonce: String(processNonce),
        sequence: 1,
        created_at: Number(createdAt),
        updated_at: Number(updatedAt),
      };
      this.db.sessionLeases.set(key, lease);
      return { sequence: 1 } as T;
    }
    if (sql.startsWith("update session_leases")) {
      const [nextSequence, updatedAt, sessionId, userId, username, clientVersion, buildId, processNonce, previousSequence] = this.values;
      const lease = this.db.sessionLeases.get(String(sessionId));
      if (
        !lease
        || lease.user_id !== Number(userId)
        || lease.username !== String(username)
        || lease.client_version !== String(clientVersion)
        || lease.build_id !== String(buildId)
        || lease.process_nonce !== String(processNonce)
        || lease.sequence !== Number(previousSequence)
      ) return null;
      lease.sequence = Number(nextSequence);
      lease.updated_at = Number(updatedAt);
      return { sequence: lease.sequence } as T;
    }
    if (sql.startsWith("insert into integrity_events") && sql.includes("returning event_id")) {
      const [eventId, userId, trusted, phase, reason, clientVersion, buildId, occurredAt] = this.values;
      const key = String(eventId);
      if (this.db.integrityEvents.has(key)) return null;
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
      return { event_id: key } as T;
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
    if (sql.startsWith("delete from session_leases where session_id = ? and user_id = ?")) {
      const sessionId = String(this.values[0]);
      const current = this.db.sessionLeases.get(sessionId);
      if (current?.user_id === Number(this.values[1])) this.db.sessionLeases.delete(sessionId);
      return changed(current ? 1 : 0);
    }
    if (sql.startsWith("delete from session_leases")) return changed(0);
    if (sql.startsWith("delete from integrity_events where event_id = ?")) {
      return changed(this.db.integrityEvents.delete(String(this.values[0])) ? 1 : 0);
    }
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

function totalRateCount(db: FakeD1Database): number {
  return [...db.rateLimits.values()].reduce((total, count) => total + count, 0);
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

async function testEnv(options: { signingSecret?: boolean | string; token?: string } = {}): Promise<{
  env: Env;
  db: FakeD1Database;
  publicKey: CryptoKey;
  token: string;
  bobToken: string;
}> {
  const fixture = await ephemeralSigningFixture();
  const db = new FakeD1Database();
  const token = options.token ?? TEST_TOKEN;
  const bobToken = `${token}-bob`;
  const password = await passwordHash("correct horse", TEST_SALT);
  db.users.push({
    id: 7,
    username: "alice",
    name: "Alice",
    token,
    password,
    salt: TEST_SALT,
    enabled: 1,
    banned: 0,
  });
  db.users.push({
    id: 8,
    username: "bob",
    name: "Bob",
    token: bobToken,
    password,
    salt: TEST_SALT,
    enabled: 1,
    banned: 0,
  });
  const signingSecret = typeof options.signingSecret === "string"
    ? options.signingSecret
    : options.signingSecret === false
      ? undefined
      : fixture.secret;
  return {
    env: {
      DB: db as unknown as D1Database,
      VOTA_API_TOKEN: "unused-in-tests",
      SESSION_SIGNING_PRIVATE_KEY_PKCS8: signingSecret,
    },
    db,
    publicKey: fixture.publicKey,
    token,
    bobToken,
  };
}

async function fetchWorker(env: Env, path: string, init?: RequestInit): Promise<Response> {
  return worker.fetch(new Request(`https://api.nwflash.cc.cd${path}`, init), env);
}

function loginPayload(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    username: "alice",
    password: "correct horse",
    client_version: "1.4.0",
    build_id: "build-2026-08-23",
    process_nonce: "nonce-abc",
    session_id: "session-abc",
    ...overrides,
  };
}

async function postLogin(env: Env, overrides: Record<string, unknown> = {}): Promise<Response> {
  return fetchWorker(env, "/api/login", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Nwflash-Version": "1.4.0" },
    body: JSON.stringify(loginPayload(overrides)),
  });
}

async function postHeartbeat(
  env: Env,
  token: string,
  overrides: Record<string, unknown> = {},
): Promise<Response> {
  return fetchWorker(env, "/api/heartbeat", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
      "X-Nwflash-Version": "1.4.0",
    },
    body: JSON.stringify({
      sessionId: "session-abc",
      clientVersion: "1.4.0",
      active: true,
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      sequence: 1,
      ...overrides,
    }),
  });
}

function decodeLeaseClaims(body: Record<string, unknown>): Record<string, unknown> {
  return JSON.parse(new TextDecoder().decode(decodeBase64Url(String(body.lease_payload)))) as Record<string, unknown>;
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

  it("persists login sequence one and advances the complete binding to heartbeat sequence two", async () => {
    const { env, db, publicKey, token } = await testEnv({ token: "continuity-token" });
    const login = await postLogin(env);
    const loginBody = await login.json() as Record<string, unknown>;
    const loginClaims = decodeLeaseClaims(loginBody);

    expect(login.status).toBe(200);
    expect(loginClaims.sequence).toBe(1);
    expect(db.sessionLeases.get("session-abc")).toMatchObject({
      user_id: 7,
      username: "alice",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      sequence: 1,
    });

    const response = await postHeartbeat(env, token, { sequence: loginClaims.sequence });
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(await verifyEnvelope(publicKey, String(body.lease_payload), String(body.lease_signature))).toBe(true);
    expect(decodeLeaseClaims(body)).toMatchObject({
      kind: "heartbeat",
      username: "alice",
      session_id: "session-abc",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-abc",
      sequence: 2,
    });
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(2);
  });

  it("rejects an unknown session without returning a signed lease", async () => {
    const { env, token } = await testEnv({ token: "unknown-session-token" });

    const response = await postHeartbeat(env, token);
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(409);
    expect(body).not.toHaveProperty("lease_payload");
    expect(body).not.toHaveProperty("lease_signature");
  });

  it("rejects a rate-limited heartbeat without signing or advancing the session", async () => {
    const { env, db, token } = await testEnv({ token: "rate-limited-session-token" });
    expect((await postLogin(env)).status).toBe(200);
    expect((await postHeartbeat(env, token)).status).toBe(200);

    const response = await postHeartbeat(env, token, { sequence: 2 });
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(429);
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(2);
  });

  it("rejects replay of an already accepted heartbeat sequence", async () => {
    const { env, db, token } = await testEnv({ token: "replay-session-token" });
    expect((await postLogin(env)).status).toBe(200);
    expect((await postHeartbeat(env, token)).status).toBe(200);
    vi.mocked(Date.now).mockReturnValue(FIXED_NOW_MS + 4_000);

    const replay = await postHeartbeat(env, token, { sequence: 1 });
    const body = await replay.json() as Record<string, unknown>;

    expect(replay.status).toBe(409);
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(2);
  });

  it("rejects sequence rollback after multiple accepted heartbeats", async () => {
    const { env, db, token } = await testEnv({ token: "rollback-session-token" });
    expect((await postLogin(env)).status).toBe(200);
    expect((await postHeartbeat(env, token)).status).toBe(200);
    vi.mocked(Date.now).mockReturnValue(FIXED_NOW_MS + 4_000);
    expect((await postHeartbeat(env, token, { sequence: 2 })).status).toBe(200);
    vi.mocked(Date.now).mockReturnValue(FIXED_NOW_MS + 8_000);

    const rollback = await postHeartbeat(env, token, { sequence: 1 });

    expect(rollback.status).toBe(409);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(3);
  });

  it("rejects a caller-selected sequence jump", async () => {
    const { env, db, token } = await testEnv({ token: "jump-session-token" });
    expect((await postLogin(env)).status).toBe(200);

    const jump = await postHeartbeat(env, token, { sequence: 41 });
    const body = await jump.json() as Record<string, unknown>;

    expect(jump.status).toBe(409);
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("rejects build mutation without advancing the persisted sequence", async () => {
    const { env, db, token } = await testEnv({ token: "build-mutation-token" });
    expect((await postLogin(env)).status).toBe(200);

    const response = await postHeartbeat(env, token, { build_id: "mutated-build" });

    expect(response.status).toBe(409);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("rejects client version mutation without advancing the persisted sequence", async () => {
    const { env, db, token } = await testEnv({ token: "version-mutation-token" });
    expect((await postLogin(env)).status).toBe(200);

    const response = await postHeartbeat(env, token, { clientVersion: "1.4.1" });

    expect(response.status).toBe(409);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("rejects process nonce mutation without advancing the persisted sequence", async () => {
    const { env, db, token } = await testEnv({ token: "nonce-mutation-token" });
    expect((await postLogin(env)).status).toBe(200);

    const response = await postHeartbeat(env, token, { process_nonce: "mutated-nonce" });

    expect(response.status).toBe(409);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("rejects a login session collision without returning another token or lease", async () => {
    const { env, db } = await testEnv({ token: "collision-owner-token" });
    expect((await postLogin(env)).status).toBe(200);

    const collision = await postLogin(env, { username: "bob" });
    const body = await collision.json() as Record<string, unknown>;

    expect(collision.status).toBe(409);
    expect(body).not.toHaveProperty("token");
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.user_id).toBe(7);
  });

  it("rejects cross-user heartbeat ownership without returning a signed lease", async () => {
    const { env, db, bobToken } = await testEnv({ token: "cross-user-owner-token" });
    expect((await postLogin(env)).status).toBe(200);

    const response = await postHeartbeat(env, bobToken);
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(409);
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.user_id).toBe(7);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("allows only one of two concurrent same-sequence heartbeats to win the CAS", async () => {
    const { env, db, token } = await testEnv({ token: "concurrent-sequence-token" });
    expect((await postLogin(env)).status).toBe(200);

    const responses = await Promise.all([
      postHeartbeat(env, token),
      postHeartbeat(env, token),
    ]);
    const statuses = responses.map((response) => response.status).sort((left, right) => left - right);
    const bodies = await Promise.all(responses.map((response) => response.json() as Promise<Record<string, unknown>>));

    expect(statuses).toEqual([200, 409]);
    expect(bodies.filter((body) => "lease_payload" in body)).toHaveLength(1);
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(2);
  });

  it("preserves request, account, password, banned, and disabled failures during a signing outage", async () => {
    const malformed = await testEnv({ signingSecret: false });
    const unknown = await testEnv({ signingSecret: false });
    const wrongPassword = await testEnv({ signingSecret: false });
    const banned = await testEnv({ signingSecret: false });
    const disabled = await testEnv({ signingSecret: false });
    banned.db.users[0].banned = 1;
    disabled.db.users[0].enabled = 0;

    const responses = await Promise.all([
      postLogin(malformed.env, { username: "" }),
      postLogin(unknown.env, { username: "nobody" }),
      postLogin(wrongPassword.env, { password: "wrong password" }),
      postLogin(banned.env),
      postLogin(disabled.env),
    ]);

    expect(responses.map((response) => response.status)).toEqual([400, 401, 401, 401, 401]);
    expect([
      malformed.db.sessionLeases.size,
      unknown.db.sessionLeases.size,
      wrongPassword.db.sessionLeases.size,
      banned.db.sessionLeases.size,
      disabled.db.sessionLeases.size,
    ]).toEqual([0, 0, 0, 0, 0]);
  });

  it("fails malformed signing key closed after credential verification and before session persistence", async () => {
    const { env, db } = await testEnv({ signingSecret: "not-base64url!" });

    const response = await postLogin(env);

    expect(response.status).toBe(503);
    expect(db.sessionLeases.size).toBe(0);
  });

  it("does not advance heartbeat state when signing the candidate fails", async () => {
    const { env, db, token } = await testEnv({ token: "heartbeat-signing-failure-token" });
    expect((await postLogin(env)).status).toBe(200);
    env.SESSION_SIGNING_PRIVATE_KEY_PKCS8 = "not-base64url!";

    const response = await postHeartbeat(env, token);
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(503);
    expect(body).not.toHaveProperty("lease_payload");
    expect(db.sessionLeases.get("session-abc")?.sequence).toBe(1);
  });

  it("keeps goodbye functional without a signing secret or new lease", async () => {
    const { env, db } = await testEnv({ signingSecret: false });
    db.sessions.set("session-goodbye", {
      user_id: 7,
      last_seen_at: Math.floor(FIXED_NOW_MS / 1000),
      force_exit_at: null,
      force_exit_reason: null,
    });
    db.sessionLeases.set("session-goodbye", {
      session_id: "session-goodbye",
      user_id: 7,
      username: "alice",
      client_version: "1.4.0",
      build_id: "build-2026-08-23",
      process_nonce: "nonce-goodbye",
      sequence: 4,
      created_at: Math.floor(FIXED_NOW_MS / 1000),
      updated_at: Math.floor(FIXED_NOW_MS / 1000),
    });
    const response = await fetchWorker(env, "/api/heartbeat", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${TEST_TOKEN}` },
      body: JSON.stringify({ sessionId: "session-goodbye", active: false }),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true, force_exit: false });
    expect(db.sessions.has("session-goodbye")).toBe(false);
    expect(db.sessionLeases.has("session-goodbye")).toBe(false);
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
    expect(totalRateCount(db)).toBe(1);
  });

  it("atomically claims a concurrent duplicate event so exactly one request consumes quota", async () => {
    const { env, db } = await testEnv();
    const concurrency = 8;
    db.synchronizeIntegritySelects(concurrency);
    const request = () => fetchWorker(env, "/api/integrity/report", {
      method: "POST",
      headers: { "Content-Type": "application/json", "CF-Connecting-IP": "203.0.113.31" },
      body: JSON.stringify(telemetry("event-concurrent-idempotent")),
    });

    const responses = await Promise.all(Array.from({ length: concurrency }, request));
    const statuses = responses.map((response) => response.status);

    expect(statuses.filter((status) => status === 202)).toHaveLength(1);
    expect(statuses.filter((status) => status === 200)).toHaveLength(concurrency - 1);
    expect(db.integrityEvents.size).toBe(1);
    expect(totalRateCount(db)).toBe(1);
  });
});
