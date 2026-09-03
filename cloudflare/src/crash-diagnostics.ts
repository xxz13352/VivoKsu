import { integrityIpHash } from "./security";

/**
 * 崩溃报告补传(P0)—— 客户端下次启动时把上次 panic(crash.log)上报服务端。
 *
 * 与 /api/integrity/report 同一套防滥用骨架(Content-Length 预检 + 流式读
 * 上限 + 严格闭集字段 + D1 claim/rate-limit 事务批处理 + event_id 幂等),
 * 但承载客户端 panic 文本与 backtrace(发送前客户端必须本地脱敏;服务端
 * 只做结构校验与长度上限,不再二次脱敏)。
 *
 * 匿名可报(崩溃可能发生在登出后);携带有效 bearer 时绑定 api_user_id
 * 并标记 trusted。错误映射:400 结构非法 / 413 超限 / 429 窗口配额 /
 * 202 首次接受 / 200 重复幂等 / 500 写入失败。
 */

/** 请求体上限(panic + backtrace 之和的硬顶;字符串字段另有独立上限)。 */
export const CRASH_MAX_BODY_BYTES = 65_536;
/** 每 IP 窗口内最多接受的崩溃报告数(崩溃补传正常每进程至多 1 条)。 */
export const CRASH_RATE_LIMIT = 5;
/** 限流窗口(秒):比 integrity 的 60s 宽,启动补传允许跨网络抖动重试。 */
export const CRASH_RATE_WINDOW_SECONDS = 600;

export class CrashBodyTooLargeError extends Error {}
export class InvalidCrashReportError extends Error {}

const CRASH_FIELDS = new Set([
  "event_id",
  "client_version",
  "build_id",
  "session_id",
  "panic_message",
  "backtrace",
  "occurred_at",
]);

export interface CrashReport {
  event_id: string;
  client_version: string;
  build_id: string;
  session_id: string;
  panic_message: string;
  backtrace: string;
  occurred_at: number;
}

export async function readCrashReport(request: Request): Promise<CrashReport> {
  const contentType = request.headers.get("Content-Type")?.split(";", 1)[0].trim().toLowerCase();
  if (contentType !== "application/json") throw new InvalidCrashReportError();

  const contentLength = request.headers.get("Content-Length");
  if (contentLength !== null) {
    const declared = Number(contentLength);
    if (!Number.isSafeInteger(declared) || declared < 0) throw new InvalidCrashReportError();
    if (declared > CRASH_MAX_BODY_BYTES) throw new CrashBodyTooLargeError();
  }

  if (!request.body) throw new InvalidCrashReportError();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > CRASH_MAX_BODY_BYTES) {
      await reader.cancel();
      throw new CrashBodyTooLargeError();
    }
    chunks.push(value);
  }

  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new InvalidCrashReportError();
  } finally {
    bytes.fill(0);
  }
  return validateCrashReport(parsed);
}

function validateCrashReport(value: unknown): CrashReport {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new InvalidCrashReportError();
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== CRASH_FIELDS.size || keys.some((key) => !CRASH_FIELDS.has(key))) {
    throw new InvalidCrashReportError();
  }
  if (typeof record.event_id !== "string" || !/^[A-Za-z0-9._:-]{1,64}$/.test(record.event_id)) {
    throw new InvalidCrashReportError();
  }
  if (
    typeof record.client_version !== "string"
    || !/^[A-Za-z0-9][A-Za-z0-9._+-]{0,31}$/.test(record.client_version)
  ) {
    throw new InvalidCrashReportError();
  }
  if (typeof record.build_id !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/.test(record.build_id)) {
    throw new InvalidCrashReportError();
  }
  if (typeof record.session_id !== "string" || !/^[A-Za-z0-9._:-]{1,64}$/.test(record.session_id)) {
    throw new InvalidCrashReportError();
  }
  if (
    typeof record.panic_message !== "string"
    || record.panic_message.length === 0
    || record.panic_message.length > 16_384
  ) {
    throw new InvalidCrashReportError();
  }
  if (typeof record.backtrace !== "string" || record.backtrace.length > 32_768) {
    throw new InvalidCrashReportError();
  }
  if (!Number.isSafeInteger(record.occurred_at) || Number(record.occurred_at) <= 0) {
    throw new InvalidCrashReportError();
  }
  return record as unknown as CrashReport;
}

/** 与 integrity 上报一致的窗口起点对齐(整窗对齐,而非滑动)。 */
export function crashWindowStart(nowSeconds: number): number {
  const window = CRASH_RATE_WINDOW_SECONDS;
  return Math.floor(nowSeconds / window) * window;
}

export async function crashIpHash(ip: string): Promise<string> {
  return integrityIpHash(ip);
}

/**
 * 持久化一条崩溃报告(claim → 计数 → 写入 → 清 claim 的单事务批处理)。
 * 返回 {accepted}(首次)/ {duplicate}(并发或重试幂等)/ {rateLimited}
 * (IP 窗口配额已满);批处理不一致时抛错由上层 500。
 */
