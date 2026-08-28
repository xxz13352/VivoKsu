import {
  decodeTraceCursorV2,
  encodeTraceCursorV2,
  TraceValidationError,
  type KeysetPageV2,
  type RomLogAdminRowV2,
  type TraceApiErrorCodeV2,
  type TraceCursorV2,
  type TraceEventDetailV2,
  type TraceEventV2,
  type TraceOutputChunkV2,
  type TraceOutputPageV2,
  type TraceOutputStreamV2,
  type TraceOverviewV2,
  type TraceRunDetailV2,
  type TraceRunSummaryV2,
  type TraceUserSummaryV2,
} from "../../src/trace-v2-contract";

export interface Env {
  DB: D1Database;
  ONLINE_TIMEOUT_MS?: string;
}

export interface AdminIdentity {
  id: number;
  username: string;
}

const DEFAULT_LIMIT = 50;
const MAX_LIMIT = 200;
const EXPORT_BATCH_LIMIT = 200;
const MAX_LIKE_PATTERN_BYTES = 50;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TRACE_OUTCOMES = new Set(["running", "success", "failed", "canceled", "denied", "aborted", "unknown"]);

export const ADMIN_RESPONSE_SECURITY_HEADERS: Readonly<Record<string, string>> = Object.freeze({
  "Cache-Control": "no-store",
  "Content-Security-Policy":
    "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; "
    + "object-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
    + "font-src 'self'; connect-src 'self'",
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Resource-Policy": "same-origin",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  "Referrer-Policy": "no-referrer",
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
});

const RESPONSE_HEADERS: Record<string, string> = {
  ...ADMIN_RESPONSE_SECURITY_HEADERS,
  "Content-Type": "application/json; charset=utf-8",
};

const COMBINED_RUNS_CTE = `
WITH combined_runs AS (
  SELECT
    2 AS source_schema,
    'v2:' || r.run_id AS trace_ref,
    r.run_id AS run_id,
    NULL AS legacy_id,
    r.api_user_id AS user_id,
    u.username AS username,
    COALESCE(r.api_user_name, u.name) AS user_name,
    r.operation_kind AS operation_kind,
    r.title AS title,
    r.outcome AS outcome,
    r.client_version AS client_version,
    r.started_at_ms AS started_at_ms,
    r.ended_at_ms AS ended_at_ms,
    r.duration_ms AS duration_ms,
    r.trace_complete AS trace_complete,
    r.trace_loss_reason AS trace_loss_reason,
    printf(
      '00000000-0000-7%s-9%s-00%s',
      substr(printf('%016x', r.rowid), 1, 3),
      substr(printf('%016x', r.rowid), 4, 3),
      substr(printf('%016x', r.rowid), 7, 10)
    ) AS sort_run_id,
    r.device_serial AS device_serial,
    r.source_paths_json AS source_paths_json,
    r.source_urls_json AS source_urls_json,
    r.error_code AS error_code
  FROM usage_operation_runs AS r
  LEFT JOIN api_users AS u ON u.id = r.api_user_id

  UNION ALL

  SELECT
    1 AS source_schema,
    'v1:' || CAST(l.id AS TEXT) AS trace_ref,
    NULL AS run_id,
    l.id AS legacy_id,
    l.api_user_id AS user_id,
    u.username AS username,
    COALESCE(l.api_user_name, u.name) AS user_name,
    l.operation_kind AS operation_kind,
    COALESCE(l.title, '') AS title,
    CASE LOWER(l.status)
      WHEN 'started' THEN 'running'
      WHEN 'running' THEN 'running'
      WHEN 'success' THEN 'success'
      WHEN 'failed' THEN 'failed'
      WHEN 'canceled' THEN 'canceled'
      WHEN 'denied' THEN 'denied'
      WHEN 'aborted' THEN 'aborted'
      ELSE 'unknown'
    END AS outcome,
    '' AS client_version,
    l.started_at * 1000 AS started_at_ms,
    CASE WHEN l.ended_at IS NULL THEN NULL ELSE l.ended_at * 1000 END AS ended_at_ms,
    l.duration_ms AS duration_ms,
    0 AS trace_complete,
    'legacy_client_no_step_data' AS trace_loss_reason,
    printf(
      '00000000-0000-7%s-8%s-00%s',
      substr(printf('%016x', l.id), 1, 3),
      substr(printf('%016x', l.id), 4, 3),
      substr(printf('%016x', l.id), 7, 10)
    ) AS sort_run_id,
    NULL AS device_serial,
    '[]' AS source_paths_json,
    '[]' AS source_urls_json,
    NULL AS error_code
  FROM usage_logs AS l
  LEFT JOIN api_users AS u ON u.id = l.api_user_id
  WHERE NOT EXISTS (
    SELECT 1 FROM usage_operation_runs AS projected WHERE projected.run_id = l.event_key
  )
)
`;

