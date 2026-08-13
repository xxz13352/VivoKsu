/**
 * web.nwflash.cc.cd —— VivoKsu ROM 服务后台管理。
 *
 * 功能:管理员登录 / 版本号控制 / API 用户管理 / 访问日志。
 * 与 api.nwflash.cc.cd 共用 D1 数据库(nwflash-db):版本控制与访问日志由 API 侧执行,
 * 本后台负责管理。
 *
 * 安全:全站 HTTPS(Cloudflare 边缘 TLS 1.3)+ HSTS + CSP + HttpOnly/Secure 会话 Cookie
 * + PBKDF2-SHA256 密码哈希 + 随机 session token。
 */

import adminHtml from "./admin.html";

export interface Env {
  /** D1 绑定(nwflash-db) */
  DB: D1Database;
  /** 首次部署时若库内无管理员,用此密码创建初始管理员(用户名 ADMIN_SEED_USERNAME,默认 admin)。部署后建议移除/改密。 */
  ADMIN_SEED_PASSWORD?: string;
  ADMIN_SEED_USERNAME?: string;
}

const SESSION_TTL_MS = 7 * 24 * 3600 * 1000; // 7 天
const PBKDF2_ITERATIONS = 100_000;

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
      await ensureAdminSeed(env);

      // 页面
      if (request.method === "GET" && (url.pathname === "/" || url.pathname === "")) {
        return new Response(adminHtml, {
          headers: { "Content-Type": "text/html; charset=utf-8", ...SECURE_HEADERS },
        });
      }

      // API
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

  // 登录 / 会话(免鉴权)
  if (method === "POST" && path === "/api/login") return login(request, env);
  if (method === "GET" && path === "/api/me") return me(request, env);
  if (method === "POST" && path === "/api/logout") return logout(request, env);

  // 以下全部需要管理员会话
  const admin = await requireAdmin(request, env);
  if (admin instanceof Response) return admin; // 401

  if (path === "/api/change-password" && method === "POST")
    return changePassword(request, admin, env);

  // 版本号控制
  if (path === "/api/versions" && method === "GET") return listVersions(env);
  if (path === "/api/versions" && method === "POST") return addVersion(request, env);
  if (path.startsWith("/api/versions/") && method === "PUT") return updateVersion(request, path, env);
  if (path.startsWith("/api/versions/") && method === "DELETE") return deleteVersion(path, env);

  // 用户管理
  if (path === "/api/users" && method === "GET") return listUsers(env);
  if (path === "/api/users" && method === "POST") return addUser(request, env);
  if (path.startsWith("/api/users/") && method === "PUT") return updateUser(request, path, env);
  if (path.startsWith("/api/users/") && method === "DELETE") return deleteUser(path, env);
  if (path.endsWith("/rotate-token") && method === "POST")
    return rotateUserToken(request, path, env);

  // 日志
  if (path === "/api/logs" && method === "GET") return listLogs(url, env);

  return json({ error: "Not found" }, 404);
}

/* ------------------------------------------------------------------ */
/* 鉴权                                                                */
/* ------------------------------------------------------------------ */

async function ensureAdminSeed(env: Env) {
  const seedPassword = env.ADMIN_SEED_PASSWORD;
  if (!seedPassword) return;
  const existing = await env.DB.prepare("SELECT COUNT(*) AS n FROM admins").first<{ n: number }>();
  if (existing && existing.n > 0) return;

  const username = env.ADMIN_SEED_USERNAME || "admin";
  const salt = randomHex(16);
  const hash = await pbkdf2(seedPassword, salt);
  await env.DB.prepare("INSERT INTO admins (username, salt, password_hash) VALUES (?, ?, ?)")
    .bind(username, salt, hash)
    .run();
}

