import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import userWorker, { type Env as WorkerEnv } from "../src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const PASSWORD = "correct horse";
const NEW_PASSWORD = "new correct horse";
const SALT = "00112233445566778899aabbccddeeff";
const ALICE_TOKEN = "a".repeat(64);
const BOB_TOKEN = "b".repeat(64);
const FIXED_NOW_MS = 1_787_544_000_000;
const CSP = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

beforeEach(async () => {
  vi.spyOn(Date, "now").mockReturnValue(FIXED_NOW_MS);
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

afterEach(() => vi.restoreAllMocks());

describe("personal ops Worker with real Workerd D1", () => {
  it("sets an HttpOnly cookie without returning the token on login", async () => {
    await seedUser();

    const response = await postLogin("alice", PASSWORD, false);
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    const setCookie = response.headers.get("set-cookie") ?? "";
    expect(setCookie).toContain("__Host-nwflash_user=");
    expect(setCookie).toContain("; Path=/");
    expect(setCookie).toContain("; Secure");
    expect(setCookie).toContain("; HttpOnly");
    expect(setCookie).toContain("; SameSite=Strict");
    expect(setCookie).not.toContain("Domain=");
    expect(setCookie).not.toContain("Max-Age");
    expect(body).toMatchObject({ ok: true, username: "alice", name: "Alice" });
    expect(body).not.toHaveProperty("token");
  });

  it("requires the requested-with header before processing login", async () => {
    await seedUser();

    const response = await request("/api/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ username: "alice", password: PASSWORD, remember: false }),
    });

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ message: "请求缺少必要请求头。" });
    expect(await scalar("SELECT COUNT(*) AS value FROM login_attempts")).toBe(0);
  });

  it("uses exact distinct rate keys, a coarse IP ceiling, and removes stale windows", async () => {
    await seedUser();
    await env.DB.prepare(
      "INSERT INTO login_attempts (k, window_start, count) VALUES ('stale-hash', ?, 1)",
    ).bind(Math.floor(FIXED_NOW_MS / 1000) - 7_200).run();

    const response = await postLogin("alice", PASSWORD, true, "203.0.113.45");
    const rows = await env.DB.prepare(
      "SELECT k, count FROM login_attempts ORDER BY k",
    ).all<{ k: string; count: number }>();

    expect(response.headers.get("set-cookie")).toContain("Max-Age=2592000");
    expect(rows.results).toEqual([
      { k: "59a5a6b7d26ccec4f492b4e9b87a0830472f793d6deb6e9acd27ac6a445dd3b2", count: 1 },
      { k: "faa01814b0208fc8db3d8040fb093c7bea121545071d072953792dbe7e26331b", count: 1 },
    ]);
    expect(await scalar("SELECT COUNT(*) AS value FROM login_attempts WHERE k = 'stale-hash'")).toBe(0);

    await env.DB.prepare(
      "UPDATE login_attempts SET count = 24 WHERE k = ?",
    ).bind("59a5a6b7d26ccec4f492b4e9b87a0830472f793d6deb6e9acd27ac6a445dd3b2").run();
    const limited = await postLogin("another-name", "wrong password", false, "203.0.113.45");
    expect(limited.status).toBe(429);
  });

  it("performs exactly one PBKDF2 for every credential outcome", async () => {
    await seedUser();
    await seedUser({ id: 8, username: "disabled", name: "Disabled", token: "8".repeat(64) });
    await seedUser({ id: 9, username: "banned", name: "Banned", token: "9".repeat(64) });
    await seedUser({ id: 10, username: "passwordless", name: "Passwordless", token: "c".repeat(64) });
    await env.DB.batch([
      env.DB.prepare("UPDATE api_users SET enabled = 0 WHERE id = 8"),
      env.DB.prepare("UPDATE api_users SET banned = 1 WHERE id = 9"),
      env.DB.prepare("UPDATE api_users SET password = NULL, salt = NULL WHERE id = 10"),
    ]);
    const deriveBits = vi.spyOn(crypto.subtle, "deriveBits");

    const responses = await Promise.all([
      postLogin("missing", "wrong password", false),
      postLogin("disabled", "wrong password", false),
      postLogin("banned", "wrong password", false),
      postLogin("passwordless", "wrong password", false),
      postLogin("alice", "wrong password", false),
    ]);

    expect(responses.map((response) => response.status)).toEqual([401, 401, 401, 401, 401]);
    expect(deriveBits).toHaveBeenCalledTimes(5);
  });

  it("authenticates only the portal cookie and expires invalid or revoked cookies", async () => {
    await seedUser();
    await seedUser({ id: 8, username: "revoked", name: "Revoked", token: `revoked:${"c".repeat(64)}` });

    const bearer = await request("/api/me", { headers: { Authorization: `Bearer ${ALICE_TOKEN}` } });
    const invalid = await request("/api/me", { headers: { Cookie: cookie("not-a-token") } });
    const revoked = await request("/api/me", { headers: { Cookie: cookie(`revoked:${"c".repeat(64)}`) } });

    expect(bearer.status).toBe(401);
    expect(invalid.status).toBe(401);
    expect(invalid.headers.get("set-cookie")).toContain("Max-Age=0");
    expect(revoked.status).toBe(401);
    expect(revoked.headers.get("set-cookie")).toContain("Max-Age=0");
  });

  it("expires the cookie on logout without revoking the shared desktop credential", async () => {
    await seedUser();

    const response = await request("/api/logout", writeOptions(cookie(ALICE_TOKEN), {}));

    expect(response.status).toBe(200);
    expect(response.headers.get("set-cookie")).toContain("Max-Age=0");
    expect(await storedString("SELECT token AS value FROM api_users WHERE id = 7")).toBe(ALICE_TOKEN);
  });

  it("revokes the old token and removes every session in one password change", async () => {
    const oldToken = await seedUserAndSessions();

    const response = await changePassword(cookie(oldToken), PASSWORD, NEW_PASSWORD);

    expect(response.status).toBe(200);
    expect(await scalar("SELECT COUNT(*) AS value FROM api_users WHERE token = ?", oldToken)).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM session_leases WHERE user_id = 7")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM online_sessions WHERE user_id = 7")).toBe(0);
    expect(await getMe(cookie(oldToken))).toHaveProperty("status", 401);
    expect(await response.json()).toEqual({ ok: true, reauthenticate: true });
    expect(response.headers.get("set-cookie")).toContain("Max-Age=0");
    expect(await storedString("SELECT token AS value FROM api_users WHERE id = 7")).toMatch(/^revoked:[0-9a-f]{64}$/);
  });

  it("issues a fresh token only after reauthentication of a revoked account", async () => {
    const oldToken = await seedRevokedUser();

    const rejected = await postLogin("alice", "wrong password", false);
    expect(rejected.status).toBe(401);
    expect(await storedToken()).toBe(oldToken);

    const accepted = await postLogin("alice", NEW_PASSWORD, false);
    expect(accepted.status).toBe(200);
    expect(await storedToken()).toMatch(/^[0-9a-f]{64}$/);
    expect(await storedToken()).not.toBe(oldToken);
    expect(await accepted.json()).not.toHaveProperty("token");
  });

  it("lets concurrent revoked-token logins converge on one winning active token", async () => {
    await seedRevokedUser();

    const responses = await Promise.all([
      postLogin("alice", NEW_PASSWORD, false),
      postLogin("alice", NEW_PASSWORD, false),
    ]);
    const activeToken = await storedToken();

    expect(responses.map((response) => response.status)).toEqual([200, 200]);
    expect(activeToken).toMatch(/^[0-9a-f]{64}$/);
    expect(responses.map(cookieToken)).toEqual([activeToken, activeToken]);
    for (const response of responses) expect(await response.json()).not.toHaveProperty("token");
  });

  it("re-reads the winning active token after a deterministic revoked-login CAS loss", async () => {
    await seedRevokedUser();
    const winningToken = "e".repeat(64);
    interceptNextPbkdf2(async () => {
      await env.DB.prepare("UPDATE api_users SET token = ? WHERE id = 7").bind(winningToken).run();
    });

    const response = await postLogin("alice", NEW_PASSWORD, false);

    expect(response.status).toBe(200);
    expect(cookieToken(response)).toBe(winningToken);
    expect(await storedToken()).toBe(winningToken);
  });

  it("returns one conflict when concurrent password updates race the token CAS", async () => {
    await seedUserAndSessions();

    const responses = await Promise.all([
      changePassword(cookie(ALICE_TOKEN), PASSWORD, NEW_PASSWORD),
      changePassword(cookie(ALICE_TOKEN), PASSWORD, "another safe password"),
    ]);

    expect(responses.map((response) => response.status).sort()).toEqual([200, 409]);
    expect(await storedToken()).toMatch(/^revoked:[0-9a-f]{64}$/);
    expect(await scalar("SELECT COUNT(*) AS value FROM session_leases WHERE user_id = 7")).toBe(0);
    expect(await scalar("SELECT COUNT(*) AS value FROM online_sessions WHERE user_id = 7")).toBe(0);
  });

  it("returns a conflict after a deterministic password-update batch CAS loss", async () => {
    await seedUserAndSessions();
    const winningToken = "f".repeat(64);
    interceptNextPbkdf2(async () => {
      await env.DB.prepare("UPDATE api_users SET token = ? WHERE id = 7").bind(winningToken).run();
    });

    const response = await changePassword(cookie(ALICE_TOKEN), PASSWORD, NEW_PASSWORD);

    expect(response.status).toBe(409);
    expect(await storedToken()).toBe(winningToken);
    expect(await scalar("SELECT COUNT(*) AS value FROM session_leases WHERE user_id = 7")).toBe(2);
    expect(await scalar("SELECT COUNT(*) AS value FROM online_sessions WHERE user_id = 7")).toBe(2);
  });

  it("computes overview counts only from the authenticated user's last seven days", async () => {
    await seedUser();
    await seedUser({ id: 8, username: "bob", name: "Bob", token: BOB_TOKEN });
    const now = Math.floor(FIXED_NOW_MS / 1000);
    const recent = now - (6 * 24 * 60 * 60);
    const old = now - (8 * 24 * 60 * 60);
    await env.DB.batch([
      usage(7, "Flashing", "success", recent),
      usage(7, "Rebooting", "failed", now - 1),
      usage(7, "Installing", "success", old),
      usage(8, "Installing", "success", recent),
      access(7, "PD-A", "1.0", 200, "https://safe.invalid/alice", sqliteDate(recent)),
      access(7, "PD-OLD", "0.9", 404, "https://safe.invalid/old", sqliteDate(old)),
      access(8, "PD-B", "1.0", 404, "https://safe.invalid/bob", sqliteDate(recent)),
      online(7, "alice-live", "203.0.113.1", now - 10),
      online(8, "bob-live", "203.0.113.2", now - 10),
    ]);

    const response = await request("/api/me/overview", { headers: { Cookie: cookie(ALICE_TOKEN) } });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      total: 3,
      operations: 2,
      rom: 1,
      successes: 2,
      failures: 1,
      activeSessions: 1,
    });
  });

  it("validates activity filters and clamps pagination over the sanitized union", async () => {
    await seedUser();
    await env.DB.batch([
      usage(7, "Flashing", "success", 100),
      usage(7, "Rebooting", "canceled", 200),
      access(7, "PD-A", "1.0", 200, "https://safe.invalid/a", "1970-01-01 00:05:00"),
    ]);

    const filtered = await request("/api/me/activities?type=operation&status=success&limit=999&offset=-8", { headers: { Cookie: cookie(ALICE_TOKEN) } });
    const invalidType = await request("/api/me/activities?type=anything", { headers: { Cookie: cookie(ALICE_TOKEN) } });
    const invalidStatus = await request("/api/me/activities?status=started", { headers: { Cookie: cookie(ALICE_TOKEN) } });

    expect(filtered.status).toBe(200);
    expect(await filtered.json()).toMatchObject({
      count: 1,
      limit: 100,
      offset: 0,
      activities: [{ type: "operation", status: "success", summary: "刷写操作" }],
    });
    expect(invalidType.status).toBe(400);
    expect(invalidStatus.status).toBe(400);
  });

  it("returns the same 404 for foreign and missing activity details", async () => {
    await seedUser();
    await seedUser({ id: 8, username: "bob", name: "Bob", token: BOB_TOKEN });
    await env.DB.prepare(
      "INSERT INTO usage_logs (id, api_user_id, operation_kind, status, started_at) VALUES (88, 8, 'Flashing', 'success', 100)",
    ).run();

    const foreign = await getActivity(cookie(ALICE_TOKEN), "operation:88");
    const missing = await getActivity(cookie(ALICE_TOKEN), "operation:999999");

    expect(foreign.status).toBe(404);
    expect(await foreign.json()).toEqual(await missing.json());
  });

  it("rejects malformed activity identifiers before querying a detail", async () => {
    await seedUser();

    for (const id of ["7", "operation:", "operation:-1", "operation:1.5", "other:1"]) {
      expect((await getActivity(cookie(ALICE_TOKEN), id)).status, id).toBe(400);
    }
  });

  it("never serializes raw activity titles or ROM URLs", async () => {
    await seedUser();
    await seedUnsafeActivity({
      title: "flash vendor_boot_a C:/secret/image.img",
      url: "https://example.test/file.zip?sign=secret",
    });

    const response = await getActivities(cookie(ALICE_TOKEN));
    const bodyText = await response.text();

    expect(response.status).toBe(200);
    expect(bodyText).not.toContain("vendor_boot_a");
    expect(bodyText).not.toContain("C:/secret");
    expect(bodyText).not.toContain("sign=secret");
  });

  it("returns the exact unavailable-step contract without fabricating operations", async () => {
    await seedUser();
    await env.DB.prepare(
      "INSERT INTO usage_logs (id, api_user_id, operation_kind, title, status, started_at) VALUES (7, 7, 'Flashing', 'private title', 'success', 100)",
    ).run();

    const response = await getActivity(cookie(ALICE_TOKEN), "operation:7");
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(body).toMatchObject({
      id: "operation:7",
      summary: "刷写操作",
      steps_state: "unavailable",
      steps: [],
      steps_message: "无更详细数据",
    });
    expect(JSON.stringify(body)).not.toContain("private title");
  });

  it("uses the fixed fallback label for prototype-named operation kinds", async () => {
    await seedUser();
    await env.DB.prepare(
      "INSERT INTO usage_logs (id, api_user_id, operation_kind, status, started_at) VALUES (8, 7, '__proto__', 'success', 100)",
    ).run();

    const response = await getActivity(cookie(ALICE_TOKEN), "operation:8");

    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ summary: "工具操作" });
  });

  it("returns owned ROM detail metadata without its URL", async () => {
    await seedUser();
    await env.DB.prepare(
      "INSERT INTO access_logs (id, api_user_id, pd, version, url, status, created_at) VALUES (9, 7, 'PD-9', '2.0', 'https://example.test/rom.zip?secret=1', 200, '2026-08-24 00:00:00')",
    ).run();

    const response = await getActivity(cookie(ALICE_TOKEN), "rom:9");
    const body = await response.json() as Record<string, unknown>;

    expect(response.status).toBe(200);
    expect(body).toMatchObject({ id: "rom:9", type: "rom", pd: "PD-9", version: "2.0", http_status: 200 });
    expect(body).not.toHaveProperty("url");
    expect(JSON.stringify(body)).not.toContain("secret=1");
  });

  it("masks IPv4 and IPv6 session addresses and hides invalid values", async () => {
    await seedUser();
    const now = Math.floor(FIXED_NOW_MS / 1000);
    await env.DB.batch([
      online(7, "ipv4", "203.0.113.92", now - 3),
      online(7, "ipv6", "2001:0db8:85a3::8a2e:0370:7334", now - 2),
      online(7, "invalid", "device-name", now - 1),
    ]);

    const response = await request("/api/me/sessions", { headers: { Cookie: cookie(ALICE_TOKEN) } });
    const body = await response.json() as { sessions: Array<Record<string, unknown>> };

    expect(body.sessions.map((session) => session.ip_masked)).toEqual(["已隐藏", "2001:0db8:…", "203.0.113.••"]);
    for (const session of body.sessions) expect(session).not.toHaveProperty("ip");
  });

  it("returns one ownership-safe 404 for foreign and missing session kicks", async () => {
    await seedUser();
    await seedUser({ id: 8, username: "bob", name: "Bob", token: BOB_TOKEN });
    const now = Math.floor(FIXED_NOW_MS / 1000);
    await online(8, "bob-session", "203.0.113.8", now).run();

    const foreign = await kickSession(cookie(ALICE_TOKEN), "bob-session");
    const missing = await kickSession(cookie(ALICE_TOKEN), "missing-session");

    expect(foreign.status).toBe(404);
    expect(await foreign.json()).toEqual(await missing.json());
    expect(await scalar("SELECT COUNT(*) AS value FROM online_sessions WHERE session_id = 'bob-session' AND force_exit_at IS NULL")).toBe(1);
  });

  it("marks an owned kick pending while the session remains online", async () => {
    await seedUser();
    const now = Math.floor(FIXED_NOW_MS / 1000);
    await online(7, "alice-session", "203.0.113.7", now).run();

    const kicked = await kickSession(cookie(ALICE_TOKEN), "alice-session");
    const sessions = await request("/api/me/sessions", { headers: { Cookie: cookie(ALICE_TOKEN) } });
    const body = await sessions.json() as { sessions: Array<Record<string, unknown>> };

    expect(kicked.status).toBe(200);
    expect(body.sessions).toHaveLength(1);
    expect(body.sessions[0]).toEqual({
      id: "alice-session",
      clientVersion: "1.4.0",
      ip_masked: "203.0.113.••",
      connectedAt: new Date((now - 60) * 1000).toISOString(),
      lastSeenAt: new Date(now * 1000).toISOString(),
      duration: "1 分钟",
      pendingExit: true,
    });
    expect(JSON.stringify(body)).not.toContain("用户在本门户强制下线");
  });

  it("accepts a 128-character password", async () => {
    await seedUser();

    const response = await changePassword(cookie(ALICE_TOKEN), PASSWORD, "x".repeat(128));

    expect(response.status).toBe(200);
    expect(await storedToken()).toMatch(/^revoked:[0-9a-f]{64}$/);
  });

  it("rejects a 129-character password without changing credentials", async () => {
    await seedUser();

    const response = await changePassword(cookie(ALICE_TOKEN), PASSWORD, "x".repeat(129));

    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ message: "新密码不能超过 128 位。" });
    expect(await storedToken()).toBe(ALICE_TOKEN);
  });

  it("serves strict-CSP portal assets with their exact content types", async () => {
    const page = await request("/");
    const css = await request("/portal/styles.css");
    const script = await request("/portal/app.js");

    expect(page.status).toBe(200);
    expect(page.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(page.headers.get("content-security-policy")).toBe(CSP);
    expect(await page.text()).toContain("PERSONAL OPS");
    expect(css.headers.get("content-type")).toBe("text/css; charset=utf-8");
    expect(await css.text()).toContain(".portal-shell");
    expect(script.headers.get("content-type")).toBe("text/javascript; charset=utf-8");
    expect(await script.text()).toContain("createPortal");
  });
});

