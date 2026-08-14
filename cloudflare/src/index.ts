/**
 * Cloudflare Worker —— Vivo ROM OTA 链接代理 + Nwflash 版本门禁。
 * 桌面应用带 PD + 版本号查询,Worker 持 VOTA 凭据,
 * 转发到 VOTA API(https://api.otau.cc.cd)取 OTA 下载链接,不向客户端暴露 token。
 *
 * 端点:
 *   GET /health                          -> { status, source }
 *   GET /api/app/version?current=X       -> Nwflash 版本策略(免登录,启动拦截用)
 *   POST /api/heartbeat                  -> 在线会话心跳(鉴权;检测强制下线 / 封禁 / 426)
 *   GET /api/online                      -> 在线用户列表(鉴权;仅显示名/版本/时长,不含 username/IP)
 *   GET /api/rom?pd=X&version=Y          -> { pd, version, url, name, sizeBytes, sha256 }
 *
 * 版本门禁:所有请求必须带 X-Nwflash-Version 头;低于后台「版本号控制」的最低版本 → 426。
 *
 * 错误映射:NOT_FOUND/not found->404, AUTH_FAIL->401, INSUFFICIENT_CREDITS->402,
 * FORBIDDEN->403, RATE_LIMITED->429, UPDATE_REQUIRED->426, 其它->502。
 */

export interface Env {
  /** VOTA API Token(worker secret,通过 `wrangler secret put VOTA_API_TOKEN` 设置)。 */
  VOTA_API_TOKEN: string;
  /** 上游 VOTA 基地址,默认 https://api.otau.cc.cd。 */
  VOTA_BASE_URL?: string;
  /** 调用 action:resolve_url(OTA,-1 信用点)/ resolve_flash_url(线刷,-3)。默认 resolve_url。 */
  VOTA_ACTION?: string;
  /** 平台版本白名单,默认 0.1.0。 */
  VOTA_VER?: string;
  /** D1 绑定(nwflash-db,与 web.nwflash.cc.cd 共用):访问日志 + Nwflash 版本控制 + 在线会话。 */
  DB: D1Database;
  /** 心跳写节流(ms):同一会话 last_seen 至少隔这么久才写一次 D1。默认 60000。 */
  HEARTBEAT_WRITE_INTERVAL_MS?: string;
  /** 在线判定窗口(ms):last_seen 在此窗口内的会话视为在线;超窗即 stale 并被清理。默认 120000。 */
  ONLINE_TIMEOUT_MS?: string;
  /** 每用户同时在线会话数上限(超出删最旧的)。默认 3。 */
  ONLINE_SESSION_CAP?: string;
}

