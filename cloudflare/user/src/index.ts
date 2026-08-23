/// <reference path="./assets.d.ts" />

import portalScript from "./portal/app.client.js";
import portalHtml from "./portal/index.html";
import portalStyles from "./portal/styles.css";

export interface Env {
  DB: D1Database;
  ONLINE_TIMEOUT_MS?: string;
}

const PBKDF2_ITERATIONS = 100_000;
const LOGIN_WINDOW_SEC = 60;
const LOGIN_MAX_ATTEMPTS = 8;
const USER_COOKIE = "__Host-nwflash_user";
const REVOKED_TOKEN_PREFIX = "revoked:";
const REMEMBER_MAX_AGE_SECONDS = 30 * 24 * 60 * 60;
const ACTIVITY_NOT_FOUND = { message: "Not Found" };

const SECURE_HEADERS: Record<string, string> = {
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Referrer-Policy": "no-referrer",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  "Content-Security-Policy":
    "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
  "Cache-Control": "no-store",
};

const SAFE_OPERATION_LABELS = new Map<string, string>([
  ["Flashing", "刷写操作"],
  ["Rebooting", "重启操作"],
  ["Transferring", "传输操作"],
  ["Installing", "安装操作"],
  ["RomQuery", "ROM 查询"],
]);

interface AuthUser {
  id: number;
  username: string;
  name: string;
  token: string;
}

interface ActivityRow {
  activity_type: "operation" | "rom";
  id: number;
  operation_kind: string;
  status: "success" | "failed" | "canceled";
  occurred_at: number;
  ended_at: number | null;
  duration_ms: number | null;
  pd: string | null;
  version: string | null;
  http_status: number | null;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (request.headers.get("x-forwarded-proto") === "http") {
      return Response.redirect(`https://${url.host}${url.pathname}${url.search}`, 301);
    }

    try {
      if (request.method === "GET" && (url.pathname === "/" || url.pathname === "")) {
        return text(portalHtml, "text/html; charset=utf-8");
      }
      if (request.method === "GET" && url.pathname === "/portal/styles.css") {
        return text(portalStyles, "text/css; charset=utf-8");
      }
      if (request.method === "GET" && url.pathname === "/portal/app.js") {
        return text(portalScript, "text/javascript; charset=utf-8");
      }
      if (url.pathname.startsWith("/api/")) return handleApi(request, url, env);
      return json({ message: "Not Found" }, 404);
    } catch (error) {
      console.error("unhandled", error);
      return json({ message: "内部错误。" }, 500);
    }
  },
};

async function handleApi(request: Request, url: URL, env: Env): Promise<Response> {
  const { method } = request;
  const path = url.pathname;

  if (method === "POST" && path === "/api/login") return login(request, env);

  if (method !== "GET" && request.headers.get("X-Requested-With") !== "XMLHttpRequest") {
    return json({ message: "请求缺少必要请求头。" }, 403);
  }

  if (method === "POST" && path === "/api/logout") {
    return json({ ok: true }, 200, { "Set-Cookie": expiredCookie() });
  }

  const authenticated = await authenticateUser(env, request);
  if (authenticated instanceof Response) return authenticated;
  const user = authenticated;

  if (method === "GET" && path === "/api/me") return me(env, user);
  if (method === "GET" && path === "/api/me/overview") return overview(env, user);
  if (method === "GET" && path === "/api/me/activities") return activities(url, env, user);
  if (method === "GET" && path.startsWith("/api/me/activities/")) return activityDetail(path, env, user);
  if (method === "POST" && path === "/api/me/password") return changePassword(request, env, user);
  if (method === "GET" && path === "/api/me/sessions") return sessions(env, user);
  if (method === "POST" && path === "/api/me/sessions/kick") return kickSession(request, env, user);

  return json({ message: "Not Found" }, 404);
}