interface SeedUserOptions {
  id?: number;
  username?: string;
  name?: string;
  token?: string;
  password?: string;
  salt?: string;
}

async function seedUser(options: SeedUserOptions = {}): Promise<void> {
  const id = options.id ?? 7;
  const username = options.username ?? "alice";
  const name = options.name ?? "Alice";
  const token = options.token ?? ALICE_TOKEN;
  const password = options.password ?? PASSWORD;
  const salt = options.salt ?? SALT;
  await env.DB.prepare(
    `INSERT INTO api_users (id, username, name, token, password, salt, enabled, banned)
     VALUES (?, ?, ?, ?, ?, ?, 1, 0)`,
  ).bind(id, username, name, token, await passwordHash(password, salt), salt).run();
}

async function seedUserAndSessions(): Promise<string> {
  await seedUser();
  const now = Math.floor(FIXED_NOW_MS / 1000);
  await env.DB.batch([
    online(7, "one", "203.0.113.1", now),
    online(7, "two", "2001:db8::1", now),
    env.DB.prepare(
      `INSERT INTO session_leases
       (session_id, user_id, username, client_version, build_id, process_nonce, sequence, created_at, updated_at)
       VALUES ('one', 7, 'alice', '1.4.0', 'build', 'nonce-1', 1, ?, ?)`,
    ).bind(now, now),
    env.DB.prepare(
      `INSERT INTO session_leases
       (session_id, user_id, username, client_version, build_id, process_nonce, sequence, created_at, updated_at)
       VALUES ('two', 7, 'alice', '1.4.0', 'build', 'nonce-2', 1, ?, ?)`,
    ).bind(now, now),
  ]);
  return ALICE_TOKEN;
}