const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET,POST,OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type, Authorization, X-Nwflash-Version",
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: CORS });
    }

    const url = new URL(request.url);
    try {
      if (url.pathname === "/health") {
        return json({ status: "ok", source: "VotaApiRomSource" }, 200);
      }

      // Nwflash 版本策略(免登录,桌面端启动拦截用)。
      if (url.pathname === "/api/app/version" && request.method === "GET") {
        return appVersion(env, request, url);
      }

      // 桌面端登录:账号+密码 → 返回 API token(商业工具门禁)。
      if (url.pathname === "/api/login" && request.method === "POST") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        return login(env, request);
      }

      // 校验本地 token(记住登录):有效返回用户信息。
      if (url.pathname === "/api/me") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        const user = await authenticateUser(env, request);
        if (user instanceof Response) return json({ loggedIn: false }, 200);
        return json({ loggedIn: true, name: user.name }, 200);
      }

      // 在线会话心跳:客户端每 5s 一次;鉴权 + 检测强制下线/封禁 + 写节流。
      if (url.pathname === "/api/heartbeat" && request.method === "POST") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        return heartbeat(env, request);
      }

      // 在线用户列表(客户端视角):仅显示名/版本/时长,不含 username/IP(user_id)。版本门禁跳过——
      // 低版本客户端由心跳 426 兜底,这里重复拦截会导致客户端同时弹两个更新窗。
      if (url.pathname === "/api/online" && request.method === "GET") {
        return onlineClients(env, request);
      }

      // 操作许可:客户端每个用户操作运行前询问;默认放行,封禁/停用拒绝。走版本门禁(低于最低版本→426,
      // 客户端应弹更新窗而非放行)。
      if (url.pathname === "/api/operation/authorize" && request.method === "POST") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        return authorizeOperation(env, request);
      }

      // 使用日志:客户端批量上传操作记录(按 kind 分类存储)。走版本门禁(与 authorize 一致)。
      if (url.pathname === "/api/usage/logs" && request.method === "POST") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        return acceptUsageLogs(env, request);
      }

      if (url.pathname === "/api/rom") {
        const gate = await checkAppVersion(env, request);
        if (gate) return gate;
        const pd = url.searchParams.get("pd");
        const version = url.searchParams.get("version");
        if (!pd || !version) {
          return json({ error: "缺少 pd 或 version 查询参数。" }, 400);
        }
        if (!env.VOTA_API_TOKEN) {
          return json({ error: "服务端未配置 VOTA 凭据。" }, 500);
        }
        return resolveRom(env, pd, version, request);
      }

      return json({ error: "Not found" }, 404);
    } catch {
      return json({ error: "内部错误。" }, 500);
    }
  },

  /**
   * Cron 兜底:客户端全部崩溃/断网后,online_sessions 的 stale 行仍会残留到请求路径清理
   * 不再触发时。每几分钟定时清理一次,保证「崩溃后可靠过期」。
   */
  async scheduled(_event: ScheduledEvent, env: Env, _ctx: ExecutionContext): Promise<void> {
    await purgeStaleSessions(env, /* force */ true);
  },
};