interface CombinedRunRow {
  source_schema: number;
  trace_ref: string;
  run_id: string | null;
  legacy_id: number | null;
  user_id: number | null;
  username: string | null;
  user_name: string | null;
  operation_kind: string;
  title: string;
  outcome: string;
  client_version: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  trace_complete: number;
  trace_loss_reason: string | null;
  sort_run_id: string;
  device_serial: string | null;
  source_paths_json: string;
  source_urls_json: string;
  error_code: string | null;
}

interface EventRow {
  event_id: string;
  run_id: string;
  sequence: number;
  event_kind: string;
  step_name: string;
  partition_name: string | null;
  status: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  command_program: string | null;
  command_argv_json: string | null;
  command_line: string | null;
  working_directory: string | null;
  paths_json: string;
  urls_json: string;
  serial: string | null;
  exit_code: number | null;
  stdout_chunks: number;
  stderr_chunks: number;
  verification: string | null;
  device_state: string | null;
  retry_safe: number | null;
  remedies_json: string;
  error_class: string | null;
  error_code: string | null;
  error_message: string | null;
  credential_redactions_json: string;
}

interface RunFilters {
  userId: number | null;
  kind: string | null;
  status: string | null;
  from: number | null;
  to: number | null;
  partition: string | null;
  errorCode: string | null;
  q: string | null;
  limit: number;
  cursor: TraceCursorV2 | null;
}

interface SqlFilter {
  sql: string;
  bindings: unknown[];
}

class QueryInputError extends Error {
  constructor(message: string, readonly status = 400) {
    super(message);
  }
}

class QueryNotFoundError extends QueryInputError {
  constructor(message: string) {
    super(message, 404);
  }
}

export async function listTraceUsersV2(request: Request, url: URL, env: Env): Promise<Response> {
  return executeQuery(request, async () => {
    const filters = parseRunFilters(url);
    filters.userId = null;
    filters.kind = null;
    filters.partition = null;
    filters.errorCode = null;
    const where = buildRunFilter(filters, false);
    const cursor = filters.cursor
      ? "AND (started_at_ms < ? OR (started_at_ms = ? AND sort_run_id < ?))"
      : "";
    const cursorBindings = filters.cursor
      ? [filters.cursor.started_at_ms, filters.cursor.started_at_ms, filters.cursor.run_id]
      : [];
    const result = await env.DB.prepare(`${COMBINED_RUNS_CTE}
      , filtered_runs AS (
        SELECT * FROM combined_runs${where.sql}
      ), ranked_users AS (
        SELECT filtered_runs.*,
               ROW_NUMBER() OVER (
                 PARTITION BY user_id ORDER BY started_at_ms DESC, sort_run_id DESC
               ) AS user_rank,
               COUNT(*) OVER (PARTITION BY user_id) AS operation_count,
               SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END)
                 OVER (PARTITION BY user_id) AS failed_count
        FROM filtered_runs
        WHERE user_id IS NOT NULL
      )
      SELECT * FROM ranked_users
      WHERE user_rank = 1 ${cursor}
      ORDER BY started_at_ms DESC, sort_run_id DESC
      LIMIT ?`)
      .bind(...where.bindings, ...cursorBindings, filters.limit + 1)
      .all<CombinedRunRow & { operation_count: number; failed_count: number }>();

    const pageRows = result.results.slice(0, filters.limit);
    const items: TraceUserSummaryV2[] = pageRows.map((row) => ({
      user_id: Number(row.user_id),
      username: row.username ?? "",
      name: row.user_name ?? "",
      operation_count: Number(row.operation_count),
      failed_count: Number(row.failed_count),
      last_operation: mapRun(row),
      last_activity_at_ms: row.ended_at_ms ?? row.started_at_ms,
    }));
    return queryJson(page(items, pageRows, result.results.length > filters.limit));
  });
}

export async function listTraceRunsV2(request: Request, url: URL, env: Env): Promise<Response> {
  return executeQuery(request, async () => {
    const filters = parseRunFilters(url);
    const rows = await queryRunRows(env, filters, true);
    const pageRows = rows.slice(0, filters.limit);
    return queryJson(page(pageRows.map(mapRun), pageRows, rows.length > filters.limit));
  });
}

export async function getTraceRunV2(request: Request, traceRef: string, env: Env): Promise<Response> {
  return executeQuery(request, async () => queryJson(await loadTraceDetail(env, parseTraceRef(traceRef))));
}

export async function getTraceEventV2(
  request: Request,
  traceRef: string,
  eventId: string,
  env: Env,
): Promise<Response> {
  return executeQuery(request, async () => {
    const parsed = parseTraceRef(traceRef);
    if (parsed.schema !== 2) throw new QueryNotFoundError("旧版记录没有步骤详情。");
    requireUuidV7(eventId, "eventId");
    const runRow = await findCombinedRun(env, parsed.traceRef);
    const eventRow = await env.DB.prepare(
      "SELECT * FROM usage_operation_events WHERE run_id = ? AND event_id = ?",
    ).bind(parsed.id, eventId).first<EventRow>();
    if (!eventRow) throw new QueryNotFoundError("事件不存在。");
    const detail: TraceEventDetailV2 = { run: mapRun(runRow), event: mapEvent(eventRow) };
    return queryJson(detail);
  });
}

