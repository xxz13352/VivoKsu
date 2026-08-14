/**
 * user.nwflash.cc.cd —— Nwflash 用户自助门户(客户后台)。
 *
 * 面向 Nwflash 授权客户的自我服务表面:查看自己的 ROM 查询日志、
 * 修改自己的密码、查看自己账户的在线会话并可强制下线。
 *
 * 鉴权:与桌面端同源的 API token(api_users.token),请求带
 * Authorization: Bearer <token>;写操作额外校验 X-Requested-With(CSRF 兜底)。
 * 登录(账号+密码)在本 worker 内完成(PBKDF2-SHA256,与 api/web 同算法)。
 *
 * 安全:全站 HTTPS(Cloudflare 边缘 TLS 1.3)+ HSTS + CSP + no-store。
 */

import userHtml from "./user.html";

export interface Env {
  /** D1 绑定(nwflash-db,与 api/web 共用) */
  DB: D1Database;
  /** 在线判定窗口(ms):与 api worker 的 ONLINE_TIMEOUT_MS 保持一致。默认 120000。 */
  ONLINE_TIMEOUT_MS?: string;
}

const PBKDF2_ITERATIONS = 100_000;

/* 登录限流:内存 token bucket(按 CF-Connecting-IP + username,尽力而为;worker 级,配合边缘限流更佳) */
const LOGIN_WINDOW_MS = 60_000;
const LOGIN_MAX_ATTEMPTS = 8;
const loginBuckets = new Map<string, { count: number; resetAt: number }>();

function loginAllowed(request: Request, username: string): boolean {
  const ip = request.headers.get("CF-Connecting-IP")
    || request.headers.get("x-forwarded-for")
    || "unknown";
  const key = ip + ":" + username.toLowerCase();
  const now = Date.now();
  if (loginBuckets.size > 10_000) {
    for (const [k, b] of loginBuckets) { if (b.resetAt <= now) loginBuckets.delete(k); }
  }
  let bucket = loginBuckets.get(key);
  if (!bucket || bucket.resetAt <= now) {
    bucket = { count: 0, resetAt: now + LOGIN_WINDOW_MS };
    loginBuckets.set(key, bucket);
  }
  bucket.count++;
  return bucket.count <= LOGIN_MAX_ATTEMPTS;
}

const SECURE_HEADERS: Record<string, string> = {
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Referrer-Policy": "no-referrer",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  "Content-Security-Policy":
    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'",
  "Cache-Control": "no-store",
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    // 强制 HTTPS(Cloudflare 边缘通常已处理,双保险)
    const proto = request.headers.get("x-forwarded-proto");
    if (proto === "http") {
      return Response.redirect(`https://${url.host}${url.pathname}${url.search}`, 301);
    }

    try {
      // 页面
      if (request.method === "GET" && (url.pathname === "/" || url.pathname === "")) {
        return new Response(userHtml, {
          headers: { "Content-Type": "text/html; charset=utf-8", ...SECURE_HEADERS },
        });
      }

      if (url.pathname.startsWith("/api/")) {
        return await handleApi(request, url, env);
      }

      return json({ error: "Not found" }, 404);
    } catch (e) {
      console.error("unhandled", e);
      return json({ error: "内部错误。" }, 500);
    }
  },
};

/* ------------------------------------------------------------------ */
/* 路由                                                                */
/* ------------------------------------------------------------------ */

async function handleApi(request: Request, url: URL, env: Env): Promise<Response> {
  const path = url.pathname;
  const method = request.method;

  // 登录(免鉴权)
  if (method === "POST" && path === "/api/login") return login(request, env);

  // CSRF 兜底:所有写操作必须带 X-Requested-With(与页面 fetch 配套)。
  if (method !== "GET" && request.headers.get("X-Requested-With") !== "XMLHttpRequest") {
    return json({ error: "请求缺少必要请求头。" }, 403);
  }

  // 以下全部需要用户 token
  const user = await authenticateUser(env, request);
  if (user instanceof Response) return user; // 401
  if (user === null) return json({ error: "请先登录。" }, 401);

  if (path === "/api/me" && method === "GET") return me(env, user);
  if (path === "/api/me/logs" && method === "GET") return myLogs(url, env, user);
  if (path === "/api/me/password" && method === "POST") return changeMyPassword(request, env, user);
  if (path === "/api/me/sessions" && method === "GET") return mySessions(env, user);
  if (path === "/api/me/sessions/kick" && method === "POST") return kickMySession(request, env, user);

  return json({ error: "Not found" }, 404);
}