async function resolveRom(env: Env, pd: string, version: string, request: Request): Promise<Response> {
  // 1. 强制登录:无 token / 无效 token → 401;封禁 → 403。
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);
  if (auth.banned) return json({ error: "账号已被封禁。" }, 403);
  const userId = auth.id;
  const userName = auth.name;

  // 2. 代理 VOTA。
  const baseUrl = env.VOTA_BASE_URL ?? "https://api.otau.cc.cd";
  const action = env.VOTA_ACTION ?? "resolve_url";
  const upstream = `${baseUrl}?action=${encodeURIComponent(action)}`;

  let resp: Response;
  try {
    resp = await fetch(upstream, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${env.VOTA_API_TOKEN}`,
      },
      body: JSON.stringify({ ver: env.VOTA_VER ?? "0.1.0", pd, version }),
    });
  } catch {
    await logAccess(env, userId, userName, pd, version, null, 502);
    return json({ error: "无法连接上游 ROM API。" }, 502);
  }

  const data = await resp.json().catch(() => null);
  if (!data || typeof data.ok !== "boolean") {
    await logAccess(env, userId, userName, pd, version, null, 502);
    return json({ error: "上游返回异常。" }, 502);
  }

  if (!data.ok) {
    const code = typeof data.code === "string" ? data.code : null;
    const error = typeof data.error === "string" ? data.error : "VOTA 未能解析 ROM 下载链接。";
    await logAccess(env, userId, userName, pd, version, null, mapError(code, error));
    return json({ error }, mapError(code, error));
  }

  const url = typeof data.url === "string" ? data.url : null;
  await logAccess(env, userId, userName, pd, version, url, 200);
  return json({
    pd,
    version,
    url,
    name: typeof data.name === "string" ? data.name : null,
    sizeBytes: typeof data.sizeBytes === "number" ? data.sizeBytes : null,
    sha256: typeof data.sha256 === "string" ? data.sha256 : null,
  }, 200);
}

/** 从 Authorization: Bearer 头解析 API 用户。无 token → null;token 无效/停用 → 401 Response。 */
async function authenticateUser(env: Env, request: Request): Promise<{ id: number; name: string; banned: boolean } | null | Response> {
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  if (!token) return null;

  const user = await env.DB
    .prepare("SELECT id, name, enabled, banned FROM api_users WHERE token = ?")
    .bind(token)
    .first<{ id: number; name: string; enabled: number; banned: number }>();
  if (!user || !user.enabled) return json({ error: "API token 无效或已停用。" }, 401);
  return { id: user.id, name: user.name, banned: user.banned === 1 };
}

/** 写一条访问日志(D1 失败不影响主流程)。 */
async function logAccess(
  env: Env,
  userId: number | null,
  userName: string | null,
  pd: string,
  version: string,
  url: string | null,
  status: number,
): Promise<void> {
  try {
    await env.DB
      .prepare(
        "INSERT INTO access_logs (api_user_id, api_user_name, pd, version, url, status) VALUES (?, ?, ?, ?, ?, ?)",
      )
      .bind(userId, userName, pd, version, url, status)
      .run();
  } catch {
    // 日志写失败不阻塞解析。
  }
}

/* ------------------------------------------------------------------ */
/* 在线会话心跳 + 在线列表 + 强制下线(数据在此侧写入/读取)                  */
/* 存储统一 INTEGER epoch 秒;upsert 只动 last_seen_at/client_version,      */
/* 绝不触碰 connected_at/user_id(时长基准与归属)。                          */
/* ------------------------------------------------------------------ */

/** per-token 心跳最小间隔(ms):换 sessionId 刷 D1 写配额的 DoS 防线。内存 Map,按 isolate 共享(Cloudflare 通常粘性路由)。 */
const HEARTBEAT_MIN_INTERVAL_MS = 3_000;
const HEARTBEAT_LIMIT_MAP_CAP = 10_000;
const heartbeatLimits = new Map<string, number>();
/** 请求路径内联清理的 per-isolate 节流;真正的兜底是 scheduled() Cron。 */
let lastPurgeAt = 0;

/** per-token 心跳限速:超频返回 false(调用方应跳过读写)。 */
function allowHeartbeat(token: string): boolean {
  const now = Date.now();
  const last = heartbeatLimits.get(token);
  if (last !== undefined && now - last < HEARTBEAT_MIN_INTERVAL_MS) return false;
  heartbeatLimits.set(token, now);
  if (heartbeatLimits.size > HEARTBEAT_LIMIT_MAP_CAP) heartbeatLimits.clear();
  return true;
}

/** 清理 stale 会话(超过在线窗口未心跳)。带 force_exit 的会话保留 24h,保证「踢后离线再回来」仍能收到 kick。 */
async function purgeStaleSessions(env: Env, force = false): Promise<void> {
  const now = Date.now();
  if (!force && now - lastPurgeAt < 60_000) return;
  lastPurgeAt = now;
  const timeoutSec = Math.floor((Number(env.ONLINE_TIMEOUT_MS) || 120_000) / 1000);
  const cutoff = Math.floor(now / 1000) - timeoutSec;
  const forceExitCutoff = Math.floor(now / 1000) - 24 * 3600;
  try {
    // 删除超过在线窗口未心跳的会话;但保留「已强制下线、客户端尚未回来确认」的行(24h 上限,防无限残留)。
    // 走 idx_online_last_seen 索引,仅触及 stale 行(每用户行数有上限)。
    await env.DB.prepare(
      "DELETE FROM online_sessions WHERE last_seen_at < ? AND (force_exit_at IS NULL OR force_exit_at < ?)",
    )
      .bind(cutoff, forceExitCutoff)
      .run();
  } catch {
    // 清理失败不影响心跳主流程。
  }
}

/** POST /api/heartbeat —— 客户端每 5s 一次。鉴权;返回是否应强制退出。 */
async function heartbeat(env: Env, request: Request): Promise<Response> {
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);
  if (auth.banned) return json({ ok: true, force_exit: true, reason: "账号已被封禁,请联系管理员。" }, 200);

  const body = await request.json().catch(() => null);
  const sessionId = typeof body?.sessionId === "string" ? body.sessionId.trim() : "";
  // 字符集白名单:sessionId 只允许 URL 安全字符,杜绝任意字符串进库(配合后台 XSS 修复,纵深防御)。
  if (!/^[A-Za-z0-9._:-]{1,64}$/.test(sessionId)) return json({ error: "sessionId 不合法。" }, 400);

  // goodbye(客户端正常/强制退出):删除会话行,绑定 user_id 防跨用户误删。
  if (body?.active === false) {
    await env.DB.prepare("DELETE FROM online_sessions WHERE session_id = ? AND user_id = ?")
      .bind(sessionId, auth.id)
      .run();
    return json({ ok: true, force_exit: false }, 200);
  }

  // per-token 限速只拦「写入」,不拦「读取」:被限速的心跳仍要读 force_exit,避免吞掉 kick 一轮。
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  const rateLimited = !allowHeartbeat(token);

  const now = Math.floor(Date.now() / 1000);
  const writeIntervalSec = Math.floor((Number(env.HEARTBEAT_WRITE_INTERVAL_MS) || 60_000) / 1000);
  const sessionCap = Number(env.ONLINE_SESSION_CAP) || 3;
  const clientVersion = typeof body?.clientVersion === "string" ? body.clientVersion.slice(0, 32) : "";
  // 仅展示用,绝不作鉴权依据(仅 Cloudflare 边缘覆写,不可伪造,但 wrangler dev 等环境可能被伪造)。
  const ip = request.headers.get("CF-Connecting-IP") || "";

  const row = await env.DB.prepare(
    "SELECT user_id, last_seen_at, force_exit_at, force_exit_reason FROM online_sessions WHERE session_id = ?",
  )
    .bind(sessionId)
    .first<{ user_id: number; last_seen_at: number; force_exit_at: number | null; force_exit_reason: string | null }>();

  if (!row) {
    // 新会话(或被裁掉的会话):若被限速则跳过建行(下个心跳再建),否则插入 + 清理多余 stale 行。
    if (rateLimited) {
      return json({ ok: true, force_exit: false }, 200);
    }

    // ON CONFLICT 仅当归属相同用户才更新(防并发竞态/跨用户篡改),且绝不触碰 connected_at。
    await env.DB.prepare(
      `INSERT INTO online_sessions (session_id, user_id, user_name, client_version, ip, connected_at, last_seen_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(session_id) DO UPDATE SET
         last_seen_at = excluded.last_seen_at,
         client_version = excluded.client_version
       WHERE online_sessions.user_id = excluded.user_id`,
    )
      .bind(sessionId, auth.id, auth.name, clientVersion, ip, now, now)
      .run();

    // 每用户会话数上限:只裁「已 stale」的多余行(不裁仍活跃的,避免 churn:活跃会话被裁后下次心跳
    // 又重建并再触发裁剪,cap 永达不到;配额防线主要靠 per-token 限速 + purge)。
    const staleCutoff = now - writeIntervalSec;
    await env.DB.prepare(
      `DELETE FROM online_sessions
       WHERE user_id = ? AND last_seen_at < ? AND session_id NOT IN (
         SELECT session_id FROM online_sessions WHERE user_id = ? ORDER BY last_seen_at DESC LIMIT ?
       )`,
    )
      .bind(auth.id, staleCutoff, auth.id, sessionCap)
      .run();

    await purgeStaleSessions(env);
    return json({ ok: true, force_exit: false }, 200);
  }

  // 会话已存在但归属其它用户 → 不触碰(防跨用户保活/伪造离线)。
  if (row.user_id !== auth.id) {
    return json({ ok: true, force_exit: false }, 200);
  }

  // 先判强制下线:被 kick 的会话不再刷新 last_seen(拒绝退出的客户端也不能靠心跳保活永不超时)。
  if (row.force_exit_at) {
    return json({ ok: true, force_exit: true, reason: row.force_exit_reason || "已被服务端强制下线。" }, 200);
  }

  // 写节流:距上次写 >= 间隔才写,且只动 last_seen_at/client_version/ip。被限速时跳过写入。
  if (!rateLimited && now - row.last_seen_at >= writeIntervalSec) {
    await env.DB
      .prepare("UPDATE online_sessions SET last_seen_at = ?, client_version = ?, ip = ? WHERE session_id = ?")
      .bind(now, clientVersion, ip, sessionId)
      .run();
  }

  return json({ ok: true, force_exit: false }, 200);
}

/** GET /api/online(客户端视角)—— 在线总数 + 各会话显示名/版本/时长,不含 username/IP/user_id。 */
async function onlineClients(env: Env, request: Request): Promise<Response> {
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);
  if (auth.banned) return json({ error: "账号已被封禁。" }, 403);

  const timeoutSec = Math.floor((Number(env.ONLINE_TIMEOUT_MS) || 120_000) / 1000);
  const cutoff = Math.floor(Date.now() / 1000) - timeoutSec;
  const rows = await env.DB.prepare(
    `SELECT user_id, user_name AS name, client_version, connected_at, last_seen_at
     FROM online_sessions
     WHERE last_seen_at >= ?
     ORDER BY last_seen_at DESC`,
  )
    .bind(cutoff)
    .all<{ user_id: number; name: string; client_version: string; connected_at: number; last_seen_at: number }>();

  const now = Math.floor(Date.now() / 1000);
  const sessions = rows.results.map((r) => ({
    name: r.name,
    client_version: r.client_version,
    connected_at: r.connected_at,
    last_seen_at: r.last_seen_at,
    duration_seconds: Math.max(0, now - r.connected_at),
    is_self: r.user_id === auth.id,
  }));

  await purgeStaleSessions(env);
  return json({ count: sessions.length, sessions }, 200);
}

/* ------------------------------------------------------------------ */
/* 操作许可门禁 + 使用日志                                                 */
/* ------------------------------------------------------------------ */

/** POST /api/operation/authorize —— 客户端每个用户操作运行前询问。默认放行;封禁/停用拒绝。 */
async function authorizeOperation(env: Env, request: Request): Promise<Response> {
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);

  if (auth.banned) {
    return json({ allowed: false, reason: "账号已被封禁,请联系管理员。" }, 200);
  }
  // disabled 用户由 authenticateUser 直接返回 401;此处再兜一层(理论上不会到这)。
  return json({ allowed: true }, 200);
}

/** POST /api/usage/logs —— 客户端批量上传使用日志;按 operation_kind 分类存储,绑定认证用户。event_key 幂等去重。 */
async function acceptUsageLogs(env: Env, request: Request): Promise<Response> {
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);
  if (auth.banned) return json({ error: "账号已被封禁。" }, 403);

  const body = await request.json().catch(() => null);
  const logs = Array.isArray(body?.logs) ? body.logs : [];
  if (logs.length === 0) return json({ ok: true, received: 0 }, 200);
  if (logs.length > 100) return json({ error: "单批日志最多 100 条。" }, 400);

  // 数字字段 NaN 保护:任一条 ended_at/duration_ms 非法绑定会让整批(原子)500 且客户端永久重试。
  const toInt = (v: unknown): number | null => {
    if (v == null) return null;
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  };

  const statement = env.DB.prepare(
    `INSERT INTO usage_logs (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at, ended_at, duration_ms)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
     ON CONFLICT(event_key) DO NOTHING`,
  );
  const batch = logs.map((log) =>
    statement.bind(
      auth.id,
      auth.name,
      String(log?.operation || "").slice(0, 32),
      String(log?.title || "").slice(0, 200),
      String(log?.status || "started").slice(0, 16),
      log?.event_id != null ? String(log.event_id).slice(0, 64) : null,
      Number(log?.started_at) || 0,
      toInt(log?.ended_at),
      toInt(log?.duration_ms),
    ),
  );

  try {
    await env.DB.batch(batch);
  } catch {
    return json({ error: "日志写入失败。" }, 500);
  }
  return json({ ok: true, received: logs.length }, 200);
}

/* ------------------------------------------------------------------ */
/* Nwflash 版本控制(强制更新)                                           */
/* ------------------------------------------------------------------ */

/** 版本号 "1.0.0" 式逐段比较:返回 <0 / 0 / >0。非法段按 0。 */
function compareVersions(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  const n = Math.max(pa.length, pb.length);
  for (let i = 0; i < n; i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

function parseVersion(v: string): number[] {
  return v.split(".").map((s) => {
    const n = Number.parseInt(s, 10);
    return Number.isFinite(n) ? n : 0;
  });
}

/** 请求携带的客户端版本(X-Nwflash-Version 头)。 */
function clientVersion(request: Request): string {
  return request.headers.get("X-Nwflash-Version")?.trim() || "";
}

/** 生效策略:启用的 app_versions 行中版本最高者;无启用行 → null。 */
async function getAppVersionPolicy(env: Env): Promise<{ version: string; min_version: string; download_url: string } | null> {
  const rows = await env.DB.prepare(
    "SELECT version, min_version, download_url FROM app_versions WHERE enabled = 1",
  ).all<{ version: string; min_version: string; download_url: string }>();
  let best: { version: string; min_version: string; download_url: string } | null = null;
  for (const row of rows.results) {
    if (!best || compareVersions(row.version, best.version) > 0) best = row;
  }
  return best;
}

/** GET /api/app/version?current=X —— 免登录,返回版本策略(桌面端启动拦截用)。 */
async function appVersion(env: Env, request: Request, url: URL): Promise<Response> {
  const policy = await getAppVersionPolicy(env);
  if (!policy) {
    return json({ latest: null, min: null, download_url: null, update_required: false, force_update: false }, 200);
  }
  const current = url.searchParams.get("current")?.trim() || clientVersion(request) || "0.0.0";
  return json({
    latest: policy.version,
    min: policy.min_version,
    download_url: policy.download_url,
    update_required: compareVersions(current, policy.version) < 0,
    force_update: compareVersions(current, policy.min_version) < 0,
  }, 200);
}

/** 版本门禁:当前版本低于最低版本 → 426;否则 null。所有请求统一调用。 */
async function checkAppVersion(env: Env, request: Request): Promise<Response | null> {
  const policy = await getAppVersionPolicy(env);
  if (!policy) return null;
  const current = clientVersion(request);
  if (current && compareVersions(current, policy.min_version) < 0) {
    return json({
      error: "请更新 Nwflash 到最新版本后继续使用。",
      code: "UPDATE_REQUIRED",
      latest: policy.version,
      min: policy.min_version,
      download_url: policy.download_url,
    }, 426);
  }
  return null;
}

/* ------------------------------------------------------------------ */
/* 桌面端登录                                                           */
/* ------------------------------------------------------------------ */

const PBKDF2_ITERATIONS = 100_000;

/** POST /api/login { username, password } → { ok, token, username, name }。 */
async function login(env: Env, request: Request): Promise<Response> {
  const body = await request.json().catch(() => null);
  const username = (body?.username as string || "").trim();
  const password = body?.password as string || "";
  if (!username || !password) return json({ error: "缺少用户名或密码。" }, 400);

  const user = await env.DB
    .prepare("SELECT * FROM api_users WHERE username = ?")
    .bind(username)
    .first<{ id: number; name: string; token: string; password: string | null; salt: string | null; enabled: number; banned: number }>();
  if (!user) return json({ error: "用户名或密码错误。" }, 401);
  if (user.banned === 1) return json({ error: "账号已被封禁,请联系管理员。" }, 401);
  if (user.enabled !== 1) return json({ error: "账号已被停用。" }, 401);
  if (!user.password || !user.salt) return json({ error: "该账号未设置密码,请联系管理员。" }, 401);

  const hash = await pbkdf2(password, user.salt);
  if (hash !== user.password) return json({ error: "用户名或密码错误。" }, 401);

  return json({ ok: true, token: user.token, username, name: user.name }, 200);
}

async function pbkdf2(password: string, saltHex: string): Promise<string> {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(password), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", salt: hexToBytes(saltHex), iterations: PBKDF2_ITERATIONS, hash: "SHA-256" },
    key,
    256,
  );
  return [...new Uint8Array(bits)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  return [...arr].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function mapError(code: string | null, error: string): number {
  if (code === "NOT_FOUND" || (code === null && /not found/i.test(error))) return 404;
  if (code === "AUTH_FAIL") return 401;
  if (code === "INSUFFICIENT_CREDITS") return 402;
  if (code === "FORBIDDEN") return 403;
  if (code === "RATE_LIMITED") return 429;
  return 502;
}

function json(obj: unknown, status: number): Response {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json", ...CORS },
  });
}