async function login(request: Request, env: Env): Promise<Response> {
  const body = await request.json<Record<string, unknown>>().catch(() => null);
  const username = typeof body?.username === "string" ? body.username.trim() : "";
  const password = typeof body?.password === "string" ? body.password : "";
  const remember = body?.remember === true;
  if (!username || !password) return json({ message: "缺少用户名或密码。" }, 400);

  if (!await loginAllowed(env, request, username)) {
    return json({ message: "尝试过于频繁，请稍后再试。" }, 429);
  }

  const user = await env.DB.prepare(
    `SELECT id, username, name, token, password, salt, enabled, banned
     FROM api_users WHERE username = ?`,
  ).bind(username).first<{
    id: number;
    username: string;
    name: string;
    token: string;
    password: string | null;
    salt: string | null;
    enabled: number;
    banned: number;
  }>();

  if (!user || user.enabled !== 1 || user.banned !== 0 || !user.password || !user.salt) {
    return json({ message: "用户名或密码错误。" }, 401);
  }
  if (await pbkdf2(password, user.salt) !== user.password) {
    return json({ message: "用户名或密码错误。" }, 401);
  }

  let activeToken = user.token;
  if (activeToken.startsWith(REVOKED_TOKEN_PREFIX)) {
    const candidate = randomHex(32);
    const exchanged = await env.DB.prepare(
      `UPDATE api_users SET token = ?
       WHERE id = ? AND token = ? AND enabled = 1 AND banned = 0`,
    ).bind(candidate, user.id, activeToken).run();

    if (changed(exchanged) === 1) {
      activeToken = candidate;
    } else {
      const winner = await env.DB.prepare(
        "SELECT token FROM api_users WHERE id = ? AND enabled = 1 AND banned = 0",
      ).bind(user.id).first<{ token: string }>();
      if (!winner || winner.token.startsWith(REVOKED_TOKEN_PREFIX)) {
        return json({ message: "登录状态已变化，请重试。" }, 409);
      }
      activeToken = winner.token;
    }
  }

  if (!activeToken || activeToken.startsWith(REVOKED_TOKEN_PREFIX)) {
    return json({ message: "用户名或密码错误。" }, 401);
  }

  return json(
    { ok: true, username: user.username, name: user.name },
    200,
    { "Set-Cookie": activeCookie(activeToken, remember) },
  );
}

async function loginAllowed(env: Env, request: Request, username: string): Promise<boolean> {
  const ip = request.headers.get("CF-Connecting-IP")
    ?? request.headers.get("x-forwarded-for")
    ?? "unknown";
  const key = await sha256Hex(`${ip}|${username.toLowerCase()}`);
  const windowStart = Math.floor(Date.now() / 1000 / LOGIN_WINDOW_SEC) * LOGIN_WINDOW_SEC;
  const row = await env.DB.prepare(
    `INSERT INTO login_attempts (k, window_start, count) VALUES (?, ?, 1)
     ON CONFLICT(k, window_start) DO UPDATE SET count = count + 1
     RETURNING count`,
  ).bind(key, windowStart).first<{ count: number }>();
  await env.DB.prepare(
    "DELETE FROM login_attempts WHERE window_start < ?",
  ).bind(Math.floor(Date.now() / 1000) - 3_600).run();
  return Number(row?.count ?? LOGIN_MAX_ATTEMPTS + 1) <= LOGIN_MAX_ATTEMPTS;
}

async function authenticateUser(env: Env, request: Request): Promise<AuthUser | Response> {
  const token = cookieValue(request.headers.get("Cookie"), USER_COOKIE);
  if (!token) return json({ loggedIn: false, message: "请先登录。" }, 401);
  if (token.startsWith(REVOKED_TOKEN_PREFIX)) return unauthorizedWithExpiredCookie();

  const user = await env.DB.prepare(
    `SELECT id, username, name, token FROM api_users
     WHERE token = ? AND enabled = 1 AND banned = 0`,
  ).bind(token).first<AuthUser>();
  if (!user || user.token.startsWith(REVOKED_TOKEN_PREFIX)) return unauthorizedWithExpiredCookie();
  return user;
}

