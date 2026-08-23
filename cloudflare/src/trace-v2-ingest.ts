import type { Env } from "./index";
import {
  TraceUploadTooLargeError,
  TraceValidationError,
  readTraceUploadV2,
  type TraceApiErrorCodeV2,
  type TraceRejectedItemV2,
  type TraceEventV2,
  type TraceOutputChunkV2,
  type TraceUploadRequestV2,
  type TraceUploadResponseV2,
} from "./trace-v2-contract";
import { redactTraceUploadV2, type RedactedTraceUploadV2 } from "./trace-v2-redaction";

export interface AuthenticatedTraceUser {
  id: number;
  username: string;
  name: string;
  bearer_token: string;
}

interface OwnedIdRow {
  id: string;
  api_user_id: number;
}

interface PersistedEventRow {
  event_id: string;
  run_id: string;
  sequence: number;
  stdout_chunks: number;
  stderr_chunks: number;
}

interface PersistedRunRow {
  run_id: string;
  api_user_name: string;
  operation_kind: string;
  title: string;
  outcome: string;
  device_serial: string | null;
  source_ip: string | null;
  source_paths_json: string;
  source_urls_json: string;
  client_version: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number | null;
  error_class: string | null;
  error_code: string | null;
  error_message: string | null;
  final_sequence: number | null;
  trace_complete: number;
  trace_loss_reason: string | null;
  credential_redactions_json: string;
}

interface PersistedChunkRow {
  chunk_id: string;
  event_id: string;
  stream: "stdout" | "stderr";
  chunk_index: number;
}

interface PreparedTraceUpload {
  accepted: TraceUploadResponseV2["accepted"];
  durableAccepted: TraceUploadResponseV2["accepted"];
  pendingWrites: TraceUploadResponseV2["accepted"];
  rejected: TraceRejectedItemV2[];
  newEvents: TraceEventV2[];
  newChunks: TraceOutputChunkV2[];
  persistedRuns: PersistedRunRow[];
  persistedEvents: PersistedEventRow[];
  persistedChunks: PersistedChunkRow[];
}

const TRACE_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Content-Type": "application/json; charset=utf-8",
};
const TRACE_INGEST_ATTEMPTS = 3;

export async function ingestTraceUploadV2(
  env: Pick<Env, "DB">,
  request: Request,
  user: AuthenticatedTraceUser,
): Promise<Response> {
  try {
    const payload = await readTraceUploadV2(request);
    const sanitized = redactTraceUploadV2(payload, [user.bearer_token]);
    const sourceIp = request.headers.get("CF-Connecting-IP") ?? "";
    let lastBatchError: unknown;

    for (let attempt = 0; attempt < TRACE_INGEST_ATTEMPTS; attempt += 1) {
      const conflict = await findCrossUserOwnershipConflict(env.DB, sanitized.payload, user.id);
      if (conflict.length > 0) {
        return traceError(409, "TRACE_OWNERSHIP_CONFLICT", "日志标识已属于其他用户。", conflict);
      }

      const prepared = await prepareTraceUpload(env.DB, sanitized, user, sourceIp);
      const incomplete = findIncompleteRuns(sanitized.payload, prepared);
      if (incomplete.length > 0) {
        return traceError(422, "TRACE_INCOMPLETE", "日志完整性声明与已提交证据不一致。", incomplete);
      }

      const statements = buildTraceStatements(
        env.DB,
        sanitized,
        prepared,
        user,
        sourceIp,
        crypto.randomUUID(),
      );
      try {
        await env.DB.batch(statements);
        return traceJson({
          ok: true,
          accepted: prepared.accepted,
          rejected: prepared.rejected,
        } satisfies TraceUploadResponseV2, 200);
      } catch (error) {
        lastBatchError = error;
      }
    }

    const finalConflict = await findCrossUserOwnershipConflict(env.DB, sanitized.payload, user.id);
    if (finalConflict.length > 0) {
      return traceError(409, "TRACE_OWNERSHIP_CONFLICT", "日志标识已属于其他用户。", finalConflict);
    }
    const finalPrepared = await prepareTraceUpload(env.DB, sanitized, user, sourceIp);
    if (!hasTraceItemIds(finalPrepared.pendingWrites)) {
      return traceJson({
        ok: true,
        accepted: finalPrepared.durableAccepted,
        rejected: finalPrepared.rejected,
      } satisfies TraceUploadResponseV2, 200);
    }
    throw lastBatchError;
  } catch (error) {
    if (error instanceof TraceUploadTooLargeError) {
      return traceError(413, "TRACE_BODY_TOO_LARGE", "日志上传内容超过大小限制。");
    }
    if (error instanceof TraceValidationError) {
      return traceError(400, "TRACE_INVALID", "日志上传内容无效。");
    }
    return traceError(500, "TRACE_INTERNAL", "日志写入失败。");
  }
}

