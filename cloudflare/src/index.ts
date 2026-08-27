import {
  INTEGRITY_RATE_LIMIT,
  INTEGRITY_RATE_WINDOW_SECONDS,
  LEASE_TTL_SECONDS,
  IntegrityBodyTooLargeError,
  InvalidIntegrityReportError,
  SigningConfigurationError,
  createPinset,
  integrityIpHash,
  readIntegrityReport,
  signLease,
  signPinset,
  tokenSha256,
  type LeaseClaims,
} from "./security";
import { ingestTraceUploadV2, traceErrorV2 } from "./trace-v2-ingest";

/**
 * Cloudflare Worker —— Vivo ROM OTA 链接代理 + Nwflash 版本门禁。
 * 桌面应用带 PD + 版本号查询,Worker 持 VOTA 凭据,
 * 转发到 VOTA API(https://api.otau.cc.cd)取 OTA 下载链接,不向客户端暴露 token。
 *
 * 端点:
 *   GET /health                          -> { status, source }
 *   GET /api/app/version?current=X       -> Nwflash 版本策略(免登录,启动拦截用)
 *   GET /api/security/pins               -> Ed25519 签名双 SPKI pin 清单
 *   POST /api/integrity/report           -> 严格限长/限流/幂等完整性遥测
 *   POST /api/heartbeat                  -> 在线会话心跳 + 递增签名租约
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
  /** Ed25519 PKCS#8 DER 的无填充 base64url(worker secret)。缺失或无效时签名端点失败关闭。 */
  SESSION_SIGNING_PRIVATE_KEY_PKCS8?: string;
  /** 上游 VOTA 基地址,默认 https://api.otau.cc.cd。 */
  VOTA_BASE_URL?: string;
  /** 调用 action:resolve_url(OTA,-1 信用点)/ resolve_flash_url(线刷,-3)。默认 resolve_url。 */
  VOTA_ACTION?: string;
  /** 平台版本白名单,默认 0.1.0。 */
  VOTA_VER?: string;
  /** D1 绑定(nwflash-db,与 web.nwflash.cc.cd 共用):访问/版本/在线会话 + 完整性事件/限流。 */
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

      if (url.pathname === "/api/security/pins" && request.method === "GET") {
        return securityPins(env);
      }

      if (url.pathname === "/api/integrity/report" && request.method === "POST") {
        return await acceptIntegrityReport(env, request);
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
        if (user instanceof Response || user === null) return json({ loggedIn: false }, 200);
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

      if (url.pathname === "/api/usage/traces/v2" && request.method === "POST") {
        const gate = await checkAppVersion(env, request);
        if (gate) return withNoStore(gate);
        const auth = await authenticateUser(env, request);
        if (auth instanceof Response) {
          return traceErrorV2(401, "TRACE_UNAUTHORIZED", "API token 无效或已停用。");
        }
        if (auth === null) return traceErrorV2(401, "TRACE_UNAUTHORIZED", "请先登录。");
        if (auth.banned) return traceErrorV2(403, "TRACE_FORBIDDEN", "账号已被封禁。");
        const bearerToken = request.headers.get("Authorization")?.slice(7).trim() ?? "";
        return ingestTraceUploadV2(env, request, { ...auth, bearer_token: bearerToken });
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
      if (url.pathname === "/api/usage/traces/v2" && request.method === "POST") {
        return traceErrorV2(500, "TRACE_INTERNAL", "日志写入失败。");
      }
      return json({ error: "内部错误。" }, 500);
    }
  },

  /**
   * Cron 兜底:客户端全部崩溃/断网后,online_sessions 的 stale 行仍会残留到请求路径清理
   * 不再触发时。每几分钟定时清理一次,保证「崩溃后可靠过期」。
   */
  async scheduled(_event: ScheduledEvent, env: Env, _ctx: ExecutionContext): Promise<void> {
    await purgeStaleSessions(env, /* force */ true);
    await purgeIntegrityRateLimits(env);
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

  const data = await resp.json().catch(() => null) as Record<string, unknown> | null;
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
async function authenticateUser(
  env: Env,
  request: Request,
): Promise<{ id: number; username: string; name: string; banned: boolean } | null | Response> {
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  if (!token) return null;

  const user = await env.DB
    .prepare("SELECT id, username, name, enabled, banned FROM api_users WHERE token = ?")
    .bind(token)
    .first<{ id: number; username: string; name: string; enabled: number; banned: number }>();
  if (!user || !user.enabled) return json({ error: "API token 无效或已停用。" }, 401);
  return { id: user.id, username: user.username, name: user.name, banned: user.banned === 1 };
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
/* 签名 pin 清单 + 最小完整性遥测                                      */
/* ------------------------------------------------------------------ */

async function securityPins(env: Env): Promise<Response> {
  const now = Math.floor(Date.now() / 1000);
  try {
    return json(await signPinset(createPinset(now), env.SESSION_SIGNING_PRIVATE_KEY_PKCS8), 200);
  } catch (error) {
    if (error instanceof SigningConfigurationError) return signingUnavailable();
    throw error;
  }
}

async function acceptIntegrityReport(env: Env, request: Request): Promise<Response> {
  let report;
  try {
    report = await readIntegrityReport(request);
  } catch (error) {
    if (error instanceof IntegrityBodyTooLargeError) return json({ error: "请求体过大。" }, 413);
    if (error instanceof InvalidIntegrityReportError) return json({ error: "完整性事件不合法。" }, 400);
    throw error;
  }

  const authHeader = request.headers.get("Authorization");
  let userId: number | null = null;
  let trusted = 0;
  if (authHeader !== null) {
    const auth = await authenticateUser(env, request);
    if (auth instanceof Response) return auth;
    if (auth === null) return json({ error: "API token 无效或已停用。" }, 401);
    userId = auth.id;
    trusted = 1;
  }

  const now = Math.floor(Date.now() / 1000);
  const windowStart = Math.floor(now / INTEGRITY_RATE_WINDOW_SECONDS) * INTEGRITY_RATE_WINDOW_SECONDS;
  const ipHash = await integrityIpHash(request.headers.get("CF-Connecting-IP") || "unknown");
  const claimToken = crypto.randomUUID();
  const results = await env.DB.batch([
    env.DB.prepare(
      `INSERT INTO integrity_event_claims (event_id, claim_token, created_at)
       SELECT ?, ?, ?
       WHERE NOT EXISTS (SELECT 1 FROM integrity_events WHERE event_id = ?)
       ON CONFLICT(event_id) DO NOTHING
       RETURNING claim_token`,
    ).bind(report.event_id, claimToken, now, report.event_id),
    env.DB.prepare(
      `INSERT INTO integrity_rate_limits (ip_hash, window_start, count, last_event_id)
       SELECT ?, ?, 1, ?
       WHERE EXISTS (
         SELECT 1 FROM integrity_event_claims
         WHERE event_id = ? AND claim_token = ?
       )
       ON CONFLICT(ip_hash, window_start) DO UPDATE SET
         count = CASE
           WHEN integrity_rate_limits.last_event_id = excluded.last_event_id
             THEN integrity_rate_limits.count
           ELSE integrity_rate_limits.count + 1
         END,
         last_event_id = excluded.last_event_id
       WHERE EXISTS (
         SELECT 1 FROM integrity_event_claims
         WHERE event_id = ? AND claim_token = ?
       )
       RETURNING count`,
    ).bind(ipHash, windowStart, report.event_id, report.event_id, claimToken, report.event_id, claimToken),
    env.DB.prepare(
      `INSERT INTO integrity_events
         (event_id, api_user_id, trusted, phase, reason, client_version, build_id, occurred_at)
       SELECT ?, ?, ?, ?, ?, ?, ?, ?
       WHERE EXISTS (
         SELECT 1 FROM integrity_event_claims
         WHERE event_id = ? AND claim_token = ?
       )
         AND COALESCE((
           SELECT count FROM integrity_rate_limits WHERE ip_hash = ? AND window_start = ?
         ), ?) <= ?
       ON CONFLICT(event_id) DO NOTHING
       RETURNING event_id`,
    ).bind(
      report.event_id,
      userId,
      trusted,
      report.phase,
      report.reason,
      report.client_version,
      report.build_id,
      report.occurred_at,
      report.event_id,
      claimToken,
      ipHash,
      windowStart,
      INTEGRITY_RATE_LIMIT + 1,
      INTEGRITY_RATE_LIMIT,
    ),
    env.DB.prepare(
      `DELETE FROM integrity_event_claims
       WHERE event_id = ? AND claim_token = ?
       RETURNING event_id`,
    ).bind(report.event_id, claimToken),
  ]);

  const claimed = (results[0] as D1Result<{ claim_token: string }>).results[0];
  if (claimed) {
    const count = (results[1] as D1Result<{ count: number }>).results[0]?.count;
    const inserted = (results[2] as D1Result<{ event_id: string }>).results[0];
    const cleaned = (results[3] as D1Result<{ event_id: string }>).results[0];
    if (!cleaned || typeof count !== "number") throw new Error("integrity claim transaction did not clean its owner claim");
    if (count <= INTEGRITY_RATE_LIMIT && inserted) return json({ ok: true, accepted: true }, 202);
    if (count > INTEGRITY_RATE_LIMIT && !inserted) return json({ error: "完整性事件上报过于频繁。" }, 429);
    throw new Error("integrity claim transaction returned an inconsistent outcome");
  }

  const accepted = await env.DB.prepare(
    "SELECT event_id FROM integrity_events WHERE event_id = ?",
  )
    .bind(report.event_id)
    .first<{ event_id: string }>();
  if (accepted) return json({ ok: true, duplicate: true }, 200);
  throw new Error("integrity claim lost without a durable accepted event");
}

async function purgeIntegrityRateLimits(env: Env): Promise<void> {
  const cutoff = Math.floor(Date.now() / 1000) - 2 * INTEGRITY_RATE_WINDOW_SECONDS;
  try {
    await env.DB.prepare("DELETE FROM integrity_rate_limits WHERE window_start < ?").bind(cutoff).run();
  } catch {
    // 遥测限流清理失败不影响在线会话 Cron;后续 Cron 会再次尝试。
  }
}

function signingUnavailable(): Response {
  return json({ error: "签名服务不可用。" }, 503);
}

function isBoundIdentifier(value: string, maxLength: number): boolean {
  return value.length <= maxLength && /^[A-Za-z0-9._:-]+$/.test(value);
}

function isClientVersion(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._+-]{0,31}$/.test(value);
}

/* ------------------------------------------------------------------ */
/* 在线会话心跳 + 在线列表 + 强制下线(数据在此侧写入/读取)                  */
/* 存储统一 INTEGER epoch 秒;upsert 只动 last_seen_at/client_version,      */
/* 绝不触碰 connected_at/user_id(时长基准与归属)。                          */
/* ------------------------------------------------------------------ */

/** 每个 API 用户的活动心跳最小间隔。由 session_leases 的关联 D1 CAS 谓词全局执行。 */
const HEARTBEAT_MIN_INTERVAL_SECONDS = 3;
/** 请求路径内联清理的 per-isolate 节流;真正的兜底是 scheduled() Cron。 */
let lastPurgeAt = 0;

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
    await env.DB.prepare(
      `DELETE FROM session_leases
       WHERE updated_at < ? AND session_id NOT IN (
         SELECT session_id FROM online_sessions WHERE force_exit_at IS NOT NULL AND force_exit_at >= ?
       )`,
    )
      .bind(cutoff, forceExitCutoff)
      .run();
  } catch {
    // 清理失败不影响心跳主流程。
  }
}