async function me(env: Env, user: AuthUser): Promise<Response> {
  const cutoff = Math.floor(Date.now() / 1000) - onlineTimeoutSec(env);
  const row = await env.DB.prepare(
    "SELECT COUNT(*) AS n FROM online_sessions WHERE user_id = ? AND last_seen_at >= ?",
  ).bind(user.id, cutoff).first<{ n: number }>();
  return json({
    loggedIn: true,
    username: user.username,
    name: user.name,
    online: Number(row?.n ?? 0),
  }, 200);
}

async function overview(env: Env, user: AuthUser): Promise<Response> {
  const cutoff = Math.floor(Date.now() / 1000) - onlineTimeoutSec(env);
  const row = await env.DB.prepare(
    `SELECT
       (SELECT COUNT(*) FROM usage_logs WHERE api_user_id = ?) AS operations,
       (SELECT COUNT(*) FROM access_logs WHERE api_user_id = ?) AS rom,
       (SELECT COUNT(*) FROM usage_logs WHERE api_user_id = ? AND status = 'success')
         + (SELECT COUNT(*) FROM access_logs WHERE api_user_id = ? AND status BETWEEN 200 AND 299) AS successes,
       (SELECT COUNT(*) FROM usage_logs WHERE api_user_id = ? AND status <> 'success')
         + (SELECT COUNT(*) FROM access_logs WHERE api_user_id = ? AND (status < 200 OR status > 299 OR status IS NULL)) AS failures,
       (SELECT COUNT(*) FROM online_sessions WHERE user_id = ? AND last_seen_at >= ?) AS active_sessions`,
  ).bind(user.id, user.id, user.id, user.id, user.id, user.id, user.id, cutoff).first<{
    operations: number;
    rom: number;
    successes: number;
    failures: number;
    active_sessions: number;
  }>();

  const operations = Number(row?.operations ?? 0);
  const rom = Number(row?.rom ?? 0);
  return json({
    total: operations + rom,
    operations,
    rom,
    successes: Number(row?.successes ?? 0),
    failures: Number(row?.failures ?? 0),
    activeSessions: Number(row?.active_sessions ?? 0),
  }, 200);
}

const ACTIVITY_UNION = `
  SELECT *
  FROM (
    SELECT 'operation' AS activity_type, id, operation_kind,
           CASE WHEN status IN ('success','failed','canceled') THEN status ELSE 'failed' END AS status,
           started_at AS occurred_at, ended_at, duration_ms,
           NULL AS pd, NULL AS version, NULL AS http_status
    FROM usage_logs WHERE api_user_id = ?
    UNION ALL
    SELECT 'rom' AS activity_type, id, 'RomQuery' AS operation_kind,
           CASE WHEN status BETWEEN 200 AND 299 THEN 'success' ELSE 'failed' END AS status,
           CAST(strftime('%s', created_at) AS INTEGER) AS occurred_at,
           NULL AS ended_at, NULL AS duration_ms,
           pd, version, status AS http_status
    FROM access_logs WHERE api_user_id = ?
  ) AS activities
  WHERE (? = 'all' OR activity_type = ?)
    AND (? = 'all' OR status = ?)`;

async function activities(url: URL, env: Env, user: AuthUser): Promise<Response> {
  const type = url.searchParams.get("type") ?? "all";
  const status = url.searchParams.get("status") ?? "all";
  if (!isActivityType(type)) return json({ message: "活动类型无效。" }, 400);
  if (!isActivityStatus(status)) return json({ message: "活动状态无效。" }, 400);

  const limit = clampInteger(url.searchParams.get("limit"), 100, 1, 100);
  const offset = clampInteger(url.searchParams.get("offset"), 0, 0, Number.MAX_SAFE_INTEGER);
  const bindings = [user.id, user.id, type, type, status, status] as const;
  const countRow = await env.DB.prepare(
    `SELECT COUNT(*) AS n FROM (${ACTIVITY_UNION}) AS filtered`,
  ).bind(...bindings).first<{ n: number }>();
  const rows = await env.DB.prepare(
    `${ACTIVITY_UNION}
     ORDER BY occurred_at DESC, id DESC
     LIMIT ? OFFSET ?`,
  ).bind(...bindings, limit, offset).all<ActivityRow>();

  return json({
    activities: rows.results.map(serializeActivity),
    count: Number(countRow?.n ?? 0),
    limit,
    offset,
  }, 200);
}

