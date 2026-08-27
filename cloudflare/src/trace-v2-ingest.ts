import type { Env } from "./index";
import {
  TRACE_RUN_MAX_EVENTS,
  TRACE_RUN_MAX_EVENT_STORAGE_BYTES,
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
  retention_detail_cleared: number;
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
  retention_detail_cleared: number;
}

interface PersistedChunkRow {
  chunk_id: string;
  event_id: string;
  stream: "stdout" | "stderr";
  chunk_index: number;
  text: string;
  byte_count: number;
  sha256: string;
  credential_redactions_json: string;
}

interface PersistedRunEventUsage {
  run_id: string;
  event_count: number;
  storage_bytes: number;
}

interface PreparedTraceUpload {
  accepted: TraceUploadResponseV2["accepted"];
  durableAccepted: TraceUploadResponseV2["accepted"];
  pendingWrites: TraceUploadResponseV2["accepted"];
  rejected: TraceRejectedItemV2[];
  writableRuns: TraceUploadRequestV2["runs"];
  newEvents: TraceEventV2[];
  newChunks: TraceOutputChunkV2[];
  persistedRuns: PersistedRunRow[];
  persistedEvents: PersistedEventRow[];
  persistedChunks: PersistedChunkRow[];
}

const TRACE_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Cache-Control": "no-store",
  "Content-Type": "application/json; charset=utf-8",
};
const TRACE_INGEST_ATTEMPTS = 3;
const D1_READ_ID_BATCH_SIZE = 90;
const RETENTION_SEALED_MESSAGE = "retention_expired: detail sealed after 30 days; open trace mutations are no longer accepted.";
const TRACE_EVENT_STORAGE_SQL = [
  "event_id", "run_id", "event_kind", "step_name", "partition_name", "status",
  "command_program", "command_argv_json", "command_line", "working_directory",
  "paths_json", "urls_json", "serial", "verification", "device_state", "remedies_json",
  "error_class", "error_code", "error_message", "credential_redactions_json",
].map((column) => `length(CAST(COALESCE(${column}, '') AS BLOB))`).join(" + ");

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
        return traceErrorV2(409, "TRACE_OWNERSHIP_CONFLICT", "日志标识已属于其他用户。", conflict);
      }

      const prepared = await prepareTraceUpload(env.DB, sanitized, user, sourceIp);
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
        if (isIncompleteTraceError(error)) {
          return traceErrorV2(
            422,
            "TRACE_INCOMPLETE",
            "日志完整性声明与已提交证据不一致。",
            [
              ...incompleteRunRejections(sanitized.payload),
              ...prepared.rejected.filter((item) => item.code === "credential_rejected"),
            ],
          );
        }
        lastBatchError = error;
      }
    }

    const finalConflict = await findCrossUserOwnershipConflict(env.DB, sanitized.payload, user.id);
    if (finalConflict.length > 0) {
      return traceErrorV2(409, "TRACE_OWNERSHIP_CONFLICT", "日志标识已属于其他用户。", finalConflict);
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
      return traceErrorV2(413, "TRACE_BODY_TOO_LARGE", "日志上传内容超过大小限制。");
    }
    if (error instanceof TraceValidationError) {
      return traceErrorV2(400, "TRACE_INVALID", "日志上传内容无效。");
    }
    return traceErrorV2(500, "TRACE_INTERNAL", "日志写入失败。");
  }
}

