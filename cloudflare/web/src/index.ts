/**
 * web.nwflash.cc.cd —— Nwflash ROM 服务后台管理。
 *
 * 功能:管理员登录 / 版本号控制 / API 用户管理 / 访问日志 / 在线会话管理(强制下线)。
 * 与 api.nwflash.cc.cd 共用 D1 数据库(nwflash-db):版本控制与访问日志由 API 侧执行,
 * 本后台负责管理;在线会话行由 API 侧写入,本后台读取并设置 force_exit。
 *
 * 安全:全站 HTTPS(Cloudflare 边缘 TLS 1.3)+ HSTS + CSP + HttpOnly/Secure 会话 Cookie
 * + PBKDF2-SHA256 密码哈希 + 随机 session token + 状态变更请求校验 X-Requested-With(CSRF 兜底)。
 */

import adminHtml from "./admin.html";
import {
  exportTracesV2,
  getAppVersionsSummaryV2,
  getTraceEventV2,
  getTraceOutputV2,
  getTraceOverviewV2,
  getTraceRunV2,
  listRomLogsV2,
  listTraceRunsV2,
  listTraceUsersV2,
  traceQueryErrorResponse,
} from "./trace-v2-query";

export interface Env {
  /** D1 绑定(nwflash-db) */
  DB: D1Database;
  /** 首次部署时若库内无管理员,用此密码创建初始管理员(用户名 ADMIN_SEED_USERNAME,默认 admin)。部署后建议移除/改密。 */
  ADMIN_SEED_PASSWORD?: string;
  ADMIN_SEED_USERNAME?: string;
  /** 在线判定窗口(ms):与 api worker 的 ONLINE_TIMEOUT_MS 保持一致。默认 120000。 */
  ONLINE_TIMEOUT_MS?: string;
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
      if (isFrozenAdminApiPath(url.pathname)) {
        return traceQueryErrorResponse(request, 500, "TRACE_INTERNAL", "内部错误。");
      }
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

  // CSRF 兜底:所有状态变更请求必须带 X-Requested-With(与 admin.html 的 fetch 配套)。
  // 登录除外(跨站表单无法自定义该头,此处覆盖登录后的全部写操作)。
  if (method !== "GET" && request.headers.get("X-Requested-With") !== "XMLHttpRequest") {
    if (isFrozenAdminApiPath(path)) {
      return traceQueryErrorResponse(request, 403, "TRACE_FORBIDDEN", "请求缺少必要请求头。");
    }
    return json({ error: "请求缺少必要请求头。" }, 403);
  }

  // 以下全部需要管理员会话
  const admin = await requireAdmin(request, env);
  if (admin instanceof Response) {
    if (isFrozenAdminApiPath(path)) {
      return traceQueryErrorResponse(request, 401, "TRACE_UNAUTHORIZED", "未登录或会话已过期。");
    }
    return admin; // 401
  }

  if (path === "/api/change-password" && method === "POST")
    return changePassword(request, admin, env);

  // Nwflash 版本控制(强制更新)
  if (path === "/api/app-versions/summary") {
    if (method === "GET") return getAppVersionsSummaryV2(request, env);
    return traceQueryErrorResponse(request, 405, "TRACE_INVALID", "该接口只支持 GET。");
  }
  if (path === "/api/app-versions" && method === "GET") return listAppVersions(env);
  if (path === "/api/app-versions" && method === "POST") return addAppVersion(request, env);
  if (path.startsWith("/api/app-versions/") && method === "PUT") return updateAppVersion(request, path, env);
  if (path.startsWith("/api/app-versions/") && method === "DELETE") return deleteAppVersion(path, env);

  // 用户管理
  if (path === "/api/users" && method === "GET") return listUsers(env);
  if (path === "/api/users" && method === "POST") return addUser(request, env);
  if (path.startsWith("/api/users/") && method === "PUT") return updateUser(request, path, env);
  if (path.startsWith("/api/users/") && method === "DELETE") return deleteUser(path, env);
  if (path.endsWith("/rotate-token") && method === "POST")
    return rotateUserToken(request, path, env);

  // 日志
  if (path === "/api/logs" && method === "GET") return listLogs(url, env);

  // 在线会话(管理端)
  if (path === "/api/online" && method === "GET") return onlineAdmin(env);
  if (path === "/api/online/kick" && method === "POST") return kickOnline(request, admin, env);

  // 使用日志(管理端)
  if (path === "/api/usage-logs/v2/users" && method === "GET")
    return listTraceUsersV2(request, url, env);
  if (path === "/api/usage-logs/v2/runs" && method === "GET")
    return listTraceRunsV2(request, url, env);
  if (path === "/api/usage-logs/v2/overview" && method === "GET")
    return getTraceOverviewV2(request, url, env);
  if (path === "/api/usage-logs/v2/export" && method === "GET")
    return exportTracesV2(request, url, admin, env);