async function login(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const username = body?.username as string;
  const password = body?.password as string;
  if (!username || !password) return json({ error: "缺少用户名或密码。" }, 400);

  const admin = await env.DB.prepare("SELECT * FROM admins WHERE username = ?")
    .bind(username)
    .first<AdminRow>();
  if (!admin) return json({ error: "用户名或密码错误。" }, 401);

  const hash = await pbkdf2(password, admin.salt);
  if (hash !== admin.password_hash) return json({ error: "用户名或密码错误。" }, 401);

  const token = randomHex(32);
  const expires = new Date(Date.now() + SESSION_TTL_MS).toISOString();
  await env.DB.prepare("INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (?, ?, ?)")
    .bind(admin.id, token, expires)
    .run();

  return json({ ok: true, username: admin.username }, 200, {
    "Set-Cookie": `nwflash_session=${token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${Math.floor(SESSION_TTL_MS / 1000)}`,
  });
}

async function me(request: Request, env: Env): Promise<Response> {
  const admin = await requireAdmin(request, env);
  if (admin instanceof Response) return json({ loggedIn: false }, 200);
  return json({ loggedIn: true, username: admin.username }, 200);
}

async function logout(request: Request, env: Env): Promise<Response> {
  const token = getSessionToken(request);
  if (token) {
    await env.DB.prepare("DELETE FROM admin_sessions WHERE token = ?").bind(token).run();
  }
  return json({ ok: true }, 200, {
    "Set-Cookie": "nwflash_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
  });
}

async function requireAdmin(request: Request, env: Env): Promise<AdminRow | Response> {
  const token = getSessionToken(request);
  if (!token) return json({ error: "未登录。" }, 401);
  const session = await env.DB.prepare(
    "SELECT * FROM admin_sessions WHERE token = ? AND expires_at > ?"
  )
    .bind(token, new Date().toISOString())
    .first<AdminSessionRow>();
  if (!session) return json({ error: "会话已过期,请重新登录。" }, 401);

  const admin = await env.DB.prepare("SELECT * FROM admins WHERE id = ?").bind(session.admin_id).first<AdminRow>();
  if (!admin) return json({ error: "管理员不存在。" }, 401);
  return admin;
}

function getSessionToken(request: Request): string | null {
  const cookie = request.headers.get("Cookie") || "";
  for (const part of cookie.split(";")) {
    const [k, ...rest] = part.trim().split("=");
    if (k === "nwflash_session") return rest.join("=");
  }
  return null;
}

async function changePassword(request: Request, admin: AdminRow, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const newPassword = body?.newPassword as string;
  if (!newPassword || newPassword.length < 8) return json({ error: "新密码至少 8 位。" }, 400);

  const salt = randomHex(16);
  const hash = await pbkdf2(newPassword, salt);
  await env.DB.prepare("UPDATE admins SET salt = ?, password_hash = ? WHERE id = ?")
    .bind(salt, hash, admin.id)
    .run();

  // 改密后吊销其它会话
  await env.DB.prepare("DELETE FROM admin_sessions WHERE admin_id = ?").bind(admin.id).run();
  return json({ ok: true }, 200);
}

/* ------------------------------------------------------------------ */
/* 版本号控制                                                           */
/* ------------------------------------------------------------------ */

async function listVersions(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    "SELECT id, pd, version, enabled, created_at FROM versions ORDER BY pd, version"
  ).all<VersionRow>();
  return json({ versions: rows.results }, 200);
}

async function addVersion(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const pd = (body?.pd as string || "").trim();
  const version = (body?.version as string || "").trim();
  if (!pd || !version) return json({ error: "缺少 pd 或 version。" }, 400);

  const existing = await env.DB.prepare("SELECT id FROM versions WHERE pd = ? AND version = ?")
    .bind(pd, version)
    .first();
  if (existing) return json({ error: "该 PD + 版本已存在。" }, 409);

  await env.DB.prepare("INSERT INTO versions (pd, version) VALUES (?, ?)").bind(pd, version).run();
  return json({ ok: true }, 201);
}