async function seedRevokedUser(): Promise<string> {
  const marker = `revoked:${"d".repeat(64)}`;
  await seedUser({ token: marker, password: NEW_PASSWORD });
  return marker;
}

async function seedUnsafeActivity(input: { title: string; url: string }): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO usage_logs (api_user_id, operation_kind, title, status, started_at) VALUES (7, 'Flashing', ?, 'success', 100)",
    ).bind(input.title),
    env.DB.prepare(
      "INSERT INTO access_logs (api_user_id, pd, version, url, status, created_at) VALUES (7, 'PD-X', '1.0', ?, 200, '2026-08-24 00:00:00')",
    ).bind(input.url),
  ]);
}

function usage(userId: number, kind: string, status: string, startedAt: number): D1PreparedStatement {
  return env.DB.prepare(
    "INSERT INTO usage_logs (api_user_id, operation_kind, status, started_at) VALUES (?, ?, ?, ?)",
  ).bind(userId, kind, status, startedAt);
}

function access(userId: number, pd: string, version: string, status: number, url: string, createdAt = "2026-08-24 00:00:00"): D1PreparedStatement {
  return env.DB.prepare(
    "INSERT INTO access_logs (api_user_id, pd, version, status, url, created_at) VALUES (?, ?, ?, ?, ?, ?)",
  ).bind(userId, pd, version, status, url, createdAt);
}