export async function getTraceOutputV2(
  request: Request,
  traceRef: string,
  eventId: string,
  url: URL,
  admin: AdminIdentity,
  env: Env,
): Promise<Response> {
  return executeQuery(request, async () => {
    const parsed = parseTraceRef(traceRef);
    if (parsed.schema !== 2) throw new QueryNotFoundError("旧版记录没有输出详情。");
    requireUuidV7(eventId, "eventId");
    const stream = parseOutputStream(url.searchParams.get("stream"));
    const afterChunk = parseAfterChunk(url.searchParams.get("afterChunk"));
    const limit = parseLimit(url.searchParams.get("limit"));
    const metadata = await env.DB.prepare(
      `SELECT r.api_user_id, r.trace_complete, e.stdout_chunks, e.stderr_chunks
       FROM usage_operation_runs AS r
       JOIN usage_operation_events AS e ON e.run_id = r.run_id
       WHERE r.run_id = ? AND e.event_id = ?`,
    ).bind(parsed.id, eventId).first<{
      api_user_id: number;
      trace_complete: number;
      stdout_chunks: number;
      stderr_chunks: number;
    }>();
    if (!metadata) throw new QueryNotFoundError("事件不存在。");

    await writeRequiredAudit(
      env,
      admin,
      "view_trace_output",
      metadata.api_user_id,
      eventId,
      `trace_ref=${parsed.traceRef};stream=${stream};after_chunk=${afterChunk}`,
    );

    const result = await env.DB.prepare(
      `SELECT chunk_id, event_id, stream, chunk_index, text, byte_count, sha256
       FROM usage_output_chunks
       WHERE event_id = ? AND stream = ? AND chunk_index > ?
       ORDER BY chunk_index ASC
       LIMIT ?`,
    ).bind(eventId, stream, afterChunk, limit + 1).all<TraceOutputChunkV2>();
    const chunks = result.results.slice(0, limit).map(mapOutputChunk);
    const hasMore = result.results.length > limit;
    const expectedChunks = stream === "stdout" ? metadata.stdout_chunks : metadata.stderr_chunks;
    const loadedThrough = chunks.length > 0 ? chunks[chunks.length - 1].chunk_index : afterChunk;
    const body: TraceOutputPageV2 = {
      run_id: parsed.id,
      event_id: eventId,
      stream,
      chunks,
      next_after_chunk: hasMore ? loadedThrough : null,
      output_complete: metadata.trace_complete === 1
        && !hasMore
        && (expectedChunks === 0 || loadedThrough >= expectedChunks - 1),
    };
    return queryJson(body);
  });
}

export async function getTraceOverviewV2(request: Request, url: URL, env: Env): Promise<Response> {
  return executeQuery(request, async () => {
    const bucket = url.searchParams.get("bucket") ?? "hour";
    if (bucket !== "hour") throw new QueryInputError("bucket 只支持 hour。");
    const filters = parseRunFilters(url, false);
    filters.userId = null;
    filters.kind = null;
    filters.status = null;
    filters.partition = null;
    filters.errorCode = null;
    filters.q = null;
    const where = buildRunFilter(filters, false);
    const timeoutMs = positiveIntegerOrDefault(env.ONLINE_TIMEOUT_MS, 120_000);
    const onlineCutoff = Math.floor((Date.now() - timeoutMs) / 1000);

    const [runTotals, apiUsers, onlineSessions, trendRows, failureRows] = await Promise.all([
      env.DB.prepare(`${COMBINED_RUNS_CTE}
        SELECT COUNT(*) AS operations,
               COALESCE(SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END), 0) AS failed
        FROM combined_runs${where.sql}`)
        .bind(...where.bindings)
        .first<{ operations: number; failed: number }>(),
      env.DB.prepare("SELECT COUNT(*) AS value FROM api_users").first<{ value: number }>(),
      env.DB.prepare("SELECT COUNT(*) AS value FROM online_sessions WHERE last_seen_at >= ?")
        .bind(onlineCutoff).first<{ value: number }>(),
      env.DB.prepare(`${COMBINED_RUNS_CTE}
        SELECT CAST(started_at_ms / ${HOUR_MS} AS INTEGER) * ${HOUR_MS} AS bucket_start_ms,
               COUNT(*) AS operations,
               COALESCE(SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END), 0) AS failed
        FROM combined_runs${where.sql}
        GROUP BY bucket_start_ms
        ORDER BY bucket_start_ms ASC`)
        .bind(...where.bindings)
        .all<{ bucket_start_ms: number; operations: number; failed: number }>(),
      env.DB.prepare(`${COMBINED_RUNS_CTE}
        SELECT * FROM combined_runs${appendCondition(where.sql, "outcome = 'failed'")}
        ORDER BY started_at_ms DESC, sort_run_id DESC
        LIMIT 10`)
        .bind(...where.bindings)
        .all<CombinedRunRow>(),
    ]);

    const body: TraceOverviewV2 = {
      totals: {
        api_users: Number(apiUsers?.value ?? 0),
        online_sessions: Number(onlineSessions?.value ?? 0),
        operations: Number(runTotals?.operations ?? 0),
        failed: Number(runTotals?.failed ?? 0),
      },
      trend: trendRows.results.map((row) => ({
        bucket_start_ms: Number(row.bucket_start_ms),
        operations: Number(row.operations),
        failed: Number(row.failed),
      })),
      recent_failures: failureRows.results.map(mapRun),
    };
    return queryJson(body);
  });
}