/* ------------------------------------------------------------------ */
/* 鉴权                                                                */
/* ------------------------------------------------------------------ */

interface AuthUser {
  id: number;
  username: string;
  name: string;
}

/** 从 Authorization: Bearer 解析 API 用户。无 token → null;无效/停用/封禁 → 401 Response。 */
async function authenticateUser(env: Env, request: Request): Promise<AuthUser | null | Response> {
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  if (!token) return null;

  const user = await env.DB
    .prepare("SELECT id, username, name, enabled, banned FROM api_users WHERE token = ?")
    .bind(token)
    .first<{ id: number; username: string; name: string; enabled: number; banned: number }>();
  if (!user || !user.enabled) return json({ error: "API token 无效或已停用。" }, 401);
  if (user.banned === 1) return json({ error: "账号已被封禁。" }, 403);
  return { id: user.id, username: user.username, name: user.name };
}

/** POST /api/login { username, password } → { ok, token, username, name }。 */
async function login(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const username = (body?.username as string || "").trim();
  const password = body?.password as string || "";
  if (!username || !password) return json({ error: "缺少用户名或密码。" }, 400);

  // 登录限流:先限流再查库(同一 IP+账号 8 次/分钟,防枚举轰炸)。
  if (!loginAllowed(request, username)) {
    return json({ error: "尝试过于频繁,请稍后再试。" }, 429);
  }

  const user = await env.DB
    .prepare("SELECT id, username, name, token, password, salt, enabled, banned FROM api_users WHERE username = ?")
    .bind(username)
    .first<{ id: number; username: string; name: string; token: string; password: string | null; salt: string | null; enabled: number; banned: number }>();
  if (!user) return json({ error: "用户名或密码错误。" }, 401);
  if (user.banned === 1) return json({ error: "账号已被封禁,请联系管理员。" }, 401);
  if (user.enabled !== 1) return json({ error: "账号已被停用。" }, 401);
  if (!user.password || !user.salt) return json({ error: "该账号未设置密码,请联系管理员。" }, 401);

  const hash = await pbkdf2(password, user.salt);
  if (hash !== user.password) return json({ error: "用户名或密码错误。" }, 401);

  return json({ ok: true, token: user.token, username, name: user.name }, 200);
}

/* ------------------------------------------------------------------ */
/* 我的信息 / 在线状态                                                   */
/* ------------------------------------------------------------------ */

/** 在线判定窗口(秒):与 api worker 一致。 */
function onlineTimeoutSec(env: Env): number {
  return Math.floor((Number(env.ONLINE_TIMEOUT_MS) || 120_000) / 1000);
}

/** GET /api/me —— 用户信息 + 当前是否在线(活跃会话数)。 */
async function me(env: Env, user: AuthUser): Promise<Response> {
  const cutoff = Math.floor(Date.now() / 1000) - onlineTimeoutSec(env);
  const row = await env.DB
    .prepare("SELECT COUNT(*) AS n FROM online_sessions WHERE user_id = ? AND last_seen_at >= ?")
    .bind(user.id, cutoff)
    .first<{ n: number }>();
  return json({ loggedIn: true, username: user.username, name: user.name, online: row?.n ?? 0 }, 200);
}

/** GET /api/me/sessions —— 我自己的在线会话(含强制下线状态)。 */
async function mySessions(env: Env, user: AuthUser): Promise<Response> {
  const cutoff = Math.floor(Date.now() / 1000) - onlineTimeoutSec(env);
  const rows = await env.DB
    .prepare(
      `SELECT session_id, client_version, ip, connected_at, last_seen_at, force_exit_at, force_exit_reason
       FROM online_sessions WHERE user_id = ? AND last_seen_at >= ?
       ORDER BY last_seen_at DESC`,
    )
    .bind(user.id, cutoff)
    .all<{
      session_id: string;
      client_version: string | null;
      ip: string | null;
      connected_at: number;
      last_seen_at: number;
      force_exit_at: number | null;
      force_exit_reason: string | null;
    }>();

  const now = Math.floor(Date.now() / 1000);
  const sessions = rows.results.map((r) => ({
    session_id: r.session_id,
    client_version: r.client_version,
    ip: r.ip,
    connected_at: r.connected_at,
    last_seen_at: r.last_seen_at,
    duration_seconds: Math.max(0, now - r.connected_at),
    force_exit: r.force_exit_at !== null,
    force_exit_reason: r.force_exit_reason,
  }));
  return json({ count: sessions.length, sessions }, 200);
}