async function prepareTraceUpload(
  db: D1Database,
  sanitized: RedactedTraceUploadV2,
  user: AuthenticatedTraceUser,
  sourceIp: string,
): Promise<PreparedTraceUpload> {
  const payload = sanitized.payload;
  const runIds = payload.runs.map((run) => run.run_id);
  const eventIds = payload.events.map((event) => event.event_id);
  const persistedRuns = await readPersistedRuns(db, runIds);
  const persistedRunIds = new Set(persistedRuns.map((run) => run.run_id));
  const persistedRunById = new Map(persistedRuns.map((run) => [run.run_id, run]));
  const projectedRunIds = await readProjectedRunIds(
    db,
    payload.runs
      .filter((run) => run.trace_complete && run.outcome !== "running")
      .map((run) => run.run_id),
  );
  const completedRunIds = new Set(
    persistedRuns.filter((run) => run.trace_complete === 1).map((run) => run.run_id),
  );
  const persistedEvents = await readPersistedEvents(db, runIds, eventIds);
  const persistedEventById = new Map(persistedEvents.map((event) => [event.event_id, event]));
  const persistedEventBySequence = new Map(
    persistedEvents.map((event) => [sequenceKey(event.run_id, event.sequence), event]),
  );

  const acceptedEvents: string[] = [];
  const durableEvents: string[] = [];
  const rejected: TraceRejectedItemV2[] = [];
  const newEvents: TraceEventV2[] = [];
  for (const event of payload.events) {
    if (persistedEventById.has(event.event_id)) {
      acceptedEvents.push(event.event_id);
      durableEvents.push(event.event_id);
      continue;
    }
    if (completedRunIds.has(event.run_id)) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "sequence_conflict",
        message: "已完成的运行不接受新事件。",
      });
      continue;
    }
    if (persistedEventBySequence.has(sequenceKey(event.run_id, event.sequence))) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "sequence_conflict",
        message: "同一运行序号已由其他事件占用。",
      });
      continue;
    }
    acceptedEvents.push(event.event_id);
    newEvents.push(event);
  }

  const acceptedEventIds = new Set(acceptedEvents);
  const chunkEventIds = [...new Set([
    ...persistedEvents.map((event) => event.event_id),
    ...payload.output_chunks.map((chunk) => chunk.event_id),
  ])];
  const persistedChunks = await readPersistedChunks(
    db,
    chunkEventIds,
    payload.output_chunks.map((chunk) => chunk.chunk_id),
  );
  const persistedChunkById = new Map(persistedChunks.map((chunk) => [chunk.chunk_id, chunk]));
  const persistedChunkByIndex = new Map(
    persistedChunks.map((chunk) => [chunkKey(chunk.event_id, chunk.stream, chunk.chunk_index), chunk]),
  );
  const acceptedChunks: string[] = [];
  const durableChunks: string[] = [];
  const newChunks: TraceOutputChunkV2[] = [];
  const eventRunIds = new Map([
    ...persistedEvents.map((event) => [event.event_id, event.run_id] as const),
    ...payload.events.map((event) => [event.event_id, event.run_id] as const),
  ]);
  for (const chunk of payload.output_chunks) {
    if (persistedChunkById.has(chunk.chunk_id)) {
      acceptedChunks.push(chunk.chunk_id);
      durableChunks.push(chunk.chunk_id);
      continue;
    }
    if (completedRunIds.has(eventRunIds.get(chunk.event_id) ?? "")) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "sequence_conflict",
        message: "已完成的运行不接受新输出分块。",
      });
      continue;
    }
    if (!acceptedEventIds.has(chunk.event_id)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的事件父项。",
      });
      continue;
    }
    if (persistedChunkByIndex.has(chunkKey(chunk.event_id, chunk.stream, chunk.chunk_index))) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "sequence_conflict",
        message: "同一输出流序号已由其他分块占用。",
      });
      continue;
    }
    acceptedChunks.push(chunk.chunk_id);
    newChunks.push(chunk);
  }

  return {
    accepted: {
      runs: payload.runs.map((run) => run.run_id),
      events: acceptedEvents,
      output_chunks: acceptedChunks,
    },
    durableAccepted: {
      runs: payload.runs.filter((run) => persistedRunIds.has(run.run_id)).map((run) => run.run_id),
      events: durableEvents,
      output_chunks: durableChunks,
    },
    pendingWrites: {
      runs: payload.runs
        .filter((run) => runRequiresMutation(
          run,
          persistedRunById.get(run.run_id),
          projectedRunIds,
          sanitized,
          user,
          sourceIp,
        ))
        .map((run) => run.run_id),
      events: newEvents.map((event) => event.event_id),
      output_chunks: newChunks.map((chunk) => chunk.chunk_id),
    },
    rejected,
    newEvents,
    newChunks,
    persistedRuns,
    persistedEvents,
    persistedChunks,
  };
}