export async function exportTracesV2(
  request: Request,
  url: URL,
  admin: AdminIdentity,
  env: Env,
): Promise<Response> {
  return executeQuery(request, async () => {
    const filters = parseRunFilters(url, false);
    await writeRequiredAudit(
      env,
      admin,
      "export_trace",
      filters.userId,
      null,
      exportAuditReason(filters),
    );
    const exportFilters: RunFilters = { ...filters, limit: EXPORT_BATCH_LIMIT, cursor: null };
    const firstBatch = await queryRunRows(env, exportFilters, true, EXPORT_BATCH_LIMIT);
    return new Response(createExportStream(env, exportFilters, firstBatch), {
      status: 200,
      headers: {
        ...RESPONSE_HEADERS,
        "Content-Type": "application/x-ndjson; charset=utf-8",
        "Content-Disposition": `attachment; filename="nwflash-traces-${Date.now()}.ndjson"`,
      },
    });
  });
}

export async function getAppVersionsSummaryV2(request: Request, env: Env): Promise<Response> {
  return executeQuery(request, async () => {
    const result = await env.DB.prepare(
      "SELECT version, min_version FROM app_versions WHERE enabled = 1 ORDER BY id ASC",
    ).all<{ version: string; min_version: string }>();
    const versions = [...result.results].sort((a, b) => compareVersions(b.version, a.version));
    const now = Date.now();
    const dayStart = utcDayStart(now);
    const updateRequired = await env.DB.prepare(
      `SELECT COUNT(*) AS value FROM usage_operation_runs
       WHERE error_code = 'UPDATE_REQUIRED' AND started_at_ms >= ? AND started_at_ms < ?`,
    ).bind(dayStart, dayStart + DAY_MS).first<{ value: number }>();
    return queryJson({
      current_version: versions[0]?.version ?? null,
      minimum_version: versions[0]?.min_version ?? null,
      supported_versions: versions.map((row) => row.version),
      today_426: Number(updateRequired?.value ?? 0),
      as_of_ms: now,
    });
  });
}

export async function listRomLogsV2(request: Request, url: URL, env: Env): Promise<Response> {
  return executeQuery(request, async () => {
    const limit = parseLimit(url.searchParams.get("limit"));
    const cursor = parseCursor(url.searchParams.get("cursor"));
    const userId = parseOptionalPositiveInteger(url.searchParams.get("userId"), "userId");
    const status = parseOptionalHttpStatus(url.searchParams.get("status"));
    const pd = optionalText(url.searchParams.get("pd"));
    const version = optionalText(url.searchParams.get("version"));
    const q = optionalText(url.searchParams.get("q"));
    const conditions: string[] = [];
    const bindings: unknown[] = [];
    if (userId !== null) { conditions.push("l.user_id = ?"); bindings.push(userId); }
    if (pd !== null) { conditions.push("l.pd = ?"); bindings.push(pd); }
    if (version !== null) { conditions.push("l.version = ?"); bindings.push(version); }
    if (status !== null) { conditions.push("l.status = ?"); bindings.push(status); }
    if (q !== null) {
      const pattern = likePattern(q);
      const columns = ["l.username", "l.user_name", "l.pd", "l.version", "l.url", "CAST(l.status AS TEXT)", "CAST(l.id AS TEXT)"];
      conditions.push(`(${columns.map((column) => `${column} LIKE ? ESCAPE '\\'`).join(" OR ")})`);
      bindings.push(...columns.map(() => pattern));
    }
    if (cursor) {
      conditions.push("(started_at_ms < ? OR (started_at_ms = ? AND sort_run_id < ?))");
      bindings.push(cursor.started_at_ms, cursor.started_at_ms, cursor.run_id);
    }
    const where = conditions.length > 0 ? `WHERE ${conditions.join(" AND ")}` : "";
    const result = await env.DB.prepare(`
      WITH rom_rows AS (
        SELECT l.id,
               l.api_user_id AS user_id,
               COALESCE(l.api_user_name, u.name) AS user_name,
               COALESCE(l.pd, '') AS pd,
               COALESCE(l.version, '') AS version,
               COALESCE(l.status, 0) AS status,
               l.url,
               CAST(strftime('%s', l.created_at) AS INTEGER) * 1000 AS created_at_ms,
               CAST(strftime('%s', l.created_at) AS INTEGER) * 1000 AS started_at_ms,
               printf('00000000-0000-7000-8000-%012x', l.id) AS sort_run_id,
               u.username AS username
        FROM access_logs AS l
        LEFT JOIN api_users AS u ON u.id = l.api_user_id
      )
      SELECT * FROM rom_rows AS l
      ${where}
      ORDER BY started_at_ms DESC, sort_run_id DESC
      LIMIT ?`)
      .bind(...bindings, limit + 1)
      .all<{
        id: number;
        user_id: number | null;
        user_name: string | null;
        pd: string;
        version: string;
        status: number;
        url: string | null;
        created_at_ms: number;
        started_at_ms: number;
        sort_run_id: string;
      }>();
    const pageRows = result.results.slice(0, limit);
    const items: RomLogAdminRowV2[] = pageRows.map((row) => ({
      id: Number(row.id),
      user_id: row.user_id === null ? null : Number(row.user_id),
      user_name: row.user_name,
      pd: row.pd,
      version: row.version,
      status: Number(row.status),
      url: row.url,
      failure_reason: null,
      detail_unavailable_reason: Number(row.status) >= 400
        ? "legacy_record_no_failure_reason"
        : null,
      created_at_ms: Number(row.created_at_ms),
    }));
    return queryJson(page(items, pageRows, result.results.length > limit));
  });
}