async function activityDetail(path: string, env: Env, user: AuthUser): Promise<Response> {
  const encoded = path.slice("/api/me/activities/".length);
  let activityId: string;
  try {
    activityId = decodeURIComponent(encoded);
  } catch {
    return json({ message: "活动 ID 无效。" }, 400);
  }
  const match = /^(operation|rom):([1-9][0-9]*)$/.exec(activityId);
  if (!match) return json({ message: "活动 ID 无效。" }, 400);
  const id = Number(match[2]);
  if (!Number.isSafeInteger(id)) return json({ message: "活动 ID 无效。" }, 400);

  if (match[1] === "operation") {
    const row = await env.DB.prepare(
      `SELECT id, operation_kind,
              CASE WHEN status IN ('success','failed','canceled') THEN status ELSE 'failed' END AS status,
              started_at, ended_at, duration_ms
       FROM usage_logs WHERE id = ? AND api_user_id = ?`,
    ).bind(id, user.id).first<{
      id: number;
      operation_kind: string;
      status: string;
      started_at: number;
      ended_at: number | null;
      duration_ms: number | null;
    }>();
    if (!row) return json(ACTIVITY_NOT_FOUND, 404);
    return json({
      id: `operation:${row.id}`,
      type: "operation",
      status: row.status,
      summary: safeOperationLabel(row.operation_kind),
      timestamp: row.started_at,
      ended_at: row.ended_at,
      duration_ms: row.duration_ms,
      steps_state: "unavailable",
      steps: [],
      steps_message: "无更详细数据",
    }, 200);
  }

  const row = await env.DB.prepare(
    `SELECT id, pd, version, status AS http_status,
            CASE WHEN status BETWEEN 200 AND 299 THEN 'success' ELSE 'failed' END AS status,
            CAST(strftime('%s', created_at) AS INTEGER) AS occurred_at
     FROM access_logs WHERE id = ? AND api_user_id = ?`,
  ).bind(id, user.id).first<{
    id: number;
    pd: string | null;
    version: string | null;
    http_status: number | null;
    status: string;
    occurred_at: number;
  }>();
  if (!row) return json(ACTIVITY_NOT_FOUND, 404);
  return json({
    id: `rom:${row.id}`,
    type: "rom",
    status: row.status,
    summary: "ROM 查询",
    timestamp: row.occurred_at,
    pd: row.pd,
    version: row.version,
    http_status: row.http_status,
  }, 200);
}

