/**
 * Cloudflare Worker —— Vivo ROM OTA 链接代理。
 * 复刻原 VivoKsu.Server 的接口:桌面应用带 PD + 版本号查询,服务端(Worker)持 VOTA 凭据,
 * 转发到 VOTA API(https://api.otau.cc.cd)取 OTA 下载链接,不向客户端暴露 token。
 *
 * 端点:
 *   GET /health                 -> { status, source }
 *   GET /api/rom?pd=X&version=Y -> { pd, version, url, name, sizeBytes, sha256 }
 *
 * 错误映射与 .NET 版一致:NOT_FOUND/not found->404, AUTH_FAIL->401, INSUFFICIENT_CREDITS->402,
 * FORBIDDEN->403, RATE_LIMITED->429, 其它->502。
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
  /** D1 绑定(nwflash-db,与 web.nwflash.cc.cd 共用):版本控制 + 访问日志。 */
  DB: D1Database;
}

const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET,POST,OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type, Authorization",
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

      if (url.pathname === "/api/rom") {
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
};

async function resolveRom(env: Env, pd: string, version: string, request: Request): Promise<Response> {
  // 1. 客户端认证(可选 API token):携带有效 token 记到对应用户;无效 token → 401;不带 → 匿名。
  const auth = await authenticateUser(env, request);
  if (auth instanceof Response) return auth;
  const userId = auth?.id ?? null;
  const userName = auth?.name ?? "匿名";

  // 2. 版本号控制:只允许后台「版本号控制」里启用的 PD+版本。
  const allowed = await env.DB
    .prepare("SELECT id FROM versions WHERE pd = ? AND version = ? AND enabled = 1")
    .bind(pd, version)
    .first();
  if (!allowed) {
    await logAccess(env, userId, userName, pd, version, null, 404);
    return json({ error: "该版本未授权或不存在。" }, 404);
  }

  // 3. 代理 VOTA。
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

/** 从 Authorization: Bearer 头解析 API 用户。无 token → null(匿名);token 无效/停用 → 401 Response。 */
async function authenticateUser(env: Env, request: Request): Promise<{ id: number; name: string } | null | Response> {
  const header = request.headers.get("Authorization") || "";
  const token = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  if (!token) return null;

  const user = await env.DB
    .prepare("SELECT id, name FROM api_users WHERE token = ? AND enabled = 1")
    .bind(token)
    .first<{ id: number; name: string }>();
  if (!user) return json({ error: "API token 无效或已停用。" }, 401);
  return user;
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