export function traceQueryErrorResponse(
  request: Request,
  status: number,
  code: TraceApiErrorCodeV2,
  message: string,
): Response {
  const requestId = request.headers.get("CF-Ray")?.trim() || crypto.randomUUID();
  return queryJson({ ok: false, error: { code, message, request_id: requestId } }, status);
}

async function executeQuery(request: Request, operation: () => Promise<Response>): Promise<Response> {
  try {
    return await operation();
  } catch (error) {
    if (error instanceof QueryInputError) {
      return traceQueryErrorResponse(request, error.status, "TRACE_INVALID", error.message);
    }
    if (error instanceof TraceValidationError) {
      return traceQueryErrorResponse(request, 400, "TRACE_INVALID", "请求参数无效。");
    }
    console.error("trace V2 administrator query failed", error);
    return traceQueryErrorResponse(request, 500, "TRACE_INTERNAL", "内部错误。");
  }
}

async function queryRunRows(
  env: Env,
  filters: RunFilters,
  paginated: boolean,
  rowLimit = filters.limit + 1,
): Promise<CombinedRunRow[]> {
  const where = buildRunFilter(filters, paginated);
  const limit = paginated ? "LIMIT ?" : "";
  const bindings = paginated ? [...where.bindings, rowLimit] : where.bindings;
  const result = await env.DB.prepare(`${COMBINED_RUNS_CTE}
    SELECT * FROM combined_runs${where.sql}
    ORDER BY started_at_ms DESC, sort_run_id DESC
    ${limit}`)
    .bind(...bindings)
    .all<CombinedRunRow>();
  return result.results;
}

function createExportStream(
  env: Env,
  initialFilters: RunFilters,
  initialRows: CombinedRunRow[],
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let rows: CombinedRunRow[] | null = initialRows;
  let cursor: TraceCursorV2 | null = null;
  let finished = false;

  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (finished) {
        controller.close();
        return;
      }
      try {
        const current = rows ?? await queryRunRows(
          env,
          { ...initialFilters, cursor },
          true,
          EXPORT_BATCH_LIMIT,
        );
        rows = null;
        if (current.length === 0) {
          finished = true;
          controller.close();
          return;
        }

        controller.enqueue(encoder.encode(
          `${current.map((row) => JSON.stringify(mapRun(row))).join("\n")}\n`,
        ));
        const last = current[current.length - 1];
        cursor = { v: 1, started_at_ms: Number(last.started_at_ms), run_id: last.sort_run_id };
        if (current.length < EXPORT_BATCH_LIMIT) {
          finished = true;
          controller.close();
        }
      } catch (error) {
        console.error("trace V2 export stream failed", error);
        controller.error(new Error("trace export stream failed"));
      }
    },
  });
}