function hasTraceItemIds(items: TraceUploadResponseV2["accepted"]): boolean {
  return items.runs.length > 0 || items.events.length > 0 || items.output_chunks.length > 0;
}

async function readPersistedRuns(db: D1Database, runIds: string[]): Promise<PersistedRunRow[]> {
  if (runIds.length === 0) return [];
  const rows = await db.prepare(
    `SELECT run_id, api_user_name, operation_kind, title, outcome, device_serial, source_ip,
            source_paths_json, source_urls_json, client_version, started_at_ms, ended_at_ms,
            duration_ms, error_class, error_code, error_message, final_sequence, trace_complete,
            trace_loss_reason, credential_redactions_json
     FROM usage_operation_runs
     WHERE run_id IN (${runIds.map(() => "?").join(",")})`,
  ).bind(...runIds).all<PersistedRunRow>();
  return rows.results;
}

async function readProjectedRunIds(db: D1Database, runIds: string[]): Promise<Set<string>> {
  if (runIds.length === 0) return new Set();
  const rows = await db.prepare(
    `SELECT event_key
     FROM usage_logs
     WHERE event_key IN (${runIds.map(() => "?").join(",")})`,
  ).bind(...runIds).all<{ event_key: string }>();
  return new Set(rows.results.map((row) => row.event_key));
}