export async function storeCrashReport(
  db: D1Database,
  report: CrashReport,
  options: { apiUserId: number | null; trusted: boolean; ipHash: string; windowStart: number; now: number },
): Promise<{ accepted: boolean; duplicate: boolean; rateLimited: boolean }> {
  const claimToken = crypto.randomUUID();
  const results = await db.batch([
    db.prepare(
      `INSERT INTO crash_report_claims (event_id, claim_token, created_at)
       SELECT ?, ?, ?
       WHERE NOT EXISTS (SELECT 1 FROM crash_reports WHERE event_id = ?)
       ON CONFLICT(event_id) DO NOTHING
       RETURNING claim_token`,
    ).bind(report.event_id, claimToken, options.now, report.event_id),
    db.prepare(
      `INSERT INTO crash_report_rate_limits (ip_hash, window_start, count, last_event_id)
       SELECT ?, ?, 1, ?
       WHERE EXISTS (
         SELECT 1 FROM crash_report_claims
         WHERE event_id = ? AND claim_token = ?
       )
       ON CONFLICT(ip_hash, window_start) DO UPDATE SET
         count = CASE
           WHEN crash_report_rate_limits.last_event_id = excluded.last_event_id
             THEN crash_report_rate_limits.count
           ELSE crash_report_rate_limits.count + 1
         END,
         last_event_id = excluded.last_event_id
       WHERE EXISTS (
         SELECT 1 FROM crash_report_claims
         WHERE event_id = ? AND claim_token = ?
       )
       RETURNING count`,
    ).bind(
      options.ipHash,
      options.windowStart,
      report.event_id,
      report.event_id,
      claimToken,
      report.event_id,
      claimToken,
    ),
    db.prepare(
      `INSERT INTO crash_reports
         (event_id, api_user_id, trusted, client_version, build_id, session_id,
          panic_message, backtrace, occurred_at)
       SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?
       WHERE EXISTS (
         SELECT 1 FROM crash_report_claims
         WHERE event_id = ? AND claim_token = ?
       )
         AND COALESCE((
           SELECT count FROM crash_report_rate_limits WHERE ip_hash = ? AND window_start = ?
         ), ?) <= ?
       ON CONFLICT(event_id) DO NOTHING
       RETURNING event_id`,
    ).bind(
      report.event_id,
      options.apiUserId,
      options.trusted ? 1 : 0,
      report.client_version,
      report.build_id,
      report.session_id,
      report.panic_message,
      report.backtrace,
      report.occurred_at,
      report.event_id,
      claimToken,
      options.ipHash,
      options.windowStart,
      CRASH_RATE_LIMIT + 1,
      CRASH_RATE_LIMIT,
    ),
    db.prepare(
      `DELETE FROM crash_report_claims
       WHERE event_id = ? AND claim_token = ?
       RETURNING event_id`,
    ).bind(report.event_id, claimToken),
  ]);

  const claimed = (results[0] as D1Result<{ claim_token: string }>).results[0];
  if (claimed) {
    const count = (results[1] as D1Result<{ count: number }>).results[0]?.count;
    const inserted = (results[2] as D1Result<{ event_id: string }>).results[0];
    const cleaned = (results[3] as D1Result<{ event_id: string }>).results[0];
    if (!cleaned || typeof count !== "number") {
      throw new Error("crash claim transaction did not clean its owner claim");
    }
    if (count <= CRASH_RATE_LIMIT && inserted) return { accepted: true, duplicate: false, rateLimited: false };
    if (count > CRASH_RATE_LIMIT && !inserted) {
      return { accepted: false, duplicate: false, rateLimited: true };
    }
    throw new Error("crash claim transaction returned an inconsistent outcome");
  }

  const existing = await db
    .prepare("SELECT event_id FROM crash_reports WHERE event_id = ?")
    .bind(report.event_id)
    .first<{ event_id: string }>();
  if (existing) return { accepted: true, duplicate: true, rateLimited: false };
  throw new Error("crash claim lost without a durable accepted report");
}

/** Cron 清理:过窗限流行 + 90 天前的崩溃报告。 */
export async function purgeExpiredCrashData(db: D1Database, nowMs: number): Promise<void> {
  const now = Math.floor(nowMs / 1000);
  const rateCutoff = now - 2 * CRASH_RATE_WINDOW_SECONDS;
  const reportCutoff = now - 90 * 24 * 3600;
  try {
    await db.batch([
      db.prepare("DELETE FROM crash_report_rate_limits WHERE window_start < ?").bind(rateCutoff),
      db.prepare("DELETE FROM crash_report_claims WHERE created_at < ?").bind(rateCutoff),
      db.prepare("DELETE FROM crash_reports WHERE occurred_at < ?").bind(reportCutoff),
    ]);
  } catch {
    // 清理失败不影响 Cron 主流程;下一次调度会重试。
  }
}