  const outputMatch = path.match(/^\/api\/usage-logs\/v2\/runs\/([^/]+)\/events\/([^/]+)\/output$/);
  if (outputMatch && method === "GET") {
    const segments = decodeTraceRouteSegments(request, outputMatch[1], outputMatch[2]);
    if (segments instanceof Response) return segments;
    return getTraceOutputV2(request, segments[0], segments[1], url, admin, env);
  }
  const eventMatch = path.match(/^\/api\/usage-logs\/v2\/runs\/([^/]+)\/events\/([^/]+)$/);
  if (eventMatch && method === "GET") {
    const segments = decodeTraceRouteSegments(request, eventMatch[1], eventMatch[2]);
    if (segments instanceof Response) return segments;
    return getTraceEventV2(request, segments[0], segments[1], env);
  }
  const runMatch = path.match(/^\/api\/usage-logs\/v2\/runs\/([^/]+)$/);
  if (runMatch && method === "GET") {
    const segments = decodeTraceRouteSegments(request, runMatch[1]);
    if (segments instanceof Response) return segments;
    return getTraceRunV2(request, segments[0], env);
  }

  if (path === "/api/rom-logs/v2" && method === "GET")
    return listRomLogsV2(request, url, env);

  if (path === "/api/usage-logs/kinds" && method === "GET") return usageLogKinds(env);
  if (path === "/api/usage-logs" && method === "GET") return listUsageLogs(url, env);

  if (isFrozenAdminApiPath(path)) {
    return traceQueryErrorResponse(request, 404, "TRACE_INVALID", "接口不存在。");
  }
  return json({ error: "Not found" }, 404);
}

function isFrozenAdminApiPath(path: string): boolean {
  return path === "/api/usage-logs/v2"
    || path.startsWith("/api/usage-logs/v2/")
    || path === "/api/app-versions/summary"
    || path === "/api/rom-logs/v2";
}

function decodeTraceRouteSegments(request: Request, ...encoded: string[]): string[] | Response {
  try {
    return encoded.map((segment) => decodeURIComponent(segment));
  } catch {
    return traceQueryErrorResponse(request, 400, "TRACE_INVALID", "路径参数编码无效。");
  }
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
/* Nwflash 版本控制(强制更新)                                            */
/* ------------------------------------------------------------------ */

async function listAppVersions(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    "SELECT id, version, min_version, download_url, note, enabled, created_at FROM app_versions ORDER BY id DESC"
  ).all<AppVersionRow>();
  return json({ versions: rows.results }, 200);
}

async function addAppVersion(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const version = (body?.version as string || "").trim();
  if (!version) return json({ error: "缺少版本号。" }, 400);
  const minVersion = (body?.min_version as string || "").trim() || "0.0.0";
  const downloadUrl = (body?.download_url as string || "").trim();
  const note = (body?.note as string || "").trim();

  const existing = await env.DB.prepare("SELECT id FROM app_versions WHERE version = ?").bind(version).first();
  if (existing) return json({ error: "该版本号已存在。" }, 409);

  await env.DB.prepare("INSERT INTO app_versions (version, min_version, download_url, note) VALUES (?, ?, ?, ?)")
    .bind(version, minVersion, downloadUrl, note)
    .run();
  return json({ ok: true }, 201);
}

async function updateAppVersion(request: Request, path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  const body = await request.json().catch(() => null);

  if (typeof body?.enabled === "boolean") {
    await env.DB.prepare("UPDATE app_versions SET enabled = ? WHERE id = ?")
      .bind(body.enabled ? 1 : 0, id)
      .run();
  }
  if (typeof body?.min_version === "string" && body.min_version.trim()) {
    await env.DB.prepare("UPDATE app_versions SET min_version = ? WHERE id = ?")
      .bind(body.min_version.trim(), id)
      .run();
  }
  if (typeof body?.download_url === "string") {
    await env.DB.prepare("UPDATE app_versions SET download_url = ? WHERE id = ?")
      .bind(body.download_url.trim(), id)
      .run();
  }
  if (typeof body?.note === "string") {
    await env.DB.prepare("UPDATE app_versions SET note = ? WHERE id = ?")
      .bind(body.note.trim(), id)
      .run();
  }
  return json({ ok: true }, 200);
}

async function deleteAppVersion(path: string, env: Env): Promise<Response> {
  const id = Number(path.split("/")[3]);
  if (!Number.isFinite(id)) return json({ error: "无效 id。" }, 400);
  await env.DB.prepare("DELETE FROM app_versions WHERE id = ?").bind(id).run();
  return json({ ok: true }, 200);
}