async function changePassword(request: Request, env: Env, user: AuthUser): Promise<Response> {
  const body = await request.json<Record<string, unknown>>().catch(() => null);
  const current = typeof body?.current === "string" ? body.current : "";
  const newPassword = typeof body?.newPassword === "string" ? body.newPassword : "";
  if (!current) return json({ message: "请输入当前密码。" }, 400);
  if (newPassword.length < 8) return json({ message: "新密码至少 8 位。" }, 400);
  if (current === newPassword) return json({ message: "新密码不能与当前密码相同。" }, 400);

  const credential = await env.DB.prepare(
    "SELECT password, salt FROM api_users WHERE id = ? AND token = ? AND enabled = 1 AND banned = 0",
  ).bind(user.id, user.token).first<{ password: string | null; salt: string | null }>();
  if (!credential) return json({ message: "登录状态已变化，请重新登录。" }, 409, { "Set-Cookie": expiredCookie() });
  if (!credential.password || !credential.salt) return json({ message: "该账号未设置密码。" }, 400);
  if (await pbkdf2(current, credential.salt) !== credential.password) {
    return json({ message: "当前密码错误。" }, 401);
  }

  const newSalt = randomHex(16);
  const newHash = await pbkdf2(newPassword, newSalt);
  const revokedToken = `${REVOKED_TOKEN_PREFIX}${randomHex(32)}`;
  const results = await env.DB.batch([
    env.DB.prepare(
      `UPDATE api_users
       SET salt = ?, password = ?, token = ?
       WHERE id = ? AND token = ? AND enabled = 1 AND banned = 0`,
    ).bind(newSalt, newHash, revokedToken, user.id, user.token),
    env.DB.prepare("DELETE FROM session_leases WHERE user_id = ?").bind(user.id),
    env.DB.prepare("DELETE FROM online_sessions WHERE user_id = ?").bind(user.id),
  ]);

  if (changed(results[0]) !== 1) {
    return json({ message: "登录状态已变化，请重新登录。" }, 409, { "Set-Cookie": expiredCookie() });
  }
  return json(
    { ok: true, reauthenticate: true },
    200,
    { "Set-Cookie": expiredCookie() },
  );
}

async function sessions(env: Env, user: AuthUser): Promise<Response> {
  const cutoff = Math.floor(Date.now() / 1000) - onlineTimeoutSec(env);
  const rows = await env.DB.prepare(
    `SELECT session_id, client_version, ip, connected_at, last_seen_at, force_exit_at, force_exit_reason
     FROM online_sessions
     WHERE user_id = ? AND last_seen_at >= ?
     ORDER BY last_seen_at DESC`,
  ).bind(user.id, cutoff).all<{
    session_id: string;
    client_version: string;
    ip: string | null;
    connected_at: number;
    last_seen_at: number;
    force_exit_at: number | null;
    force_exit_reason: string | null;
  }>();

  const now = Math.floor(Date.now() / 1000);
  const serialized = rows.results.map((row) => {
    const durationSeconds = Math.max(0, now - row.connected_at);
    return {
      id: row.session_id,
      clientVersion: row.client_version,
      ip_masked: maskIp(row.ip),
      connectedAt: new Date(row.connected_at * 1000).toISOString(),
      lastSeenAt: new Date(row.last_seen_at * 1000).toISOString(),
      duration: formatDuration(durationSeconds),
      pendingExit: row.force_exit_at !== null,
      pendingExitReason: row.force_exit_reason,
    };
  });
  return json({ count: serialized.length, sessions: serialized }, 200);
}

async function kickSession(request: Request, env: Env, user: AuthUser): Promise<Response> {
  const body = await request.json<Record<string, unknown>>().catch(() => null);
  const id = typeof body?.id === "string" ? body.id.trim() : "";
  if (!id) return json({ message: "缺少会话 ID。" }, 400);
  const row = await env.DB.prepare(
    `UPDATE online_sessions
     SET force_exit_at = ?, force_exit_reason = ?
     WHERE session_id = ? AND user_id = ?
     RETURNING session_id`,
  ).bind(Math.floor(Date.now() / 1000), "用户在本门户强制下线", id, user.id)
    .first<{ session_id: string }>();
  if (!row) return json(ACTIVITY_NOT_FOUND, 404);
  return json({ ok: true }, 200);
}

function serializeActivity(row: ActivityRow): Record<string, unknown> {
  return {
    id: `${row.activity_type}:${row.id}`,
    type: row.activity_type,
    status: row.status,
    summary: row.activity_type === "rom" ? "ROM 查询" : safeOperationLabel(row.operation_kind),
    timestamp: row.occurred_at,
    ...(row.activity_type === "operation"
      ? { ended_at: row.ended_at, duration_ms: row.duration_ms }
      : { pd: row.pd, version: row.version, http_status: row.http_status }),
  };
}