function sqliteDate(epochSeconds: number): string {
  return new Date(epochSeconds * 1000).toISOString().slice(0, 19).replace("T", " ");
}

function online(userId: number, sessionId: string, ip: string, lastSeenAt: number): D1PreparedStatement {
  return env.DB.prepare(
    `INSERT INTO online_sessions
     (session_id, user_id, user_name, client_version, ip, connected_at, last_seen_at)
     VALUES (?, ?, ?, '1.4.0', ?, ?, ?)`,
  ).bind(sessionId, userId, userId === 7 ? "Alice" : "Bob", ip, lastSeenAt - 60, lastSeenAt);
}

async function postLogin(username: string, password: string, remember: boolean, ip = "198.51.100.10"): Promise<Response> {
  return request("/api/login", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "CF-Connecting-IP": ip,
      "X-Requested-With": "XMLHttpRequest",
    },
    body: JSON.stringify({ username, password, remember }),
  });
}

async function changePassword(cookieHeader: string, current: string, newPassword: string): Promise<Response> {
  return request("/api/me/password", writeOptions(cookieHeader, { current, newPassword }));
}

async function getMe(cookieHeader: string): Promise<Response> {
  return request("/api/me", { headers: { Cookie: cookieHeader } });
}

async function getActivities(cookieHeader: string): Promise<Response> {
  return request("/api/me/activities", { headers: { Cookie: cookieHeader } });
}