/** POST /api/me/sessions/kick { sessionId } —— 强制下线自己的一个会话。 */
async function kickMySession(request: Request, env: Env, user: AuthUser): Promise<Response> {
  const body = await request.json().catch(() => null);
  const sessionId = typeof body?.sessionId === "string" ? body.sessionId.trim() : "";
  if (!sessionId) return json({ error: "缺少 sessionId。" }, 400);

  const now = Math.floor(Date.now() / 1000);
  const target = await env.DB
    .prepare(
      "UPDATE online_sessions SET force_exit_at = ?, force_exit_reason = ? WHERE session_id = ? AND user_id = ? RETURNING session_id",
    )
    .bind(now, "用户在本门户强制下线", sessionId, user.id)
    .first<{ session_id: string }>();
  if (!target) return json({ error: "目标会话不存在或不属于当前账号。" }, 404);

  return json({ ok: true, affected: 1 }, 200);
}

/* ------------------------------------------------------------------ */
/* 我的日志                                                             */
/* ------------------------------------------------------------------ */

/** GET /api/me/logs?limit&offset&pd —— 我自己的 ROM 查询日志。 */
async function myLogs(url: URL, env: Env, user: AuthUser): Promise<Response> {
  const limitRaw = url.searchParams.get("limit");
  const offsetRaw = url.searchParams.get("offset");
  const limit = Math.max(1, Math.min(limitRaw === null ? 100 : (Number(limitRaw) || 100), 500));
  const offset = Math.max(0, offsetRaw === null ? 0 : Math.floor(Number(offsetRaw) || 0));
  const pd = url.searchParams.get("pd")?.trim() || "";

  let where = "WHERE api_user_id = ?";
  const bind: unknown[] = [user.id];
  if (pd) { where += " AND pd = ?"; bind.push(pd); }

  const totalRow = await env.DB.prepare(`SELECT COUNT(*) AS n FROM access_logs ${where}`).bind(...bind).first<{ n: number }>();
  const rows = await env.DB
    .prepare(`SELECT id, pd, version, url, status, created_at FROM access_logs ${where} ORDER BY id DESC LIMIT ? OFFSET ?`)
    .bind(...bind, limit, offset)
    .all<{ id: number; pd: string; version: string; url: string | null; status: number; created_at: string }>();

  return json({ logs: rows.results, total: totalRow?.n ?? 0 }, 200);
}

/* ------------------------------------------------------------------ */
/* 修改密码                                                             */
/* ------------------------------------------------------------------ */

/** POST /api/me/password { current, newPassword } —— 校验当前密码后更新。 */
async function changeMyPassword(request: Request, env: Env, user: AuthUser): Promise<Response> {
  const body = await request.json().catch(() => null);
  const current = body?.current as string || "";
  const newPassword = body?.newPassword as string || "";
  if (!current) return json({ error: "请输入当前密码。" }, 400);
  if (newPassword.length < 6) return json({ error: "新密码至少 6 位。" }, 400);
  if (current === newPassword) return json({ error: "新密码不能与当前密码相同。" }, 400);

  const row = await env.DB
    .prepare("SELECT password, salt FROM api_users WHERE id = ?")
    .bind(user.id)
    .first<{ password: string | null; salt: string | null }>();
  if (!row?.password || !row?.salt) return json({ error: "该账号未设置密码,请联系管理员。" }, 400);

  const hash = await pbkdf2(current, row.salt);
  if (hash !== row.password) return json({ error: "当前密码错误。" }, 401);

  const newSalt = randomHex(16);
  const newHash = await pbkdf2(newPassword, newSalt);
  await env.DB.prepare("UPDATE api_users SET salt = ?, password = ? WHERE id = ?")
    .bind(newSalt, newHash, user.id)
    .run();

  return json({ ok: true }, 200);
}

/* ------------------------------------------------------------------ */
/* 工具                                                                */
/* ------------------------------------------------------------------ */

function json(obj: unknown, status: number, extraHeaders: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json; charset=utf-8", ...SECURE_HEADERS, ...extraHeaders },
  });
}

function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  return [...arr].map((b) => b.toString(16).padStart(2, "0")).join("");
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
  return [...new Uint8Array(bits)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