function buildRunFilter(filters: RunFilters, includeCursor: boolean): SqlFilter {
  const conditions: string[] = [];
  const bindings: unknown[] = [];
  if (filters.userId !== null) { conditions.push("user_id = ?"); bindings.push(filters.userId); }
  if (filters.kind !== null) { conditions.push("operation_kind = ?"); bindings.push(filters.kind); }
  if (filters.status !== null) { conditions.push("outcome = ?"); bindings.push(filters.status); }
  if (filters.from !== null) { conditions.push("started_at_ms >= ?"); bindings.push(filters.from); }
  if (filters.to !== null) { conditions.push("started_at_ms <= ?"); bindings.push(filters.to); }
  if (filters.partition !== null) {
    conditions.push(`source_schema = 2 AND EXISTS (
      SELECT 1 FROM usage_operation_events AS partition_event
      WHERE partition_event.run_id = combined_runs.run_id AND partition_event.partition_name = ?
    )`);
    bindings.push(filters.partition);
  }
  if (filters.errorCode !== null) {
    conditions.push(`source_schema = 2 AND (
      error_code = ? OR EXISTS (
        SELECT 1 FROM usage_operation_events AS error_event
        WHERE error_event.run_id = combined_runs.run_id AND error_event.error_code = ?
      )
    )`);
    bindings.push(filters.errorCode, filters.errorCode);
  }
  if (filters.q !== null) {
    const pattern = likePattern(filters.q);
    const runColumns = [
      "username", "user_name", "trace_ref", "COALESCE(run_id, '')", "CAST(COALESCE(legacy_id, '') AS TEXT)",
      "operation_kind", "title", "COALESCE(device_serial, '')", "COALESCE(error_code, '')",
    ];
    const runArrays = [
      "EXISTS (SELECT 1 FROM json_each(combined_runs.source_paths_json) AS run_path WHERE CAST(run_path.value AS TEXT) LIKE ? ESCAPE '\\')",
      "EXISTS (SELECT 1 FROM json_each(combined_runs.source_urls_json) AS run_url WHERE CAST(run_url.value AS TEXT) LIKE ? ESCAPE '\\')",
    ];
    const eventColumns = [
      "search_event.event_id", "search_event.event_kind", "search_event.step_name",
      "COALESCE(search_event.partition_name, '')", "COALESCE(search_event.error_code, '')",
      "COALESCE(search_event.serial, '')",
      "COALESCE(search_event.command_program, '')", "COALESCE(search_event.command_line, '')",
      "COALESCE(search_event.working_directory, '')",
    ];
    const eventArrays = [
      "EXISTS (SELECT 1 FROM json_each(search_event.paths_json) AS event_path WHERE CAST(event_path.value AS TEXT) LIKE ? ESCAPE '\\')",
      "EXISTS (SELECT 1 FROM json_each(search_event.urls_json) AS event_url WHERE CAST(event_url.value AS TEXT) LIKE ? ESCAPE '\\')",
    ];
    conditions.push(`(
      ${runColumns.map((column) => `${column} LIKE ? ESCAPE '\\'`).join(" OR ")}
      OR ${runArrays.join(" OR ")}
      OR (source_schema = 2 AND EXISTS (
        SELECT 1 FROM usage_operation_events AS search_event
        WHERE search_event.run_id = combined_runs.run_id AND (
          ${eventColumns.map((column) => `${column} LIKE ? ESCAPE '\\'`).join(" OR ")}
          OR ${eventArrays.join(" OR ")}
        )
      ))
    )`);
    bindings.push(
      ...runColumns.map(() => pattern),
      ...runArrays.map(() => pattern),
      ...eventColumns.map(() => pattern),
      ...eventArrays.map(() => pattern),
    );
  }
  if (includeCursor && filters.cursor !== null) {
    conditions.push("(started_at_ms < ? OR (started_at_ms = ? AND sort_run_id < ?))");
    bindings.push(filters.cursor.started_at_ms, filters.cursor.started_at_ms, filters.cursor.run_id);
  }
  return { sql: conditions.length > 0 ? ` WHERE ${conditions.join(" AND ")}` : "", bindings };
}

function parseRunFilters(url: URL, includePagination = true): RunFilters {
  const status = optionalText(url.searchParams.get("status"));
  if (status !== null && !TRACE_OUTCOMES.has(status)) {
    throw new QueryInputError("status 不受支持。");
  }
  const from = parseOptionalTime(url.searchParams.get("from"), "from");
  const to = parseOptionalTime(url.searchParams.get("to"), "to");
  if (from !== null && to !== null && from > to) throw new QueryInputError("from 不能晚于 to。");
  const q = optionalText(url.searchParams.get("q"));
  if (q !== null) likePattern(q);
  return {
    userId: parseOptionalPositiveInteger(url.searchParams.get("userId"), "userId"),
    kind: optionalText(url.searchParams.get("kind")),
    status,
    from,
    to,
    partition: optionalText(url.searchParams.get("partition")),
    errorCode: optionalText(url.searchParams.get("errorCode")),
    q,
    limit: includePagination ? parseLimit(url.searchParams.get("limit")) : DEFAULT_LIMIT,
    cursor: includePagination ? parseCursor(url.searchParams.get("cursor")) : null,
  };
}