function runRequiresMutation(
  run: TraceUploadRequestV2["runs"][number],
  persisted: PersistedRunRow | undefined,
  projectedRunIds: ReadonlySet<string>,
  sanitized: RedactedTraceUploadV2,
  user: AuthenticatedTraceUser,
  sourceIp: string,
): boolean {
  if (!persisted) return true;
  const projectionPending = run.trace_complete
    && run.outcome !== "running"
    && !projectedRunIds.has(run.run_id);
  if (persisted.trace_complete === 1) return projectionPending;
  return projectionPending
    || persisted.api_user_name !== user.name
    || persisted.operation_kind !== run.operation_kind
    || persisted.title !== run.title
    || persisted.outcome !== run.outcome
    || persisted.device_serial !== run.device_serial
    || persisted.source_ip !== sourceIp
    || persisted.source_paths_json !== JSON.stringify(run.source_paths)
    || persisted.source_urls_json !== JSON.stringify(run.source_urls)
    || persisted.client_version !== run.client_version
    || persisted.started_at_ms !== run.started_at_ms
    || persisted.ended_at_ms !== run.ended_at_ms
    || persisted.duration_ms !== run.duration_ms
    || persisted.error_class !== run.error_class
    || persisted.error_code !== run.error_code
    || persisted.error_message !== run.error_message
    || persisted.final_sequence !== run.final_sequence
    || persisted.trace_complete !== (run.trace_complete ? 1 : 0)
    || persisted.trace_loss_reason !== run.trace_loss_reason
    || persisted.credential_redactions_json !== JSON.stringify(sanitized.run_redactions.get(run.run_id) ?? []);
}

async function readPersistedEvents(
  db: D1Database,
  runIds: string[],
  eventIds: string[],
): Promise<PersistedEventRow[]> {
  const clauses: string[] = [];
  const bindings: string[] = [];
  if (runIds.length > 0) {
    clauses.push(`run_id IN (${runIds.map(() => "?").join(",")})`);
    bindings.push(...runIds);
  }
  if (eventIds.length > 0) {
    clauses.push(`event_id IN (${eventIds.map(() => "?").join(",")})`);
    bindings.push(...eventIds);
  }
  if (clauses.length === 0) return [];
  const rows = await db.prepare(
    `SELECT event_id, run_id, sequence, stdout_chunks, stderr_chunks
     FROM usage_operation_events
     WHERE ${clauses.join(" OR ")}`,
  ).bind(...bindings).all<PersistedEventRow>();
  return uniqueRows(rows.results, (row) => row.event_id);
}

async function readPersistedChunks(
  db: D1Database,
  eventIds: string[],
  chunkIds: string[],
): Promise<PersistedChunkRow[]> {
  const clauses: string[] = [];
  const bindings: string[] = [];
  if (eventIds.length > 0) {
    clauses.push(`event_id IN (${eventIds.map(() => "?").join(",")})`);
    bindings.push(...eventIds);
  }
  if (chunkIds.length > 0) {
    clauses.push(`chunk_id IN (${chunkIds.map(() => "?").join(",")})`);
    bindings.push(...chunkIds);
  }
  if (clauses.length === 0) return [];
  const rows = await db.prepare(
    `SELECT chunk_id, event_id, stream, chunk_index
     FROM usage_output_chunks
     WHERE ${clauses.join(" OR ")}`,
  ).bind(...bindings).all<PersistedChunkRow>();
  return uniqueRows(rows.results, (row) => row.chunk_id);
}