function safeOperationLabel(operationKind: string): string {
  return SAFE_OPERATION_LABELS.get(operationKind) ?? "工具操作";
}

function isActivityType(value: string): value is "all" | "operation" | "rom" {
  return value === "all" || value === "operation" || value === "rom";
}

function isActivityStatus(value: string): value is "all" | "success" | "failed" | "canceled" {
  return value === "all" || value === "success" || value === "failed" || value === "canceled";
}

function clampInteger(raw: string | null, fallback: number, minimum: number, maximum: number): number {
  if (raw === null || raw.trim() === "") return fallback;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(minimum, Math.min(Math.floor(parsed), maximum));
}

function onlineTimeoutSec(env: Env): number {
  return Math.floor((Number(env.ONLINE_TIMEOUT_MS) || 120_000) / 1000);
}

function maskIp(value: string | null): string {
  if (!value) return "已隐藏";
  const ipv4 = value.split(".");
  if (ipv4.length === 4 && ipv4.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255)) {
    return `${ipv4[0]}.${ipv4[1]}.${ipv4[2]}.••`;
  }
  if (isIpv6(value)) {
    const visible = value.split(":").filter(Boolean).slice(0, 2);
    return visible.length ? `${visible.join(":")}:…` : "已隐藏";
  }
  return "已隐藏";
}

function isIpv6(value: string): boolean {
  if (!value.includes(":") || !/^[0-9a-f:]+$/i.test(value)) return false;
  if ((value.match(/::/g) ?? []).length > 1) return false;
  const groups = value.split(":");
  if (groups.some((group) => group.length > 4)) return false;
  const nonempty = groups.filter(Boolean);
  return value.includes("::") ? nonempty.length < 8 : nonempty.length === 8;
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  return `${Math.floor(seconds / 3600)} 小时`;
}

function cookieValue(header: string | null, name: string): string {
  if (!header) return "";
  for (const part of header.split(";")) {
    const index = part.indexOf("=");
    if (index < 0 || part.slice(0, index).trim() !== name) continue;
    return part.slice(index + 1).trim();
  }
  return "";
}

function activeCookie(token: string, remember: boolean): string {
  const maxAge = remember ? `; Max-Age=${REMEMBER_MAX_AGE_SECONDS}` : "";
  return `${USER_COOKIE}=${token}; Path=/; Secure; HttpOnly; SameSite=Strict${maxAge}`;
}

function expiredCookie(): string {
  return `${USER_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0`;
}

function unauthorizedWithExpiredCookie(): Response {
  return json({ loggedIn: false, message: "登录已失效。" }, 401, { "Set-Cookie": expiredCookie() });
}

function text(content: string, contentType: string): Response {
  return new Response(content, { headers: { ...SECURE_HEADERS, "Content-Type": contentType } });
}

function json(body: unknown, status: number, extraHeaders: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { ...SECURE_HEADERS, "Content-Type": "application/json; charset=utf-8", ...extraHeaders },
  });
}

function changed(result: D1Result<unknown>): number {
  return Number(result.meta.changes ?? 0);
}

function randomHex(bytes: number): string {
  const array = new Uint8Array(bytes);
  crypto.getRandomValues(array);
  return [...array].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function pbkdf2(password: string, saltHex: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(password),
    "PBKDF2",
    false,
    ["deriveBits"],
  );
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", salt: hexToBytes(saltHex), iterations: PBKDF2_ITERATIONS, hash: "SHA-256" },
    key,
    256,
  );
  return [...new Uint8Array(bits)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function hexToBytes(hex: string): ArrayBuffer {
  const bytes = /^(?:[0-9a-f]{2})+$/i.test(hex)
    ? Uint8Array.from(hex.match(/.{2}/g) ?? [], (pair) => Number.parseInt(pair, 16))
    : new Uint8Array();
  return bytes.buffer as ArrayBuffer;
}