async function loadTraceDetail(env: Env, parsed: ParsedTraceRef): Promise<TraceRunDetailV2> {
  const runRow = await findCombinedRun(env, parsed.traceRef);
  if (parsed.schema === 1) {
    return {
      source_schema: 1,
      detail_available: false,
      detail_unavailable_reason: "legacy_client_no_step_data",
      run: mapRun(runRow),
      events: [],
    };
  }
  const events = await env.DB.prepare(
    "SELECT * FROM usage_operation_events WHERE run_id = ? ORDER BY sequence ASC",
  ).bind(parsed.id).all<EventRow>();
  return {
    source_schema: 2,
    detail_available: true,
    detail_unavailable_reason: null,
    run: mapRun(runRow),
    events: events.results.map(mapEvent),
  };
}

async function findCombinedRun(env: Env, traceRef: string): Promise<CombinedRunRow> {
  const row = await env.DB.prepare(`${COMBINED_RUNS_CTE}
    SELECT * FROM combined_runs WHERE trace_ref = ? LIMIT 1`)
    .bind(traceRef).first<CombinedRunRow>();
  if (!row) throw new QueryNotFoundError("运行记录不存在。");
  return row;
}

function mapRun(row: CombinedRunRow): TraceRunSummaryV2 {
  return {
    source_schema: row.source_schema === 2 ? 2 : 1,
    trace_ref: row.trace_ref,
    run_id: row.run_id,
    legacy_id: row.legacy_id === null ? null : Number(row.legacy_id),
    user_id: row.user_id === null ? null : Number(row.user_id),
    username: row.username,
    user_name: row.user_name,
    operation_kind: row.operation_kind,
    title: row.title,
    outcome: normalizeOutcome(row.outcome),
    client_version: row.client_version,
    started_at_ms: Number(row.started_at_ms),
    ended_at_ms: nullableNumber(row.ended_at_ms),
    duration_ms: nullableNumber(row.duration_ms),
    trace_complete: row.trace_complete === 1,
    trace_loss_reason: row.trace_loss_reason,
  };
}

function mapEvent(row: EventRow): TraceEventV2 {
  const command = row.command_program === null ? null : {
    program: row.command_program,
    argv: parseStringArray(row.command_argv_json),
    display_command: row.command_line ?? "",
    working_directory: row.working_directory,
    paths: parseStringArray(row.paths_json),
    urls: parseStringArray(row.urls_json),
    serial: row.serial,
  };
  return {
    event_id: row.event_id,
    run_id: row.run_id,
    sequence: Number(row.sequence),
    kind: normalizeEventKind(row.event_kind),
    step_name: row.step_name,
    partition_name: row.partition_name,
    status: normalizeEventStatus(row.status),
    started_at_ms: Number(row.started_at_ms),
    ended_at_ms: nullableNumber(row.ended_at_ms),
    duration_ms: nullableNumber(row.duration_ms),
    command,
    exit_code: nullableNumber(row.exit_code),
    stdout_chunks: Number(row.stdout_chunks),
    stderr_chunks: Number(row.stderr_chunks),
    verification: row.verification,
    device_state: row.device_state,
    retry_safe: row.retry_safe === null ? null : row.retry_safe === 1,
    remedies: parseStringArray(row.remedies_json),
    error_class: row.error_class,
    error_code: row.error_code,
    error_message: row.error_message,
    credential_redactions: parseRedactions(row.credential_redactions_json),
  };
}

function mapOutputChunk(row: TraceOutputChunkV2): TraceOutputChunkV2 {
  return {
    chunk_id: row.chunk_id,
    event_id: row.event_id,
    stream: row.stream,
    chunk_index: Number(row.chunk_index),
    text: row.text,
    byte_count: Number(row.byte_count),
    sha256: row.sha256,
  };
}

interface ParsedTraceRef { schema: 1 | 2; id: string; traceRef: string; }

function parseTraceRef(traceRef: string): ParsedTraceRef {
  const v2 = /^v2:(.+)$/.exec(traceRef);
  if (v2) {
    requireUuidV7(v2[1], "traceRef");
    return { schema: 2, id: v2[1], traceRef };
  }
  const v1 = /^v1:([1-9][0-9]*)$/.exec(traceRef);
  if (v1 && Number.isSafeInteger(Number(v1[1]))) {
    return { schema: 1, id: v1[1], traceRef };
  }
  throw new QueryInputError("traceRef 格式无效。");
}

function requireUuidV7(value: string, field: string): void {
  if (!UUID_V7.test(value)) throw new QueryInputError(`${field} 必须是 UUIDv7。`);
}

function parseCursor(value: string | null): TraceCursorV2 | null {
  if (value === null || value === "") return null;
  return decodeTraceCursorV2(value);
}

function parseLimit(value: string | null): number {
  if (value === null || value === "") return DEFAULT_LIMIT;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) throw new QueryInputError("limit 必须是正整数。");
  return Math.min(parsed, MAX_LIMIT);
}

function parseAfterChunk(value: string | null): number {
  if (value === null || value === "") return -1;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < -1) throw new QueryInputError("afterChunk 必须是不小于 -1 的整数。");
  return parsed;
}