function findIncompleteRuns(
  payload: TraceUploadRequestV2,
  prepared: PreparedTraceUpload,
): TraceRejectedItemV2[] {
  const newEventIds = new Set(prepared.newEvents.map((event) => event.event_id));
  const newChunkIds = new Set(prepared.newChunks.map((chunk) => chunk.chunk_id));
  const resultingEvents: Array<PersistedEventRow | TraceEventV2> = [
    ...prepared.persistedEvents,
    ...payload.events.filter((event) => newEventIds.has(event.event_id)),
  ];
  const resultingChunks: Array<PersistedChunkRow | TraceOutputChunkV2> = [
    ...prepared.persistedChunks,
    ...payload.output_chunks.filter((chunk) => newChunkIds.has(chunk.chunk_id)),
  ];
  const incomplete: TraceRejectedItemV2[] = [];

  for (const run of payload.runs) {
    if (!run.trace_complete || run.final_sequence === null) continue;
    const events = resultingEvents.filter((event) => event.run_id === run.run_id);
    const bySequence = new Map(events.map((event) => [event.sequence, event]));
    let complete = events.every((event) => event.sequence <= run.final_sequence!);
    for (let sequence = 1; complete && sequence <= run.final_sequence; sequence += 1) {
      if (!bySequence.has(sequence)) complete = false;
    }
    for (const event of events) {
      if (!complete) break;
      for (const stream of ["stdout", "stderr"] as const) {
        const expected = event[`${stream}_chunks`];
        const indexes = new Set(
          resultingChunks
            .filter((chunk) => chunk.event_id === event.event_id && chunk.stream === stream)
            .map((chunk) => chunk.chunk_index),
        );
        if (indexes.size !== expected) {
          complete = false;
          break;
        }
        for (let index = 0; index < expected; index += 1) {
          if (!indexes.has(index)) {
            complete = false;
            break;
          }
        }
      }
    }
    if (!complete) {
      incomplete.push({
        entity: "run",
        id: run.run_id,
        code: "incomplete_trace",
        message: "完整日志缺少连续事件或已声明的输出分块。",
      });
    }
  }
  return incomplete;
}

function uniqueRows<T>(rows: T[], key: (row: T) => string): T[] {
  return [...new Map(rows.map((row) => [key(row), row])).values()];
}

function sequenceKey(runId: string, sequence: number): string {
  return `${runId}\u0000${sequence}`;
}

function chunkKey(eventId: string, stream: string, chunkIndex: number): string {
  return `${eventId}\u0000${stream}\u0000${chunkIndex}`;
}

async function findCrossUserOwnershipConflict(
  db: D1Database,
  payload: TraceUploadRequestV2,
  userId: number,
): Promise<TraceRejectedItemV2[]> {
  const checks = [
    ownershipRows(db, "run_id", "usage_operation_runs", "api_user_id", payload.runs.map((run) => run.run_id)),
    ownershipRows(
      db,
      "event_id",
      "usage_operation_events JOIN usage_operation_runs USING (run_id)",
      "usage_operation_runs.api_user_id",
      payload.events.map((event) => event.event_id),
    ),
    ownershipRows(
      db,
      "chunk_id",
      "usage_output_chunks JOIN usage_operation_events USING (event_id) JOIN usage_operation_runs USING (run_id)",
      "usage_operation_runs.api_user_id",
      payload.output_chunks.map((chunk) => chunk.chunk_id),
    ),
  ] as const;
  const [runs, events, chunks] = await Promise.all(checks);
  return [
    ...ownershipConflicts("run", runs, userId),
    ...ownershipConflicts("event", events, userId),
    ...ownershipConflicts("output_chunk", chunks, userId),
  ];
}

async function ownershipRows(
  db: D1Database,
  idColumn: string,
  tableExpression: string,
  ownerExpression: string,
  ids: string[],
): Promise<OwnedIdRow[]> {
  if (ids.length === 0) return [];
  const placeholders = ids.map(() => "?").join(",");
  const rows = await db.prepare(
    `SELECT ${idColumn} AS id, ${ownerExpression} AS api_user_id
     FROM ${tableExpression}
     WHERE ${idColumn} IN (${placeholders})`,
  ).bind(...ids).all<OwnedIdRow>();
  return rows.results;
}

function ownershipConflicts(
  entity: TraceRejectedItemV2["entity"],
  rows: OwnedIdRow[],
  userId: number,
): TraceRejectedItemV2[] {
  return rows
    .filter((row) => row.api_user_id !== userId)
    .map((row) => ({ entity, id: row.id, code: "invalid", message: "日志标识已属于其他用户。" }));
}