/* ------------------------------------------------------------------ */
/* API 用户管理                                                         */
/* ------------------------------------------------------------------ */

async function listUsers(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    "SELECT id, username, name, enabled, banned, note, created_at FROM api_users ORDER BY id"
  ).all<UserRow>(); // token / password 不回显
  return json({ users: rows.results }, 200);
}

async function addUser(request: Request, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const username = (body?.username as string || "").trim();
  const name = (body?.name as string || "").trim() || username;
  const password = body?.password as string || "";
  if (!username) return json({ error: "缺少登录账号(username)。" }, 400);
  if (password.length < 6) return json({ error: "初始密码至少 6 位。" }, 400);

  const exists = await env.DB.prepare("SELECT id FROM api_users WHERE username = ?").bind(username).first();
  if (exists) return json({ error: "登录账号已存在。" }, 409);

  const token = randomHex(32);
  const salt = randomHex(16);
  const passwordHash = await pbkdf2(password, salt);
  const note = (body?.note as string || "").trim();
  const res = await env.DB
    .prepare("INSERT INTO api_users (username, name, token, password, salt, note) VALUES (?, ?, ?, ?, ?, ?)")
    .bind(username, name, token, passwordHash, salt, note)
    .run();
  const id = Number(res.meta.last_row_id);
  return json({ ok: true, id, username, name, token }, 201); // token 只在创建时显示一次
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
  if (typeof body?.banned === "boolean") {
    await env.DB.prepare("UPDATE api_users SET banned = ? WHERE id = ?")
      .bind(body.banned ? 1 : 0, id)
      .run();
  }
  if (typeof body?.note === "string") {
    await env.DB.prepare("UPDATE api_users SET note = ? WHERE id = ?").bind(body.note.trim(), id).run();
  }
  if (typeof body?.newPassword === "string" && body.newPassword.length >= 6) {
    const salt = randomHex(16);
    const passwordHash = await pbkdf2(body.newPassword, salt);
    await env.DB.prepare("UPDATE api_users SET salt = ?, password = ? WHERE id = ?")
      .bind(salt, passwordHash, id)
      .run();
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
  const limitRaw = Number(url.searchParams.get("limit"));
  const offsetRaw = Number(url.searchParams.get("offset"));
  const limit = Math.max(1, Math.min(Number.isFinite(limitRaw) ? limitRaw : 100, 500));
  const offset = Math.max(0, Number.isFinite(offsetRaw) ? Math.floor(offsetRaw) : 0);
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
/* 在线会话(管理端):读取 + 强制下线                                       */
/* ------------------------------------------------------------------ */

/** GET /api/online(管理端)—— 完整字段(含 username/IP/session_id),仅 admin 可见。 */
async function onlineAdmin(env: Env): Promise<Response> {
  const timeoutSec = Math.floor((Number(env.ONLINE_TIMEOUT_MS) || 120_000) / 1000);
  const cutoff = Math.floor(Date.now() / 1000) - timeoutSec;
  const rows = await env.DB.prepare(
    `SELECT s.session_id, s.user_id, u.username, s.user_name AS name,
            s.client_version, s.ip, s.connected_at, s.last_seen_at,
            s.force_exit_at, s.force_exit_reason
     FROM online_sessions s
     JOIN api_users u ON u.id = s.user_id
     WHERE s.last_seen_at >= ?
     ORDER BY s.last_seen_at DESC`,
  )
    .bind(cutoff)
    .all<OnlineSessionRow>();

  const now = Math.floor(Date.now() / 1000);
  const sessions = rows.results.map((r) => ({
    session_id: r.session_id,
    user_id: r.user_id,
    username: r.username,
    name: r.name,
    client_version: r.client_version,
    ip: r.ip,
    connected_at: r.connected_at,
    last_seen_at: r.last_seen_at,
    duration_seconds: Math.max(0, now - r.connected_at),
    force_exit: r.force_exit_at !== null,
  }));
  return json({ count: sessions.length, sessions }, 200);
}

/** POST /api/online/kick —— 设 force_exit,客户端下一个心跳(≤5s)收到后退出进程。 */
async function kickOnline(request: Request, admin: AdminRow, env: Env): Promise<Response> {
  const body = await request.json().catch(() => null);
  const sessionId = typeof body?.sessionId === "string" ? body.sessionId.trim() : "";
  const userId = Number(body?.userId);
  const reason = typeof body?.reason === "string" ? body.reason.slice(0, 200).trim() : "";
  const now = Math.floor(Date.now() / 1000);

  // 只审计成功 kick;目标 user_id 由 RETURNING 取回(不依赖调用方传)。
  if (sessionId) {
    const target = await env.DB.prepare(
      "UPDATE online_sessions SET force_exit_at = ?, force_exit_reason = ? WHERE session_id = ? RETURNING user_id",
    )
      .bind(now, reason, sessionId)
      .first<{ user_id: number }>();
    if (!target) return json({ error: "目标不在线或不存在。" }, 404);
    await writeAudit(env, admin, "kick", target.user_id, sessionId, reason);
    return json({ ok: true, affected: 1 }, 200);
  }

  if (Number.isFinite(userId)) {
    // kick-by-user 更新该用户全部会话(否则第二台设备/新会话逃逸)。
    const target = await env.DB.prepare(
      "UPDATE online_sessions SET force_exit_at = ?, force_exit_reason = ? WHERE user_id = ? RETURNING user_id",
    )
      .bind(now, reason, userId)
      .first<{ user_id: number }>();
    if (!target) return json({ error: "目标不在线或不存在。" }, 404);
    await writeAudit(env, admin, "kick", userId, null, reason);
    return json({ ok: true, affected: 1 }, 200);
  }

  return json({ error: "缺少 sessionId 或 userId。" }, 400);
}

/** 写一条管理动作审计(尽力而为,失败不影响踢人结果)。 */
async function writeAudit(
  env: Env,
  admin: AdminRow,
  action: string,
  targetUserId: number | null,
  targetSessionId: string | null,
  reason: string,
): Promise<void> {
  try {
    await env.DB.prepare(
      "INSERT INTO admin_audit_log (admin_id, admin_username, action, target_user_id, target_session_id, reason) VALUES (?, ?, ?, ?, ?, ?)",
    )
      .bind(admin.id, admin.username, action, targetUserId, targetSessionId, reason)
      .run();
  } catch {
    // 审计失败不影响踢人结果。
  }
}

/** GET /api/usage-logs/kinds(管理端)—— 已出现的操作分类,用于下拉筛选。 */
async function usageLogKinds(env: Env): Promise<Response> {
  const rows = await env.DB.prepare(
    "SELECT DISTINCT operation_kind AS kind FROM usage_logs ORDER BY kind",
  ).all<{ kind: string }>();
  return json({ kinds: rows.results.map((r) => r.kind) }, 200);
}

/** GET /api/usage-logs(管理端)—— 客户端使用日志,按操作分类过滤。返回 total 供分页/徽章用。 */
async function listUsageLogs(url: URL, env: Env): Promise<Response> {
  // limit 上限 500、下限 1;offset 下限 0;NaN 回退默认(防 ?limit=-1 无界返回 / ?limit=abc 500)。
  const limitRaw = Number(url.searchParams.get("limit"));
  const offsetRaw = Number(url.searchParams.get("offset"));
  const limit = Math.max(1, Math.min(Number.isFinite(limitRaw) ? limitRaw : 100, 500));
  const offset = Math.max(0, Number.isFinite(offsetRaw) ? Math.floor(offsetRaw) : 0);
  const kind = url.searchParams.get("kind");
  const userId = url.searchParams.get("userId");
  const status = url.searchParams.get("status");

  let where = "";
  const whereClause: string[] = [];
  const bind: unknown[] = [];
  if (kind) { whereClause.push("operation_kind = ?"); bind.push(kind); }
  if (userId) { whereClause.push("api_user_id = ?"); bind.push(Number(userId)); }
  if (status) { whereClause.push("status = ?"); bind.push(status); }
  if (whereClause.length) where = " WHERE " + whereClause.join(" AND ");

  const totalRow = await env.DB.prepare(`SELECT COUNT(*) AS n FROM usage_logs${where}`).bind(...bind).first<{ n: number }>();
  const rows = await env.DB.prepare(
    `SELECT id, api_user_name, operation_kind, title, status, started_at, ended_at, duration_ms FROM usage_logs${where} ORDER BY id DESC LIMIT ? OFFSET ?`,
  )
    .bind(...bind, limit, offset)
    .all<UsageLogRow>();

  return json({ logs: rows.results, total: totalRow?.n ?? 0 }, 200);
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

interface AppVersionRow {
  id: number;
  version: string;
  min_version: string;
  download_url: string;
  note: string;
  enabled: number;
  created_at: string;
}

interface UserRow {
  id: number;
  username: string;
  name: string;
  enabled: number;
  banned: number;
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

interface OnlineSessionRow {
  session_id: string;
  user_id: number;
  username: string;
  name: string;
  client_version: string;
  ip: string;
  connected_at: number;
  last_seen_at: number;
  force_exit_at: number | null;
  force_exit_reason: string | null;
}

interface UsageLogRow {
  id: number;
  api_user_name: string | null;
  operation_kind: string;
  title: string | null;
  status: string;
  started_at: number;
  ended_at: number | null;
  duration_ms: number | null;
}