function parseOutputStream(value: string | null): TraceOutputStreamV2 {
  if (value === null || value === "") return "stdout";
  if (value !== "stdout" && value !== "stderr") throw new QueryInputError("stream 必须是 stdout 或 stderr。");
  return value;
}

function parseOptionalTime(value: string | null, field: string): number | null {
  if (value === null || value.trim() === "") return null;
  const trimmed = value.trim();
  const parsed = /^[0-9]+$/.test(trimmed) ? Number(trimmed) : Date.parse(trimmed);
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new QueryInputError(`${field} 必须是有效时间。`);
  return parsed;
}

function parseOptionalPositiveInteger(value: string | null, field: string): number | null {
  if (value === null || value.trim() === "") return null;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) throw new QueryInputError(`${field} 必须是正整数。`);
  return parsed;
}

function parseOptionalHttpStatus(value: string | null): number | null {
  if (value === null || value.trim() === "") return null;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 100 || parsed > 599) {
    throw new QueryInputError("status 必须是有效 HTTP 状态码。");
  }
  return parsed;
}

function optionalText(value: string | null): string | null {
  if (value === null) return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function likePattern(value: string): string {
  const pattern = `%${value.replace(/[\\%_]/g, (character) => `\\${character}`)}%`;
  if (new TextEncoder().encode(pattern).byteLength > MAX_LIKE_PATTERN_BYTES) {
    throw new QueryInputError("q 转义后不能超过 50 个 UTF-8 字节。");
  }
  return pattern;
}

function appendCondition(where: string, condition: string): string {
  return where.length > 0 ? `${where} AND ${condition}` : ` WHERE ${condition}`;
}

function page<T, R extends { started_at_ms: number; sort_run_id: string }>(
  items: T[],
  rows: R[],
  hasMore: boolean,
): KeysetPageV2<T> {
  const last = rows[rows.length - 1];
  return {
    items,
    next_cursor: hasMore && last
      ? encodeTraceCursorV2({ v: 1, started_at_ms: Number(last.started_at_ms), run_id: last.sort_run_id })
      : null,
  };
}

function queryJson(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: RESPONSE_HEADERS });
}

async function writeRequiredAudit(
  env: Env,
  admin: AdminIdentity,
  action: "view_trace_output" | "export_trace",
  targetUserId: number | null,
  targetSessionId: string | null,
  reason: string,
): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO admin_audit_log
       (admin_id, admin_username, action, target_user_id, target_session_id, reason)
     VALUES (?, ?, ?, ?, ?, ?)`,
  ).bind(admin.id, admin.username, action, targetUserId, targetSessionId, reason.slice(0, 2_000)).run();
}

function exportAuditReason(filters: RunFilters): string {
  return JSON.stringify({
    userId: filters.userId,
    kind: filters.kind,
    status: filters.status,
    from: filters.from,
    to: filters.to,
    partition: filters.partition,
    errorCode: filters.errorCode,
    q: filters.q,
  });
}

function nullableNumber(value: number | null): number | null {
  return value === null ? null : Number(value);
}

function normalizeOutcome(value: string): TraceRunSummaryV2["outcome"] {
  return TRACE_OUTCOMES.has(value) ? value as TraceRunSummaryV2["outcome"] : "unknown";
}

function normalizeEventKind(value: string): TraceEventV2["kind"] {
  const values: TraceEventV2["kind"][] = ["authorization", "stage", "partition", "command", "skip", "verification", "terminal"];
  return values.includes(value as TraceEventV2["kind"]) ? value as TraceEventV2["kind"] : "stage";
}

function normalizeEventStatus(value: string): TraceEventV2["status"] {
  const values: TraceEventV2["status"][] = ["started", "success", "failed", "canceled", "skipped", "unknown"];
  return values.includes(value as TraceEventV2["status"]) ? value as TraceEventV2["status"] : "unknown";
}

function parseStringArray(value: string | null): string[] {
  if (value === null) return [];
  try {
    const parsed: unknown = JSON.parse(value);
    return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function parseRedactions(value: string): Array<{ kind: string; count: number }> {
  try {
    const parsed: unknown = JSON.parse(value);
    if (!Array.isArray(parsed)) return [];
    return parsed.flatMap((item) => {
      if (!item || typeof item !== "object") return [];
      const candidate = item as Record<string, unknown>;
      return typeof candidate.kind === "string" && Number.isSafeInteger(candidate.count) && Number(candidate.count) > 0
        ? [{ kind: candidate.kind, count: Number(candidate.count) }]
        : [];
    });
  } catch {
    return [];
  }
}

function positiveIntegerOrDefault(value: string | undefined, fallback: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : fallback;
}

function utcDayStart(nowMs: number): number {
  const now = new Date(nowMs);
  return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
}

function compareVersions(a: string, b: string): number {
  const left = a.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const right = b.split(".").map((part) => Number.parseInt(part, 10) || 0);
  for (let index = 0; index < Math.max(left.length, right.length); index += 1) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
}
