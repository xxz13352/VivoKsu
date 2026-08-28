const DAY_MS = 24 * 60 * 60 * 1_000;
const RETENTION_BATCH_LIMIT = 100;

export interface TraceRetentionResult {
  output_chunks_deleted: number;
  sensitive_fields_cleared: number;
  events_deleted: number;
  runs_deleted: number;
  cutoff_30d_ms: number;
  cutoff_90d_ms: number;
  cutoff_180d_ms: number;
}

export async function purgeExpiredTraceData(
  db: D1Database,
  nowMs: number,
): Promise<TraceRetentionResult> {
  const now = Math.floor(nowMs);
  const cutoff30d = now - 30 * DAY_MS;
  const cutoff90d = now - 90 * DAY_MS;
  const cutoff180d = now - 180 * DAY_MS;
  const [outputChunks, runDetails, eventDetails, events, _legacyProjections, runs] = await db.batch([
    db.prepare(
      `WITH candidate_runs AS MATERIALIZED (
         SELECT run.run_id, run.started_at_ms
         FROM usage_operation_runs AS run INDEXED BY idx_trace_runs_time
         WHERE run.started_at_ms < ?
           AND EXISTS (
             SELECT 1
             FROM usage_operation_events AS event INDEXED BY idx_trace_events_run_seq
             JOIN usage_output_chunks AS chunk INDEXED BY idx_trace_output_event_stream
               ON chunk.event_id = event.event_id
             WHERE event.run_id = run.run_id
           )
         ORDER BY run.started_at_ms ASC, run.run_id ASC
         LIMIT ${RETENTION_BATCH_LIMIT}
       ), candidate_chunks AS MATERIALIZED (
         SELECT chunk.chunk_id
         FROM candidate_runs AS run
         CROSS JOIN usage_operation_events AS event INDEXED BY idx_trace_events_run_seq
         CROSS JOIN usage_output_chunks AS chunk INDEXED BY idx_trace_output_event_stream
         WHERE event.run_id = run.run_id
           AND chunk.event_id = event.event_id
         ORDER BY run.started_at_ms ASC,
                  run.run_id ASC,
                  event.sequence ASC,
                  event.event_id ASC,
                  chunk.stream ASC,
                  chunk.chunk_index ASC,
                  chunk.chunk_id ASC
         LIMIT ?
       )
       DELETE FROM usage_output_chunks
       WHERE chunk_id IN (
         SELECT chunk_id FROM candidate_chunks
       )`,
    ).bind(cutoff30d, RETENTION_BATCH_LIMIT),
    db.prepare(
      `UPDATE usage_operation_runs
       SET device_serial = NULL,
           source_ip = NULL,
           source_paths_json = '[]',
           source_urls_json = '[]',
           error_message = NULL,
           credential_redactions_json = '[]',
           retention_detail_cleared = 1,
           updated_at = strftime('%s','now')
       WHERE run_id IN (
         SELECT run.run_id
         FROM usage_operation_runs AS run
         WHERE run.started_at_ms < ?
           AND run.retention_detail_cleared = 0
         ORDER BY run.started_at_ms ASC, run.run_id ASC
         LIMIT ?
       )`,
    ).bind(cutoff30d, RETENTION_BATCH_LIMIT),
    db.prepare(
      `WITH candidate_runs AS MATERIALIZED (
         SELECT run.run_id, run.started_at_ms
         FROM usage_operation_runs AS run INDEXED BY idx_trace_runs_time
         WHERE run.started_at_ms < ?
           AND EXISTS (
             SELECT 1
             FROM usage_operation_events AS event
                  INDEXED BY idx_trace_events_retention_detail_pending
             WHERE event.run_id = run.run_id
               AND event.retention_detail_cleared = 0
           )
         ORDER BY run.started_at_ms ASC, run.run_id ASC
         LIMIT ${RETENTION_BATCH_LIMIT}
       ), candidate_events AS MATERIALIZED (
         SELECT event.event_id
         FROM candidate_runs AS run
         CROSS JOIN usage_operation_events AS event
                    INDEXED BY idx_trace_events_retention_detail_pending
         WHERE event.run_id = run.run_id
           AND event.retention_detail_cleared = 0
         ORDER BY run.started_at_ms ASC,
                  run.run_id ASC,
                  event.sequence ASC,
                  event.event_id ASC
         LIMIT ?
       )
       UPDATE usage_operation_events
       SET command_program = NULL,
           command_argv_json = NULL,
           command_line = NULL,
           working_directory = NULL,
           paths_json = '[]',
           urls_json = '[]',
           serial = NULL,
           verification = NULL,
           device_state = NULL,
           remedies_json = '[]',
           error_message = NULL,
           credential_redactions_json = '[]',
           retention_detail_cleared = 1
       WHERE event_id IN (
         SELECT event_id FROM candidate_events
       )`,
    ).bind(cutoff30d, RETENTION_BATCH_LIMIT),
    db.prepare(
      `WITH candidate_runs AS MATERIALIZED (
         SELECT run.run_id, run.started_at_ms
         FROM usage_operation_runs AS run INDEXED BY idx_trace_runs_time
         WHERE run.started_at_ms < ?
           AND EXISTS (
             SELECT 1
             FROM usage_operation_events AS event INDEXED BY idx_trace_events_run_seq
             WHERE event.run_id = run.run_id
               AND NOT EXISTS (
                 SELECT 1
                 FROM usage_output_chunks AS chunk INDEXED BY idx_trace_output_event_stream
                 WHERE chunk.event_id = event.event_id
               )
           )
         ORDER BY run.started_at_ms ASC, run.run_id ASC
         LIMIT ${RETENTION_BATCH_LIMIT}
       ), candidate_events AS MATERIALIZED (
         SELECT event.event_id
         FROM candidate_runs AS run
         CROSS JOIN usage_operation_events AS event INDEXED BY idx_trace_events_run_seq
         WHERE event.run_id = run.run_id
           AND NOT EXISTS (
             SELECT 1
             FROM usage_output_chunks AS chunk INDEXED BY idx_trace_output_event_stream
             WHERE chunk.event_id = event.event_id
           )
         ORDER BY run.started_at_ms ASC,
                  run.run_id ASC,
                  event.sequence ASC,
                  event.event_id ASC
         LIMIT ?
       )
       DELETE FROM usage_operation_events
       WHERE event_id IN (
         SELECT event_id FROM candidate_events
       )`,
    ).bind(cutoff90d, RETENTION_BATCH_LIMIT),
    db.prepare(
      `DELETE FROM usage_logs
       WHERE id IN (
         SELECT projection.id
         FROM usage_logs AS projection
         JOIN (
           SELECT run.run_id, run.api_user_id, run.started_at_ms
           FROM usage_operation_runs AS run
           WHERE run.started_at_ms < ?
             AND NOT EXISTS (
               SELECT 1
               FROM usage_operation_events AS event
               WHERE event.run_id = run.run_id
             )
           ORDER BY run.started_at_ms ASC, run.run_id ASC
           LIMIT ?
         ) AS expired_run
           ON projection.source_schema = 2
          AND projection.trace_run_id = expired_run.run_id
          AND projection.api_user_id = expired_run.api_user_id
         ORDER BY expired_run.started_at_ms ASC,
                  expired_run.run_id ASC,
                  projection.id ASC
         LIMIT ?
       )`,
    ).bind(cutoff180d, RETENTION_BATCH_LIMIT, RETENTION_BATCH_LIMIT),
    db.prepare(
      `DELETE FROM usage_operation_runs
       WHERE run_id IN (
         SELECT run.run_id
         FROM usage_operation_runs AS run
         WHERE run.started_at_ms < ?
           AND NOT EXISTS (
             SELECT 1
             FROM usage_operation_events AS event
             WHERE event.run_id = run.run_id
           )
         ORDER BY run.started_at_ms ASC, run.run_id ASC
         LIMIT ?
       )
         AND NOT EXISTS (
           SELECT 1
           FROM usage_logs AS projection
           WHERE projection.source_schema = 2
             AND projection.trace_run_id = usage_operation_runs.run_id
             AND projection.api_user_id = usage_operation_runs.api_user_id
         )`,
    ).bind(cutoff180d, RETENTION_BATCH_LIMIT),
  ]);

  return {
    output_chunks_deleted: changedRows(outputChunks),
    sensitive_fields_cleared: changedRows(runDetails) + changedRows(eventDetails),
    events_deleted: changedRows(events),
    runs_deleted: changedRows(runs),
    cutoff_30d_ms: cutoff30d,
    cutoff_90d_ms: cutoff90d,
    cutoff_180d_ms: cutoff180d,
  };
}

function changedRows(result: D1Result<unknown>): number {
  return Number(result.meta.changes ?? 0);
}