async function prepareTraceUpload(
  db: D1Database,
  sanitized: RedactedTraceUploadV2,
  user: AuthenticatedTraceUser,
  sourceIp: string,
): Promise<PreparedTraceUpload> {
  const payload = sanitized.payload;
  const payloadRunIds = payload.runs.map((run) => run.run_id);
  const payloadEventIds = payload.events.map((event) => event.event_id);
  const eventLookupRunIds = [...new Set([
    ...payloadRunIds,
    ...payload.events.map((event) => event.run_id),
  ])];
  const eventLookupIds = [...new Set([
    ...payloadEventIds,
    ...payload.output_chunks.map((chunk) => chunk.event_id),
  ])];
  const persistedEvents = await readPersistedEvents(db, eventLookupIds, payload.events);
  const runIds = [...new Set([
    ...eventLookupRunIds,
    ...persistedEvents.map((event) => event.run_id),
  ])];
  const persistedRuns = await readPersistedRuns(db, runIds);
  const persistedEventUsage = await readPersistedRunEventUsage(
    db,
    payload.events.map((event) => event.run_id),
  );
  const eventUsageByRunId = new Map(persistedEventUsage.map((usage) => [usage.run_id, usage]));
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
  const sealedRunIds = new Set(
    persistedRuns
      .filter((run) => run.trace_complete === 0 && run.retention_detail_cleared === 1)
      .map((run) => run.run_id),
  );
  const persistedEventById = new Map(persistedEvents.map((event) => [event.event_id, event]));
  const persistedEventBySequence = new Map(
    persistedEvents.map((event) => [sequenceKey(event.run_id, event.sequence), event]),
  );
  const sealedEventIds = new Set(
    persistedEvents
      .filter((event) => event.retention_detail_cleared === 1)
      .filter((event) => persistedRunById.get(event.run_id)?.trace_complete === 0)
      .map((event) => event.event_id),
  );

  const acceptedRuns: string[] = [];
  const durableRuns: string[] = [];
  const writableRuns: TraceUploadRequestV2["runs"] = [];
  const rejected: TraceRejectedItemV2[] = [];
  for (const run of payload.runs) {
    const persisted = persistedRunById.get(run.run_id);
    if (!persisted) {
      acceptedRuns.push(run.run_id);
      writableRuns.push(run);
      continue;
    }
    if (sealedRunIds.has(run.run_id)) {
      rejected.push({
        entity: "run",
        id: run.run_id,
        code: "invalid",
        message: RETENTION_SEALED_MESSAGE,
      });
      continue;
    }
    if (persistedRunMatches(run, persisted, sanitized)) {
      acceptedRuns.push(run.run_id);
      durableRuns.push(run.run_id);
      continue;
    }
    if (canAdvanceOpenRun(run, persisted, sanitized)) {
      acceptedRuns.push(run.run_id);
      writableRuns.push(run);
      continue;
    }
    rejected.push({
      entity: "run",
      id: run.run_id,
      code: "invalid",
      message: "运行标识与已持久化内容不一致。",
    });
  }
  const acceptedRunIds = new Set(acceptedRuns);
  const payloadRunIdSet = new Set(payloadRunIds);
  const availableRunIds = new Set(acceptedRunIds);
  for (const persisted of persistedRuns) {
    if (!payloadRunIdSet.has(persisted.run_id) && !sealedRunIds.has(persisted.run_id)) {
      availableRunIds.add(persisted.run_id);
    }
  }
  const runEvidenceById = new Map<string, PersistedRunRow | TraceUploadRequestV2["runs"][number]>(
    persistedRuns.map((run) => [run.run_id, run]),
  );
  for (const run of payload.runs) {
    if (acceptedRunIds.has(run.run_id)) runEvidenceById.set(run.run_id, run);
  }

  const acceptedEvents: string[] = [];
  const durableEvents: string[] = [];
  const newEvents: TraceEventV2[] = [];
  for (const event of payload.events) {
    if (payloadRunIdSet.has(event.run_id)
      && !acceptedRunIds.has(event.run_id)
      && sealedRunIds.has(event.run_id)) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "missing_parent",
        message: "事件缺少已接受的运行父项。",
      });
      continue;
    }
    if (sealedRunIds.has(event.run_id) || sealedEventIds.has(event.event_id)) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "invalid",
        message: RETENTION_SEALED_MESSAGE,
      });
      continue;
    }
    const persisted = persistedEventById.get(event.event_id);
    if (persisted) {
      if (persistedEventMatches(event, persisted, sanitized)) {
        acceptedEvents.push(event.event_id);
        durableEvents.push(event.event_id);
      } else {
        rejected.push({
          entity: "event",
          id: event.event_id,
          code: "invalid",
          message: "事件标识与已持久化内容不一致。",
        });
      }
      continue;
    }
    if (!availableRunIds.has(event.run_id)) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "missing_parent",
        message: "事件缺少已接受的运行父项。",
      });
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
    const finalSequence = runEvidenceById.get(event.run_id)?.final_sequence ?? null;
    if (finalSequence !== null && event.sequence > finalSequence) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "invalid",
        message: "事件序号超出运行声明的最终边界。",
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
    const usage = eventUsageByRunId.get(event.run_id) ?? { run_id: event.run_id, event_count: 0, storage_bytes: 0 };
    const eventBytes = traceEventStorageBytes(event, sanitized);
    if (usage.event_count + 1 > TRACE_RUN_MAX_EVENTS) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "invalid",
        message: "运行事件数量超过上限。",
      });
      continue;
    }
    if (usage.storage_bytes + eventBytes > TRACE_RUN_MAX_EVENT_STORAGE_BYTES) {
      rejected.push({
        entity: "event",
        id: event.event_id,
        code: "invalid",
        message: "事件元数据超过运行存储上限。",
      });
      continue;
    }
    usage.event_count += 1;
    usage.storage_bytes += eventBytes;
    eventUsageByRunId.set(event.run_id, usage);
    acceptedEvents.push(event.event_id);
    newEvents.push(event);
  }

  const acceptedEventIds = new Set(acceptedEvents);
  const payloadEventIdSet = new Set(payloadEventIds);
  const availableEventIds = new Set(acceptedEventIds);
  for (const persisted of persistedEvents) {
    if (!payloadEventIdSet.has(persisted.event_id)
      && !sealedRunIds.has(persisted.run_id)
      && !sealedEventIds.has(persisted.event_id)) {
      availableEventIds.add(persisted.event_id);
    }
  }
  const eventEvidenceById = new Map<string, PersistedEventRow | TraceEventV2>(
    persistedEvents.map((event) => [event.event_id, event]),
  );
  for (const event of payload.events) {
    if (acceptedEventIds.has(event.event_id)) eventEvidenceById.set(event.event_id, event);
  }
  const persistedChunks = await readPersistedChunks(
    db,
    payload.output_chunks,
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
    const ancestorRunId = eventRunIds.get(chunk.event_id) ?? "";
    if (payloadEventIdSet.has(chunk.event_id) && !acceptedEventIds.has(chunk.event_id)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的事件父项。",
      });
      continue;
    }
    if (payloadRunIdSet.has(ancestorRunId) && !acceptedRunIds.has(ancestorRunId)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的运行祖先。",
      });
      continue;
    }
    if (sealedRunIds.has(ancestorRunId) || sealedEventIds.has(chunk.event_id)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "invalid",
        message: RETENTION_SEALED_MESSAGE,
      });
      continue;
    }
    const persisted = persistedChunkById.get(chunk.chunk_id);
    if (persisted) {
      if (persistedChunkMatches(chunk, persisted, sanitized)) {
        acceptedChunks.push(chunk.chunk_id);
        durableChunks.push(chunk.chunk_id);
      } else {
        rejected.push({
          entity: "output_chunk",
          id: chunk.chunk_id,
          code: "invalid",
          message: "输出分块标识与已持久化内容不一致。",
        });
      }
      continue;
    }
    if (!availableEventIds.has(chunk.event_id)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的事件父项。",
      });
      continue;
    }
    if (!availableRunIds.has(ancestorRunId)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "missing_parent",
        message: "输出分块缺少已接受的运行祖先。",
      });
      continue;
    }
    if (completedRunIds.has(ancestorRunId)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "sequence_conflict",
        message: "已完成的运行不接受新输出分块。",
      });
      continue;
    }
    if (sanitized.credential_rejected_chunks.has(chunk.chunk_id)) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "credential_rejected",
        message: "凭据跨越原始输出分块边界，请客户端先对完整逻辑流脱敏后重试。",
      });
      continue;
    }
    const parentEvent = eventEvidenceById.get(chunk.event_id);
    const declaredChunks = parentEvent?.[`${chunk.stream}_chunks`] ?? 0;
    if (chunk.chunk_index >= declaredChunks) {
      rejected.push({
        entity: "output_chunk",
        id: chunk.chunk_id,
        code: "invalid",
        message: "输出分块索引超出事件声明总数。",
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

  const writableRunIds = new Set(writableRuns.map((run) => run.run_id));
  return {
    accepted: {
      runs: acceptedRuns,
      events: acceptedEvents,
      output_chunks: acceptedChunks,
    },
    durableAccepted: {
      runs: durableRuns,
      events: durableEvents,
      output_chunks: durableChunks,
    },
    pendingWrites: {
      runs: payload.runs
        .filter((run) => acceptedRunIds.has(run.run_id))
        .filter((run) => writableRunIds.has(run.run_id) || (
          run.trace_complete
          && run.outcome !== "running"
          && !projectedRunIds.has(run.run_id)
        ))
        .map((run) => run.run_id),
      events: newEvents.map((event) => event.event_id),
      output_chunks: newChunks.map((chunk) => chunk.chunk_id),
    },
    rejected,
    writableRuns,
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
  return readRowsByIds<PersistedRunRow>(db,
    `SELECT run_id, api_user_name, operation_kind, title, outcome, device_serial, source_ip,
            source_paths_json, source_urls_json, client_version, started_at_ms, ended_at_ms,
            duration_ms, error_class, error_code, error_message, final_sequence, trace_complete,
            trace_loss_reason, credential_redactions_json, retention_detail_cleared
     FROM usage_operation_runs`,
    "run_id",
    runIds,
  );
}

async function readProjectedRunIds(db: D1Database, runIds: string[]): Promise<Set<string>> {
  const rows = await readRowsByIds<{ event_key: string }>(db,
    `SELECT event_key
     FROM usage_logs`,
    "event_key",
    runIds,
  );
  return new Set(rows.map((row) => row.event_key));
}

function persistedRunMatches(
  run: TraceUploadRequestV2["runs"][number],
  persisted: PersistedRunRow,
  sanitized: RedactedTraceUploadV2,
): boolean {
  return persisted.operation_kind === run.operation_kind
    && persisted.title === run.title
    && persisted.outcome === run.outcome
    && persisted.device_serial === run.device_serial
    && persisted.source_paths_json === JSON.stringify(run.source_paths)
    && persisted.source_urls_json === JSON.stringify(run.source_urls)
    && persisted.client_version === run.client_version
    && persisted.started_at_ms === run.started_at_ms
    && persisted.ended_at_ms === run.ended_at_ms
    && persisted.duration_ms === run.duration_ms
    && persisted.error_class === run.error_class
    && persisted.error_code === run.error_code
    && persisted.error_message === run.error_message
    && persisted.final_sequence === run.final_sequence
    && persisted.trace_complete === (run.trace_complete ? 1 : 0)
    && persisted.trace_loss_reason === run.trace_loss_reason
    && persisted.credential_redactions_json === JSON.stringify(sanitized.run_redactions.get(run.run_id) ?? []);
}

async function readPersistedRunEventUsage(
  db: D1Database,
  runIds: string[],
): Promise<PersistedRunEventUsage[]> {
  const uniqueIds = [...new Set(runIds)];
  const batches: string[][] = [];
  for (let offset = 0; offset < uniqueIds.length; offset += D1_READ_ID_BATCH_SIZE) {
    batches.push(uniqueIds.slice(offset, offset + D1_READ_ID_BATCH_SIZE));
  }
  const results = await Promise.all(batches.map((batch) => db.prepare(
    `SELECT run_id, COUNT(*) AS event_count,
            COALESCE(SUM(${TRACE_EVENT_STORAGE_SQL}), 0) AS storage_bytes
     FROM usage_operation_events
     WHERE run_id IN (${batch.map(() => "?").join(",")})
     GROUP BY run_id`,
  ).bind(...batch).all<PersistedRunEventUsage>()));
  return results.flatMap((result) => result.results);
}

function canAdvanceOpenRun(
  run: TraceUploadRequestV2["runs"][number],
  persisted: PersistedRunRow,
  sanitized: RedactedTraceUploadV2,
): boolean {
  return persisted.trace_complete === 0
    && persisted.retention_detail_cleared === 0
    && run.trace_complete
    && run.outcome !== "running"
    && persisted.operation_kind === run.operation_kind
    && persisted.title === run.title
    && persisted.device_serial === run.device_serial
    && persisted.source_paths_json === JSON.stringify(run.source_paths)
    && persisted.source_urls_json === JSON.stringify(run.source_urls)
    && persisted.client_version === run.client_version
    && persisted.started_at_ms === run.started_at_ms
    && persisted.credential_redactions_json === JSON.stringify(sanitized.run_redactions.get(run.run_id) ?? []);
}

async function readPersistedEvents(
  db: D1Database,
  eventIds: string[],
  events: TraceEventV2[],
): Promise<PersistedEventRow[]> {
  const selectSql = `SELECT event_id, run_id, sequence, event_kind, step_name, partition_name, status,
                            started_at_ms, ended_at_ms, duration_ms, command_program, command_argv_json,
                            command_line, working_directory, paths_json, urls_json, serial, exit_code,
                            stdout_chunks, stderr_chunks, verification, device_state, retry_safe, remedies_json,
                            error_class, error_code, error_message, credential_redactions_json,
                            retention_detail_cleared
                     FROM usage_operation_events`;
  const [byId, bySequence] = await Promise.all([
    readRowsByIds<PersistedEventRow>(db, selectSql, "event_id", eventIds),
    readPersistedEventsBySequence(db, selectSql, events),
  ]);
  return uniqueRows([...byId, ...bySequence], (row) => row.event_id);
}

async function readPersistedEventsBySequence(
  db: D1Database,
  selectSql: string,
  events: TraceEventV2[],
): Promise<PersistedEventRow[]> {
  const batches: TraceEventV2[][] = [];
  for (let offset = 0; offset < events.length; offset += 45) {
    batches.push(events.slice(offset, offset + 45));
  }
  const results = await Promise.all(batches.map((batch) => db.prepare(
    `${selectSql} WHERE ${batch.map(() => "(run_id = ? AND sequence = ?)").join(" OR ")}`,
  ).bind(...batch.flatMap((event) => [event.run_id, event.sequence])).all<PersistedEventRow>()));
  return results.flatMap((result) => result.results);
}

function persistedEventMatches(
  event: TraceEventV2,
  persisted: PersistedEventRow,
  sanitized: RedactedTraceUploadV2,
): boolean {
  return persisted.run_id === event.run_id
    && persisted.sequence === event.sequence
    && persisted.event_kind === event.kind
    && persisted.step_name === event.step_name
    && persisted.partition_name === event.partition_name
    && persisted.status === event.status
    && persisted.started_at_ms === event.started_at_ms
    && persisted.ended_at_ms === event.ended_at_ms
    && persisted.duration_ms === event.duration_ms
    && persisted.command_program === (event.command?.program ?? null)
    && persisted.command_argv_json === (event.command === null ? null : JSON.stringify(event.command.argv))
    && persisted.command_line === (event.command?.display_command ?? null)
    && persisted.working_directory === (event.command?.working_directory ?? null)
    && persisted.paths_json === JSON.stringify(event.command?.paths ?? [])
    && persisted.urls_json === JSON.stringify(event.command?.urls ?? [])
    && persisted.serial === (event.command?.serial ?? null)
    && persisted.exit_code === event.exit_code
    && persisted.stdout_chunks === event.stdout_chunks
    && persisted.stderr_chunks === event.stderr_chunks
    && persisted.verification === event.verification
    && persisted.device_state === event.device_state
    && persisted.retry_safe === (event.retry_safe === null ? null : event.retry_safe ? 1 : 0)
    && persisted.remedies_json === JSON.stringify(event.remedies)
    && persisted.error_class === event.error_class
    && persisted.error_code === event.error_code
    && persisted.error_message === event.error_message
    && persisted.credential_redactions_json === JSON.stringify(
      sanitized.event_redactions.get(event.event_id) ?? event.credential_redactions,
    );
}

function traceEventStorageBytes(event: TraceEventV2, sanitized: RedactedTraceUploadV2): number {
  const values = [
    event.event_id,
    event.run_id,
    event.kind,
    event.step_name,
    event.partition_name ?? "",
    event.status,
    event.command?.program ?? "",
    event.command === null ? "" : JSON.stringify(event.command.argv),
    event.command?.display_command ?? "",
    event.command?.working_directory ?? "",
    JSON.stringify(event.command?.paths ?? []),
    JSON.stringify(event.command?.urls ?? []),
    event.command?.serial ?? "",
    event.verification ?? "",
    event.device_state ?? "",
    JSON.stringify(event.remedies),
    event.error_class ?? "",
    event.error_code ?? "",
    event.error_message ?? "",
    JSON.stringify(sanitized.event_redactions.get(event.event_id) ?? event.credential_redactions),
  ];
  return values.reduce((total, value) => total + new TextEncoder().encode(value).byteLength, 0);
}

async function readPersistedChunks(
  db: D1Database,
  chunks: TraceOutputChunkV2[],
  chunkIds: string[],
): Promise<PersistedChunkRow[]> {
  const selectSql = `SELECT chunk_id, event_id, stream, chunk_index, text, byte_count, sha256,
                            credential_redactions_json
                     FROM usage_output_chunks`;
  const [byTuple, byId] = await Promise.all([
    readPersistedChunksByTuple(db, selectSql, chunks),
    readRowsByIds<PersistedChunkRow>(db, selectSql, "chunk_id", chunkIds),
  ]);
  return uniqueRows([...byTuple, ...byId], (row) => row.chunk_id);
}

async function readPersistedChunksByTuple(
  db: D1Database,
  selectSql: string,
  chunks: TraceOutputChunkV2[],
): Promise<PersistedChunkRow[]> {
  const batches: TraceOutputChunkV2[][] = [];
  for (let offset = 0; offset < chunks.length; offset += 30) {
    batches.push(chunks.slice(offset, offset + 30));
  }
  const results = await Promise.all(batches.map((batch) => db.prepare(
    `${selectSql} WHERE ${batch.map(() => "(event_id = ? AND stream = ? AND chunk_index = ?)").join(" OR ")}`,
  ).bind(...batch.flatMap((chunk) => [chunk.event_id, chunk.stream, chunk.chunk_index])).all<PersistedChunkRow>()));
  return results.flatMap((result) => result.results);
}

function persistedChunkMatches(
  chunk: TraceOutputChunkV2,
  persisted: PersistedChunkRow,
  sanitized: RedactedTraceUploadV2,
): boolean {
  return persisted.event_id === chunk.event_id
    && persisted.stream === chunk.stream
    && persisted.chunk_index === chunk.chunk_index
    && persisted.text === chunk.text
    && persisted.byte_count === chunk.byte_count
    && persisted.sha256 === chunk.sha256
    && persisted.credential_redactions_json === JSON.stringify(
      sanitized.chunk_redactions.get(chunk.chunk_id) ?? [],
    );
}

function isIncompleteTraceError(error: unknown): boolean {
  return error instanceof Error && /trace run is incomplete/i.test(error.message);
}

function incompleteRunRejections(payload: TraceUploadRequestV2): TraceRejectedItemV2[] {
  return payload.runs
    .filter((run) => run.trace_complete)
    .map((run) => ({
      entity: "run",
      id: run.run_id,
      code: "incomplete_trace",
      message: "完整日志缺少连续事件或已声明的输出分块。",
    }));
}

async function readRowsByIds<T>(
  db: D1Database,
  selectSql: string,
  idColumn: string,
  ids: string[],
): Promise<T[]> {
  const uniqueIds = [...new Set(ids)];
  const batches: string[][] = [];
  for (let offset = 0; offset < uniqueIds.length; offset += D1_READ_ID_BATCH_SIZE) {
    batches.push(uniqueIds.slice(offset, offset + D1_READ_ID_BATCH_SIZE));
  }
  const results = await Promise.all(batches.map((batch) => db.prepare(
    `${selectSql} WHERE ${idColumn} IN (${batch.map(() => "?").join(",")})`,
  ).bind(...batch).all<T>()));
  return results.flatMap((result) => result.results);
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
  const runIds = [...new Set([
    ...payload.runs.map((run) => run.run_id),
    ...payload.events.map((event) => event.run_id),
  ])];
  const eventIds = [...new Set([
    ...payload.events.map((event) => event.event_id),
    ...payload.output_chunks.map((chunk) => chunk.event_id),
  ])];
  const checks = [
    ownershipRows(db, "run_id", "usage_operation_runs", "api_user_id", runIds),
    ownershipRows(
      db,
      "event_id",
      "usage_operation_events JOIN usage_operation_runs USING (run_id)",
      "usage_operation_runs.api_user_id",
      eventIds,
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
  return readRowsByIds<OwnedIdRow>(db,
    `SELECT ${idColumn} AS id, ${ownerExpression} AS api_user_id
     FROM ${tableExpression}`,
    idColumn,
    ids,
  );
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
    buildIngestGuardStatement(db, sanitized.payload, prepared, user.id, guardId),
  ];
  for (const run of prepared.writableRuns) {
    statements.push(db.prepare(
      `INSERT INTO usage_operation_runs
         (run_id, api_user_id, api_user_name, schema_version, operation_kind, title, outcome,
          device_serial, source_ip, source_paths_json, source_urls_json, client_version,
          started_at_ms, ended_at_ms, duration_ms, error_class, error_code, error_message,
          final_sequence, trace_complete, trace_loss_reason, credential_redactions_json)
       VALUES (?, ?, ?, 2, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(run_id) DO UPDATE SET
         operation_kind = excluded.operation_kind,
         title = excluded.title,
         outcome = excluded.outcome,
         device_serial = excluded.device_serial,
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

  for (const run of prepared.writableRuns) {
    if (!run.trace_complete) continue;
    statements.push(db.prepare(
      `UPDATE usage_operation_runs
       SET trace_complete = 1, updated_at = strftime('%s','now')
       WHERE run_id = ? AND api_user_id = ? AND trace_complete = 0`,
    ).bind(run.run_id, user.id));
  }

  const acceptedRunIds = new Set(prepared.accepted.runs);
  const persistedRunById = new Map(prepared.persistedRuns.map((run) => [run.run_id, run]));
  for (const run of sanitized.payload.runs) {
    if (!acceptedRunIds.has(run.run_id)) continue;
    if (!run.trace_complete || run.outcome === "running") continue;
    statements.push(db.prepare(
      `INSERT INTO usage_logs
         (api_user_id, api_user_name, operation_kind, title, status, event_key, started_at, ended_at, duration_ms)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(event_key) DO NOTHING`,
    ).bind(
      user.id,
      persistedRunById.get(run.run_id)?.api_user_name ?? user.name,
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

function buildIngestGuardStatement(
  db: D1Database,
  payload: TraceUploadRequestV2,
  prepared: PreparedTraceUpload,
  userId: number,
  guardId: string,
): D1PreparedStatement {
  const checks: string[] = [];
  const bindings: unknown[] = [guardId];
  const referencedRunIds = [...new Set([
    ...payload.runs.map((run) => run.run_id),
    ...payload.events.map((event) => event.run_id),
  ])];
  const referencedEventIds = [...new Set([
    ...payload.events.map((event) => event.event_id),
    ...payload.output_chunks.map((chunk) => chunk.event_id),
  ])];
  appendOwnershipGuardCheck(
    checks,
    bindings,
    "usage_operation_runs",
    "run_id",
    "api_user_id",
    referencedRunIds,
    userId,
  );
  appendOwnershipGuardCheck(
    checks,
    bindings,
    "usage_operation_events JOIN usage_operation_runs USING (run_id)",
    "event_id",
    "usage_operation_runs.api_user_id",
    referencedEventIds,
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
  appendSealedOpenRunGuard(checks, bindings, referencedRunIds, userId);
  appendSealedOpenEventGuard(checks, bindings, referencedEventIds, userId);
  const persistedRunById = new Map(prepared.persistedRuns.map((run) => [run.run_id, run]));
  const newRunIds: string[] = [];
  const persistedRunSnapshots: PersistedRunRow[] = [];
  for (const run of prepared.writableRuns) {
    const persisted = persistedRunById.get(run.run_id);
    if (persisted) persistedRunSnapshots.push(persisted);
    else newRunIds.push(run.run_id);
  }
  appendNewRunMutationPrecondition(checks, bindings, newRunIds);
  appendPersistedRunMutationPrecondition(checks, bindings, persistedRunSnapshots, userId);
  const writableRunIds = new Set(prepared.writableRuns.map((run) => run.run_id));
  appendExistingRunParentsPrecondition(
    checks,
    bindings,
    [...new Set(
    prepared.newEvents
      .map((event) => event.run_id)
      .filter((runId) => !writableRunIds.has(runId)),
    )],
    userId,
  );
  const newEventIds = new Set(prepared.newEvents.map((event) => event.event_id));
  appendExistingEventParentsPrecondition(
    checks,
    bindings,
    [...new Set(
    prepared.newChunks
      .map((chunk) => chunk.event_id)
      .filter((eventId) => !newEventIds.has(eventId)),
    )],
    userId,
  );
  const conflict = checks.length === 0 ? "0" : checks.join(" OR ");
  return db.prepare(
    `INSERT INTO usage_trace_ingest_guards (guard_id, valid)
     VALUES (?, CASE WHEN ${conflict} THEN 0 ELSE 1 END)`,
  ).bind(...bindings);
}

function appendSealedOpenRunGuard(
  checks: string[],
  bindings: unknown[],
  runIds: string[],
  userId: number,
): void {
  if (runIds.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM usage_operation_runs AS run
       WHERE run.run_id IN (SELECT value FROM json_each(?))
         AND run.api_user_id = ?
         AND run.trace_complete = 0
         AND run.retention_detail_cleared = 1
     )`,
  );
  bindings.push(JSON.stringify(runIds), userId);
}

function appendSealedOpenEventGuard(
  checks: string[],
  bindings: unknown[],
  eventIds: string[],
  userId: number,
): void {
  if (eventIds.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1
       FROM usage_operation_events AS event
       JOIN usage_operation_runs AS run ON run.run_id = event.run_id
       WHERE event.event_id IN (SELECT value FROM json_each(?))
         AND run.api_user_id = ?
         AND run.trace_complete = 0
         AND (run.retention_detail_cleared = 1 OR event.retention_detail_cleared = 1)
     )`,
  );
  bindings.push(JSON.stringify(eventIds), userId);
}

function appendExistingRunParentsPrecondition(
  checks: string[],
  bindings: unknown[],
  runIds: string[],
  userId: number,
): void {
  if (runIds.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM json_each(?) AS parent
       WHERE NOT EXISTS (
         SELECT 1 FROM usage_operation_runs AS run
         WHERE run.run_id = parent.value
           AND run.api_user_id = ?
           AND run.trace_complete = 0
           AND run.retention_detail_cleared = 0
       )
     )`,
  );
  bindings.push(JSON.stringify(runIds), userId);
}

function appendExistingEventParentsPrecondition(
  checks: string[],
  bindings: unknown[],
  eventIds: string[],
  userId: number,
): void {
  if (eventIds.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM json_each(?) AS parent
       WHERE NOT EXISTS (
         SELECT 1
         FROM usage_operation_events AS event
         JOIN usage_operation_runs AS run ON run.run_id = event.run_id
         WHERE event.event_id = parent.value
           AND run.api_user_id = ?
           AND run.trace_complete = 0
           AND run.retention_detail_cleared = 0
           AND event.retention_detail_cleared = 0
       )
     )`,
  );
  bindings.push(JSON.stringify(eventIds), userId);
}

function appendNewRunMutationPrecondition(
  checks: string[],
  bindings: unknown[],
  runIds: string[],
): void {
  if (runIds.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM json_each(?) AS candidate
       WHERE EXISTS (
         SELECT 1 FROM usage_operation_runs AS run WHERE run.run_id = candidate.value
       )
     )`,
  );
  bindings.push(JSON.stringify(runIds));
}

function appendPersistedRunMutationPrecondition(
  checks: string[],
  bindings: unknown[],
  persistedRuns: PersistedRunRow[],
  userId: number,
): void {
  if (persistedRuns.length === 0) return;
  checks.push(
    `EXISTS (
       SELECT 1 FROM json_each(?) AS snapshot
       WHERE NOT EXISTS (
         SELECT 1 FROM usage_operation_runs AS run
         WHERE run.run_id IS json_extract(snapshot.value, '$.run_id')
           AND run.api_user_id = ?
           AND run.api_user_name IS json_extract(snapshot.value, '$.api_user_name')
           AND run.operation_kind IS json_extract(snapshot.value, '$.operation_kind')
           AND run.title IS json_extract(snapshot.value, '$.title')
           AND run.outcome IS json_extract(snapshot.value, '$.outcome')
           AND run.device_serial IS json_extract(snapshot.value, '$.device_serial')
           AND run.source_ip IS json_extract(snapshot.value, '$.source_ip')
           AND json(run.source_paths_json) IS json(json_extract(snapshot.value, '$.source_paths_json'))
           AND json(run.source_urls_json) IS json(json_extract(snapshot.value, '$.source_urls_json'))
           AND run.client_version IS json_extract(snapshot.value, '$.client_version')
           AND run.started_at_ms IS json_extract(snapshot.value, '$.started_at_ms')
           AND run.ended_at_ms IS json_extract(snapshot.value, '$.ended_at_ms')
           AND run.duration_ms IS json_extract(snapshot.value, '$.duration_ms')
           AND run.error_class IS json_extract(snapshot.value, '$.error_class')
           AND run.error_code IS json_extract(snapshot.value, '$.error_code')
           AND run.error_message IS json_extract(snapshot.value, '$.error_message')
           AND run.final_sequence IS json_extract(snapshot.value, '$.final_sequence')
           AND run.trace_complete IS json_extract(snapshot.value, '$.trace_complete')
           AND run.trace_loss_reason IS json_extract(snapshot.value, '$.trace_loss_reason')
           AND json(run.credential_redactions_json) IS json(json_extract(snapshot.value, '$.credential_redactions_json'))
           AND run.retention_detail_cleared IS json_extract(snapshot.value, '$.retention_detail_cleared')
           AND run.retention_detail_cleared = 0
       )
     )`,
  );
  bindings.push(encodePersistedRunSnapshotsForGuard(persistedRuns), userId);
}

export function encodePersistedRunSnapshotsForGuard(persistedRuns: readonly PersistedRunRow[]): string {
  return JSON.stringify(persistedRuns.map((run) => ({
    ...run,
    source_paths_json: JSON.parse(run.source_paths_json) as unknown,
    source_urls_json: JSON.parse(run.source_urls_json) as unknown,
    credential_redactions_json: JSON.parse(run.credential_redactions_json) as unknown,
  })));
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
       WHERE ${idColumn} IN (SELECT value FROM json_each(?))
         AND ${ownerExpression} <> ?
     )`,
  );
  bindings.push(JSON.stringify(ids), userId);
}

export function traceErrorV2(
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