function buildTraceStatements(
  db: D1Database,
  sanitized: RedactedTraceUploadV2,
  prepared: PreparedTraceUpload,
  user: AuthenticatedTraceUser,
  sourceIp: string,
  guardId: string,
): D1PreparedStatement[] {
  const statements: D1PreparedStatement[] = [
    buildOwnershipGuardStatement(db, sanitized.payload, user.id, guardId),
  ];
  for (const run of sanitized.payload.runs) {
    statements.push(db.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          device_serial, source_ip, source_paths_json, source_urls_json, client_version,
          started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
          final_sequence, trace_complete, trace_loss_reason, credential_redactions_json)
       VALUES (?, ?, ?, 2, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(run_id) DO UPDATE SET
         api_user_name = excluded.api_user_name,
         operation_kind = excluded.operation_kind,
         title = excluded.title,
         outcome = excluded.outcome,
         device_serial = excluded.device_serial,
         source_ip = excluded.source_ip,
         source_paths_json = excluded.source_paths_json,
         source_urls_json = excluded.source_urls_json,
         client_version = excluded.client_version,
         started_at_ms = excluded.started_at_ms,
         ended_at_ms = excluded.ended_at_ms,
         duration_ms = excluded.duration_ms,
         error_class = excluded.error_class,
         error_code = excluded.error_code,
         error_message = excluded.error_message,
         final_sequence = excluded.final_sequence,
         trace_complete = excluded.trace_complete,
         trace_loss_reason = excluded.trace_loss_reason,
         credential_redactions_json = excluded.credential_redactions_json,
         updated_at = strftime('%s','now')
       WHERE usage_operation_runs.api_user_id = excluded.api_user_id
         AND usage_operation_runs.trace_complete = 0`,
    ).bind(
      run.run_id,
      user.id,
      user.name,
      run.operation_kind,
      run.title,
      run.outcome,
      run.device_serial,
      sourceIp,
      JSON.stringify(run.source_paths),
      JSON.stringify(run.source_urls),
      run.client_version,
      run.started_at_ms,
      run.ended_at_ms,
      run.duration_ms,
      run.error_class,
      run.error_code,
      run.error_message,
      run.final_sequence,
      0,
      run.trace_loss_reason,
      JSON.stringify(sanitized.run_redactions.get(run.run_id) ?? []),
    ));
  }

  const newEventIds = new Set(prepared.newEvents.map((event) => event.event_id));
  for (const event of sanitized.payload.events) {
    if (!newEventIds.has(event.event_id)) continue;
    statements.push(db.prepare(
      `INSERT INTO usage_operation_events
         (event_id, run_id, sequence, event_kind, step_name, partition_name, status,
          started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
          command_line, working_directory, paths_json, urls_json, serial, exit_code,
          stdout_chunks, stderr_chunks, verification, device_state, retry_safe, remedies_json,
          error_class, error_code, error_message, credential_redactions_json)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    ).bind(
      event.event_id,
      event.run_id,
      event.sequence,
      event.kind,
      event.step_name,
      event.partition_name,
      event.status,
      event.started_at_ms,
      event.ended_at_ms,
      event.duration_ms,
      event.command?.program ?? null,
      event.command === null ? null : JSON.stringify(event.command.argv),
      event.command?.display_command ?? null,
      event.command?.working_directory ?? null,
      JSON.stringify(event.command?.paths ?? []),
      JSON.stringify(event.command?.urls ?? []),
      event.command?.serial ?? null,
      event.exit_code,
      event.stdout_chunks,
      event.stderr_chunks,
      event.verification,
      event.device_state,
      event.retry_safe === null ? null : event.retry_safe ? 1 : 0,
      JSON.stringify(event.remedies),
      event.error_class,
      event.error_code,
      event.error_message,
      JSON.stringify(sanitized.event_redactions.get(event.event_id) ?? event.credential_redactions),
    ));
  }

  const newChunkIds = new Set(prepared.newChunks.map((chunk) => chunk.chunk_id));
  for (const chunk of sanitized.payload.output_chunks) {
    if (!newChunkIds.has(chunk.chunk_id)) continue;
    statements.push(db.prepare(
      `INSERT INTO usage_output_chunks
         (chunk_id, event_id, stream, chunk_index, text, byte_count, sha256, credential_redactions_json)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    ).bind(
      chunk.chunk_id,
      chunk.event_id,
      chunk.stream,
      chunk.chunk_index,
      chunk.text,
      chunk.byte_count,
      chunk.sha256,
      JSON.stringify(sanitized.chunk_redactions.get(chunk.chunk_id) ?? []),
    ));
  }

  for (const run of sanitized.payload.runs) {
    if (!run.trace_complete) continue;
    statements.push(db.prepare(
      `UPDATE usage_operation_runs
       SET trace_complete = 1, updated_at = strftime('%s','now')
       WHERE run_id = ? AND api_user_id = ? AND trace_complete = 0`,
    ).bind(run.run_id, user.id));
  }

  for (const run of sanitized.payload.runs) {
    if (!run.trace_complete || run.outcome === "running") continue;
    statements.push(db.prepare(
      `INSERT INTO usage_logs
         (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at, ended_at, duration_ms)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(event_key) DO NOTHING`,
    ).bind(
      user.id,
      user.name,
      run.operation_kind,
      run.title,
      run.outcome,
      run.run_id,
      Math.floor(run.started_at_ms / 1000),
      run.ended_at_ms === null ? null : Math.floor(run.ended_at_ms / 1000),
      run.duration_ms,
    ));
  }
  statements.push(db.prepare(
    "DELETE FROM usage_trace_ingest_guards WHERE guard_id = ?",
  ).bind(guardId));
  return statements;
}

function buildOwnershipGuardStatement(
  db: D1Database,
  payload: TraceUploadRequestV2,
  userId: number,
  guardId: string,
): D1PreparedStatement {
  const checks: string[] = [];
  const bindings: unknown[] = [guardId];
  appendOwnershipGuardCheck(
    checks,
    bindings,
    "usage_operation_runs",
    "run_id",
    "api_user_id",
    payload.runs.map((run) => run.run_id),
    userId,
  );
  appendOwnershipGuardCheck(
    checks,
    bindings,
    "usage_operation_events JOIN usage_operation_runs USING (run_id)",
    "event_id",
    "usage_operation_runs.api_user_id",
    payload.events.map((event) => event.event_id),
    userId,
  );
  appendOwnershipGuardCheck(
    checks,
    bindings,
    "usage_output_chunks JOIN usage_operation_events USING (event_id) JOIN usage_operation_runs USING (run_id)",
    "chunk_id",
    "usage_operation_runs.api_user_id",
    payload.output_chunks.map((chunk) => chunk.chunk_id),
    userId,
  );
  const conflict = checks.length === 0 ? "0" : checks.join(" OR ");
  return db.prepare(
    `INSERT INTO usage_trace_ingest_guards (guard_id, valid)
     VALUES (?, CASE WHEN ${conflict} THEN 0 ELSE 1 END)`,
  ).bind(...bindings);
}

function appendOwnershipGuardCheck(
  checks: string[],
  bindings: unknown[],
  tableExpression: string,
  idColumn: string,
  ownerExpression: string,
  ids: string[],
  userId: number,
): void {
  if (ids.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM ${tableExpression}
       WHERE ${idColumn} IN (${ids.map(() => "?").join(",")})
         AND ${ownerExpression} <> ?
     )`,
  );
  bindings.push(...ids, userId);
}

function traceError(
  status: number,
  code: TraceApiErrorCodeV2,
  message: string,
  details?: TraceRejectedItemV2[],
): Response {
  return traceJson({
    ok: false,
    error: {
      code,
      message,
      request_id: crypto.randomUUID(),
      ...(details && details.length > 0 ? { details } : {}),
    },
  }, status);
}

function traceJson(body: unknown, status: number): Response {
  return new Response(JSON.stringify(body), { status, headers: TRACE_HEADERS });
}