/** POST /api/heartbeat —— 只有 D1 中登录创建的完整绑定与当前序号可刷新签名租约。 */
async function heartbeat(env: Env, request: Request): Promise<Response> {
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  if (auth === null) return json({ error: "请先登录。" }, 401);
  if (auth.banned) return json({ ok: true, force_exit: true, reason: "账号已被封禁,请联系管理员。" }, 200);

  const body = await request.json().catch(() => null) as Record<string, unknown> | null;
  const rawSessionId = body?.session_id ?? body?.sessionId;
  const sessionId = typeof rawSessionId === "string" ? rawSessionId.trim() : "";
  if (!/^[A-Za-z0-9._:-]{1,64}$/.test(sessionId)) return json({ error: "sessionId 不合法。" }, 400);

  // goodbye 不创建能力;两个删除都绑定 user_id,避免跨用户清理。
  if (body?.active === false) {
    await env.DB.batch([
      env.DB.prepare("DELETE FROM session_leases WHERE session_id = ? AND user_id = ?").bind(sessionId, auth.id),
      env.DB.prepare("DELETE FROM online_sessions WHERE session_id = ? AND user_id = ?").bind(sessionId, auth.id),
    ]);
    return json({ ok: true, force_exit: false }, 200);
  }

  const rawClientVersion = body?.client_version ?? body?.clientVersion;
  const clientVersion = typeof rawClientVersion === "string" ? rawClientVersion.trim() : "";
  const buildId = typeof body?.build_id === "string" ? body.build_id.trim() : "";
  const processNonce = typeof body?.process_nonce === "string" ? body.process_nonce.trim() : "";
  const sequence = body?.sequence;
  if (!isClientVersion(clientVersion)) return json({ error: "client_version 不合法。" }, 400);
  if (!isBoundIdentifier(buildId, 128)) return json({ error: "build_id 不合法。" }, 400);
  if (!isBoundIdentifier(processNonce, 128)) return json({ error: "process_nonce 不合法。" }, 400);
  if (typeof sequence !== "number" || !Number.isSafeInteger(sequence) || sequence < 1 || sequence >= Number.MAX_SAFE_INTEGER) {
    return json({ error: "sequence 不合法。" }, 400);
  }

  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  const issuedAt = Math.floor(Date.now() / 1000);
  const nextSequence = sequence + 1;
  const claims: LeaseClaims = {
    version: 1,
    kind: "heartbeat",
    username: auth.username,
    token_sha256: await tokenSha256(token),
    client_version: clientVersion,
    build_id: buildId,
    process_nonce: processNonce,
    session_id: sessionId,
    sequence: nextSequence,
    issued_at: issuedAt,
    expires_at: issuedAt + LEASE_TTL_SECONDS,
  };

  let envelope;
  try {
    envelope = await signLease(claims, env.SESSION_SIGNING_PRIVATE_KEY_PKCS8);
  } catch (error) {
    if (error instanceof SigningConfigurationError) return signingUnavailable();
    throw error;
  }

  const advanced = await env.DB.prepare(
    `UPDATE session_leases SET sequence = ?, updated_at = ?, last_heartbeat_at = ?
     WHERE session_id = ? AND user_id = ? AND username = ?
       AND client_version = ? AND build_id = ? AND process_nonce = ? AND sequence = ?
       AND NOT EXISTS (
         SELECT 1 FROM session_leases recent_heartbeat
         WHERE recent_heartbeat.user_id = session_leases.user_id
           AND recent_heartbeat.last_heartbeat_at > ?
       )
       AND NOT EXISTS (
          SELECT 1 FROM online_sessions
          WHERE online_sessions.session_id = session_leases.session_id
            AND (online_sessions.user_id <> session_leases.user_id OR force_exit_at IS NOT NULL)
       )
     RETURNING sequence`,
  )
    .bind(
      nextSequence,
      issuedAt,
      issuedAt,
      sessionId,
      auth.id,
      auth.username,
      clientVersion,
      buildId,
      processNonce,
      sequence,
      issuedAt - HEARTBEAT_MIN_INTERVAL_SECONDS,
    )
    .first<{ sequence: number }>();
  if (!advanced) {
    const state = await env.DB.prepare(
      `SELECT sl.user_id, sl.username, sl.client_version, sl.build_id, sl.process_nonce,
              sl.sequence,
              (SELECT MAX(recent_heartbeat.last_heartbeat_at)
               FROM session_leases recent_heartbeat
               WHERE recent_heartbeat.user_id = sl.user_id) AS user_last_heartbeat_at,
              os.user_id AS online_user_id,
              os.force_exit_at, os.force_exit_reason
       FROM session_leases sl
       LEFT JOIN online_sessions os ON os.session_id = sl.session_id
       WHERE sl.session_id = ?`,
    )
      .bind(sessionId)
      .first<{
        user_id: number;
        username: string;
        client_version: string;
        build_id: string;
        process_nonce: string;
        sequence: number;
        user_last_heartbeat_at: number | null;
        online_user_id: number | null;
        force_exit_at: number | null;
        force_exit_reason: string | null;
      }>();
    const bindingMatches = state
      && state.user_id === auth.id
      && state.username === auth.username
      && state.client_version === clientVersion
      && state.build_id === buildId
      && state.process_nonce === processNonce
      && (state.online_user_id === null || state.online_user_id === auth.id);
    if (!bindingMatches) return leaseConflict();
    if (state.force_exit_at !== null) {
      return json({ ok: true, force_exit: true, reason: state.force_exit_reason || "已被服务端强制下线。" }, 200);
    }
    if (state.sequence !== sequence) return leaseConflict();
    if (
      state.user_last_heartbeat_at !== null
      && state.user_last_heartbeat_at > issuedAt - HEARTBEAT_MIN_INTERVAL_SECONDS
    ) return json({ error: "心跳过于频繁。" }, 429);
    return leaseConflict();
  }

  await projectOnlineSession(env, request, auth, sessionId, clientVersion, issuedAt);
  return json({ ok: true, force_exit: false, ...envelope }, 200);
}