async function getActivity(cookieHeader: string, id: string): Promise<Response> {
  return request(`/api/me/activities/${encodeURIComponent(id)}`, { headers: { Cookie: cookieHeader } });
}

async function kickSession(cookieHeader: string, id: string): Promise<Response> {
  return request("/api/me/sessions/kick", writeOptions(cookieHeader, { id }));
}

function writeOptions(cookieHeader: string, body: unknown): RequestInit {
  return {
    method: "POST",
    headers: {
      Cookie: cookieHeader,
      "Content-Type": "application/json",
      "X-Requested-With": "XMLHttpRequest",
    },
    body: JSON.stringify(body),
  };
}

function request(path: string, init: RequestInit = {}): Promise<Response> {
  return userWorker.fetch(new Request(`https://user.nwflash.cc.cd${path}`, init), env);
}

function cookie(token: string): string {
  return `__Host-nwflash_user=${token}`;
}

function cookieToken(response: Response): string {
  return response.headers.get("set-cookie")?.match(/__Host-nwflash_user=([^;]+)/)?.[1] ?? "";
}

async function storedToken(): Promise<string> {
  return storedString("SELECT token AS value FROM api_users WHERE id = 7");
}

async function storedString(query: string, ...bindings: unknown[]): Promise<string> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: string }>();
  return String(row?.value ?? "");
}

async function scalar(query: string, ...bindings: unknown[]): Promise<number> {
  const row = await env.DB.prepare(query).bind(...bindings).first<{ value: number }>();
  return Number(row?.value ?? 0);
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

function interceptNextPbkdf2(action: () => Promise<void>): void {
  const deriveBits = crypto.subtle.deriveBits.bind(crypto.subtle);
  let intercepted = false;
  vi.spyOn(crypto.subtle, "deriveBits").mockImplementation(async (algorithm, baseKey, length) => {
    if (!intercepted) {
      intercepted = true;
      await action();
    }
    return deriveBits(algorithm, baseKey, length);
  });
}