async function updateVersion(request: Request, path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  const body = await request.json().catch(() => null);
  const enabled = body?.enabled;
  if (typeof enabled !== "boolean") return json({ error: "缺少 enabled。" }, 400);

  await env.DB.prepare("UPDATE versions SET enabled = ? WHERE id = ?")
    .bind(enabled ? 1 : 0, id)
    .run();
  return json({ ok: true }, 200);
}

async function deleteVersion(path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  await env.DB.prepare("DELETE FROM versions WHERE id = ?").bind(id).run();
  return json({ ok: true }, 200);
}

/* ------------------------------------------------------------------ */
/* API 用户管理                                                         */
/* ------------------------------------------------------------------ */

async function listUsers(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    "SELECT id, name, enabled, note, created_at FROM api_users ORDER BY id"
  ).all<UserRow>(); // token 不回显
  return json({ users: rows.results }, 200);
}

async function addUser(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const name = (body?.name as string || "").trim();
  if (!name) return json({ error: "缺少用户名。" }, 400);

  const token = randomHex(32); // 客户端 API token
  const note = (body?.note as string || "").trim();
  const res = await env.DB.prepare("INSERT INTO api_users (name, token, note) VALUES (?, ?, ?)")
    .bind(name, token, note)
    .run();
  const id = Number(res.meta.last_row_id);
  return json({ ok: true, id, token }, 201); // token 只在创建时显示一次
}

async function updateUser(request: Request, path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  const body = await request.json().catch(() => null);

  if (typeof body?.enabled === "boolean") {
    await env.DB.prepare("UPDATE api_users SET enabled = ? WHERE id = ?")
      .bind(body.enabled ? 1 : 0, id)
      .run();
  }
  if (typeof body?.note === "string") {
    await env.DB.prepare("UPDATE api_users SET note = ? WHERE id = ?").bind(body.note.trim(), id).run();
  }
  return json({ ok: true }, 200);
}

async function deleteUser(path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  await env.DB.prepare("DELETE FROM api_users WHERE id = ?").bind(id).run();
  return json({ ok: true }, 200);
}

async function rotateUserToken(request: Request, path: string, env: Env): Promise<Response> {
  const parts = path.split("/");
  const id = Number(parts[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  const token = randomHex(32);
  await env.DB.prepare("UPDATE api_users SET token = ? WHERE id = ?").bind(token, id).run();
  return json({ ok: true, token }, 200);
}

/* ------------------------------------------------------------------ */
/* 访问日志                                                             */
/* ------------------------------------------------------------------ */

async function listLogs(url: URL, env: Env): Promise<Response> {
  const limit = Math.min(Number(url.searchParams.get("limit") || 100), 500);
  const offset = Math.max(Number(url.searchParams.get("offset") || 0), 0);
  const userId = url.searchParams.get("userId");
  const pd = url.searchParams.get("pd");

  let sql = "SELECT id, api_user_name, pd, version, url, status, created_at FROM access_logs";
  const where: string[] = [];
  const bind: unknown[] = [];
  if (userId) { where.push("api_user_id = ?"); bind.push(Number(userId)); }
  if (pd) { where.push("pd = ?"); bind.push(pd); }
  if (where.length) sql += " WHERE " + where.join(" AND ");
  sql += " ORDER BY id DESC LIMIT ? OFFSET ?";
  bind.push(limit, offset);

  const rows = await env.DB.prepare(sql).bind(...bind).all<LogRow>();
  return json({ logs: rows.results }, 200);
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

interface AdminRow {
  id: number;
  username: string;
  salt: string;
  password_hash: string;
}

interface AdminSessionRow {
  id: number;
  admin_id: number;
  token: string;
  expires_at: string;
}

interface VersionRow {
  id: number;
  pd: string;
  version: string;
  enabled: number;
  created_at: string;
}

interface UserRow {
  id: number;
  name: string;
  enabled: number;
  note: string;
  created_at: string;
}

interface LogRow {
  id: number;
  api_user_name: string | null;
  pd: string;
  version: string;
  url: string | null;
  status: number;
  created_at: string;
}