function leaseConflict(): Response {
  return json({ error: "会话租约无效、冲突或已过期。" }, 409);
}

async function projectOnlineSession(
  env: Env,
  request: Request,
  auth: { id: number; name: string },
  sessionId: string,
  clientVersion: string,
  now: number,
): Promise<void> {
  const writeIntervalSec = Math.floor((Number(env.HEARTBEAT_WRITE_INTERVAL_MS) || 60_000) / 1000);
  const sessionCap = Number(env.ONLINE_SESSION_CAP) || 3;
  const ip = request.headers.get("CF-Connecting-IP") || "";
  try {
    const online = await env.DB.prepare(
      "SELECT user_id, last_seen_at FROM online_sessions WHERE session_id = ?",
    )
      .bind(sessionId)
      .first<{ user_id: number; last_seen_at: number }>();
    if (online && online.user_id !== auth.id) return;
    if (!online) {
      await env.DB.prepare(
        `INSERT INTO online_sessions (session_id, user_id, user_name, client_version, ip, connected_at, last_seen_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id) DO NOTHING`,
      )
        .bind(sessionId, auth.id, auth.name, clientVersion, ip, now, now)
        .run();
    } else if (now - online.last_seen_at >= writeIntervalSec) {
      await env.DB.prepare(
        "UPDATE online_sessions SET last_seen_at = ?, client_version = ?, ip = ? WHERE session_id = ? AND user_id = ?",
      )
        .bind(now, clientVersion, ip, sessionId, auth.id)
        .run();
    }
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
  } catch {
    // 在线展示投影失败不回滚已成功的安全 CAS;下次有效心跳会重试投影。
  }
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

  const body = await request.json().catch(() => null) as Record<string, unknown> | null;
  const logs = Array.isArray(body?.logs) ? body.logs as Array<Record<string, unknown> | null> : [];
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

/** POST /api/login —— 密码验证成功后返回 token 与绑定当前进程/会话的短期签名租约。 */
async function login(env: Env, request: Request): Promise<Response> {
  const body = await request.json().catch(() => null) as Record<string, unknown> | null;
  const username = (body?.username as string || "").trim();
  const password = body?.password as string || "";
  if (!username || !password) return json({ error: "缺少用户名或密码。" }, 400);

  const clientVersion = typeof body?.client_version === "string" ? body.client_version.trim() : "";
  const buildId = typeof body?.build_id === "string" ? body.build_id.trim() : "";
  const processNonce = typeof body?.process_nonce === "string" ? body.process_nonce.trim() : "";
  const sessionId = typeof body?.session_id === "string" ? body.session_id.trim() : "";
  if (!isClientVersion(clientVersion)) return json({ error: "client_version 不合法。" }, 400);
  if (!isBoundIdentifier(buildId, 128)) return json({ error: "build_id 不合法。" }, 400);
  if (!isBoundIdentifier(processNonce, 128)) return json({ error: "process_nonce 不合法。" }, 400);
  if (!isBoundIdentifier(sessionId, 64)) return json({ error: "session_id 不合法。" }, 400);

  const user = await env.DB
    .prepare("SELECT * FROM api_users WHERE username = ?")
    .bind(username)
    .first<{ id: number; username: string; name: string; token: string; password: string | null; salt: string | null; enabled: number; banned: number }>();
  if (!user) return json({ error: "用户名或密码错误。" }, 401);
  if (user.banned === 1) return json({ error: "账号已被封禁,请联系管理员。" }, 401);
  if (user.enabled !== 1) return json({ error: "账号已被停用。" }, 401);
  if (!user.password || !user.salt) return json({ error: "该账号未设置密码,请联系管理员。" }, 401);

  const hash = await pbkdf2(password, user.salt);
  if (hash !== user.password) return json({ error: "用户名或密码错误。" }, 401);

  const issuedAt = Math.floor(Date.now() / 1000);
  const claims: LeaseClaims = {
    version: 1,
    kind: "login",
    username: user.username,
    token_sha256: await tokenSha256(user.token),
    client_version: clientVersion,
    build_id: buildId,
    process_nonce: processNonce,
    session_id: sessionId,
    sequence: 1,
    issued_at: issuedAt,
    expires_at: issuedAt + LEASE_TTL_SECONDS,
  };
  try {
    const envelope = await signLease(claims, env.SESSION_SIGNING_PRIVATE_KEY_PKCS8);
    const claimed = await env.DB.prepare(
      `INSERT INTO session_leases
         (session_id, user_id, username, client_version, build_id, process_nonce, sequence, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)
       ON CONFLICT(session_id) DO NOTHING
       RETURNING sequence`,
    )
      .bind(sessionId, user.id, user.username, clientVersion, buildId, processNonce, issuedAt, issuedAt)
      .first<{ sequence: number }>();
    if (!claimed) return json({ error: "session_id 已被占用。" }, 409);
    return json({ ok: true, token: user.token, username: user.username, name: user.name, ...envelope }, 200);
  } catch (error) {
    if (error instanceof SigningConfigurationError) return signingUnavailable();
    throw error;
  }
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

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
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

function withNoStore(response: Response): Response {
  const headers = new Headers(response.headers);
  headers.set("Cache-Control", "no-store");
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}
