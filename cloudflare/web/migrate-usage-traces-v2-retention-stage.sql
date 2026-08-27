ALTER TABLE usage_operation_runs
  ADD COLUMN retention_detail_cleared INTEGER NOT NULL DEFAULT 0
  CHECK(retention_detail_cleared IN (0,1));

ALTER TABLE usage_operation_events
  ADD COLUMN retention_detail_cleared INTEGER NOT NULL DEFAULT 0
  CHECK(retention_detail_cleared IN (0,1));

CREATE INDEX idx_trace_runs_retention_detail_pending
  ON usage_operation_runs(started_at_ms, run_id)
  WHERE retention_detail_cleared = 0;

CREATE INDEX idx_trace_events_retention_detail_pending
  ON usage_operation_events(run_id, sequence, event_id)
  WHERE retention_detail_cleared = 0;

DROP TRIGGER IF EXISTS trg_trace_events_reject_sealed_detail_update;
CREATE TRIGGER trg_trace_events_reject_sealed_detail_update
BEFORE UPDATE ON usage_operation_events
WHEN OLD.retention_detail_cleared = 1
BEGIN
  SELECT RAISE(ABORT, 'trace retention detail sealed');
END;

DROP TRIGGER IF EXISTS trg_trace_runs_reject_sealed_detail_update;
CREATE TRIGGER trg_trace_runs_reject_sealed_detail_update
BEFORE UPDATE ON usage_operation_runs
WHEN OLD.retention_detail_cleared = 1
 AND (
   NEW.run_id IS NOT OLD.run_id
   OR NEW.api_user_id IS NOT OLD.api_user_id
   OR NEW.api_user_name IS NOT OLD.api_user_name
   OR NEW.schema_version IS NOT OLD.schema_version
   OR NEW.operation_kind IS NOT OLD.operation_kind
   OR NEW.title IS NOT OLD.title
   OR NEW.outcome IS NOT OLD.outcome
   OR NEW.device_serial IS NOT OLD.device_serial
   OR NEW.source_ip IS NOT OLD.source_ip
   OR NEW.source_paths_json IS NOT OLD.source_paths_json
   OR NEW.source_urls_json IS NOT OLD.source_urls_json
   OR NEW.client_version IS NOT OLD.client_version
   OR NEW.started_at_ms IS NOT OLD.started_at_ms
   OR NEW.ended_at_ms IS NOT OLD.ended_at_ms
   OR NEW.duration_ms IS NOT OLD.duration_ms
   OR NEW.error_class IS NOT OLD.error_class
   OR NEW.error_code IS NOT OLD.error_code
   OR NEW.error_message IS NOT OLD.error_message
   OR NEW.final_sequence IS NOT OLD.final_sequence
   OR NEW.trace_complete IS NOT OLD.trace_complete
   OR NEW.trace_loss_reason IS NOT OLD.trace_loss_reason
   OR NEW.credential_redactions_json IS NOT OLD.credential_redactions_json
   OR NEW.retention_detail_cleared IS NOT OLD.retention_detail_cleared
   OR NEW.created_at IS NOT OLD.created_at
 )
BEGIN
  SELECT RAISE(ABORT, 'trace retention detail sealed');
END;

DROP TRIGGER IF EXISTS trg_trace_runs_validate_completion;
CREATE TRIGGER trg_trace_runs_validate_completion
BEFORE UPDATE OF trace_complete ON usage_operation_runs
WHEN OLD.trace_complete = 0 AND NEW.trace_complete = 1
BEGIN
  SELECT RAISE(ABORT, 'trace retention detail sealed')
  WHERE OLD.retention_detail_cleared = 1 OR NEW.retention_detail_cleared = 1;
  SELECT RAISE(ABORT, 'trace completion requires terminal outcome')
  WHERE NEW.outcome = 'running';
  SELECT RAISE(ABORT, 'trace run is incomplete')
  WHERE NEW.final_sequence IS NULL
     OR (
       SELECT COUNT(*) FROM usage_operation_events WHERE run_id = NEW.run_id
     ) <> NEW.final_sequence
     OR (
       SELECT MIN(sequence) FROM usage_operation_events WHERE run_id = NEW.run_id
     ) <> 1
     OR (
       SELECT MAX(sequence) FROM usage_operation_events WHERE run_id = NEW.run_id
     ) <> NEW.final_sequence
     OR EXISTS (
       SELECT 1
       FROM usage_operation_events AS event
       WHERE event.run_id = NEW.run_id
         AND (
           event.stdout_chunks <> (
             SELECT COUNT(*) FROM usage_output_chunks
             WHERE event_id = event.event_id AND stream = 'stdout'
           )
           OR (
             event.stdout_chunks > 0
             AND (
               (
                 SELECT MIN(chunk_index) FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stdout'
               ) <> 0
               OR (
                 SELECT MAX(chunk_index) FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stdout'
               ) <> event.stdout_chunks - 1
             )
           )
           OR event.stderr_chunks <> (
             SELECT COUNT(*) FROM usage_output_chunks
             WHERE event_id = event.event_id AND stream = 'stderr'
           )
           OR (
             event.stderr_chunks > 0
             AND (
               (
                 SELECT MIN(chunk_index) FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stderr'
               ) <> 0
               OR (
                 SELECT MAX(chunk_index) FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stderr'
               ) <> event.stderr_chunks - 1
             )
           )
         )
     );
END;

DROP TRIGGER IF EXISTS trg_trace_events_reject_completed_run;
CREATE TRIGGER trg_trace_events_reject_completed_run
BEFORE INSERT ON usage_operation_events
BEGIN
  SELECT RAISE(ABORT, 'trace event parent missing')
  WHERE NOT EXISTS (
    SELECT 1 FROM usage_operation_runs WHERE run_id = NEW.run_id
  );
  SELECT RAISE(ABORT, 'trace run is complete')
  WHERE EXISTS (
    SELECT 1 FROM usage_operation_runs
    WHERE run_id = NEW.run_id AND trace_complete = 1
  );
  SELECT RAISE(ABORT, 'trace retention detail sealed')
  WHERE EXISTS (
    SELECT 1 FROM usage_operation_runs
    WHERE run_id = NEW.run_id AND retention_detail_cleared = 1
  );
  SELECT RAISE(ABORT, 'trace event sequence exceeds final sequence')
  WHERE EXISTS (
    SELECT 1 FROM usage_operation_runs
    WHERE run_id = NEW.run_id
      AND final_sequence IS NOT NULL
      AND NEW.sequence > final_sequence
  );
  SELECT RAISE(ABORT, 'trace event quota exceeded')
  WHERE (
    SELECT COUNT(*) FROM usage_operation_events WHERE run_id = NEW.run_id
  ) >= 100;
  SELECT RAISE(ABORT, 'trace event sequence exceeds run quota')
  WHERE NEW.sequence > 100;
  SELECT RAISE(ABORT, 'trace event storage exceeded')
  WHERE COALESCE((
    SELECT SUM(
      length(CAST(COALESCE(event_id, '') AS BLOB))
      + length(CAST(COALESCE(run_id, '') AS BLOB))
      + length(CAST(COALESCE(event_kind, '') AS BLOB))
      + length(CAST(COALESCE(step_name, '') AS BLOB))
      + length(CAST(COALESCE(partition_name, '') AS BLOB))
      + length(CAST(COALESCE(status, '') AS BLOB))
      + length(CAST(COALESCE(command_program, '') AS BLOB))
      + length(CAST(COALESCE(command_argv_json, '') AS BLOB))
      + length(CAST(COALESCE(command_line, '') AS BLOB))
      + length(CAST(COALESCE(working_directory, '') AS BLOB))
      + length(CAST(COALESCE(paths_json, '') AS BLOB))
      + length(CAST(COALESCE(urls_json, '') AS BLOB))
      + length(CAST(COALESCE(serial, '') AS BLOB))
      + length(CAST(COALESCE(verification, '') AS BLOB))
      + length(CAST(COALESCE(device_state, '') AS BLOB))
      + length(CAST(COALESCE(remedies_json, '') AS BLOB))
      + length(CAST(COALESCE(error_class, '') AS BLOB))
      + length(CAST(COALESCE(error_code, '') AS BLOB))
      + length(CAST(COALESCE(error_message, '') AS BLOB))
      + length(CAST(COALESCE(credential_redactions_json, '') AS BLOB))
    )
    FROM usage_operation_events
    WHERE run_id = NEW.run_id
  ), 0)
  + length(CAST(COALESCE(NEW.event_id, '') AS BLOB))
  + length(CAST(COALESCE(NEW.run_id, '') AS BLOB))
  + length(CAST(COALESCE(NEW.event_kind, '') AS BLOB))
  + length(CAST(COALESCE(NEW.step_name, '') AS BLOB))
  + length(CAST(COALESCE(NEW.partition_name, '') AS BLOB))
  + length(CAST(COALESCE(NEW.status, '') AS BLOB))
  + length(CAST(COALESCE(NEW.command_program, '') AS BLOB))
  + length(CAST(COALESCE(NEW.command_argv_json, '') AS BLOB))
  + length(CAST(COALESCE(NEW.command_line, '') AS BLOB))
  + length(CAST(COALESCE(NEW.working_directory, '') AS BLOB))
  + length(CAST(COALESCE(NEW.paths_json, '') AS BLOB))
  + length(CAST(COALESCE(NEW.urls_json, '') AS BLOB))
  + length(CAST(COALESCE(NEW.serial, '') AS BLOB))
  + length(CAST(COALESCE(NEW.verification, '') AS BLOB))
  + length(CAST(COALESCE(NEW.device_state, '') AS BLOB))
  + length(CAST(COALESCE(NEW.remedies_json, '') AS BLOB))
  + length(CAST(COALESCE(NEW.error_class, '') AS BLOB))
  + length(CAST(COALESCE(NEW.error_code, '') AS BLOB))
  + length(CAST(COALESCE(NEW.error_message, '') AS BLOB))
  + length(CAST(COALESCE(NEW.credential_redactions_json, '') AS BLOB))
  > 8388608;
END;

DROP TRIGGER IF EXISTS trg_trace_chunks_reject_completed_run;
CREATE TRIGGER trg_trace_chunks_reject_completed_run
BEFORE INSERT ON usage_output_chunks
BEGIN
  SELECT RAISE(ABORT, 'trace chunk parent missing')
  WHERE NOT EXISTS (
    SELECT 1
    FROM usage_operation_events AS event
    JOIN usage_operation_runs AS run ON run.run_id = event.run_id
    WHERE event.event_id = NEW.event_id
  );
  SELECT RAISE(ABORT, 'trace run is complete')
  WHERE EXISTS (
    SELECT 1
    FROM usage_operation_events AS event
    JOIN usage_operation_runs AS run ON run.run_id = event.run_id
    WHERE event.event_id = NEW.event_id AND run.trace_complete = 1
  );
  SELECT RAISE(ABORT, 'trace retention detail sealed')
  WHERE EXISTS (
    SELECT 1
    FROM usage_operation_events AS event
    JOIN usage_operation_runs AS run ON run.run_id = event.run_id
    WHERE event.event_id = NEW.event_id
      AND (run.retention_detail_cleared = 1 OR event.retention_detail_cleared = 1)
  );
  SELECT RAISE(ABORT, 'trace chunk index exceeds declared total')
  WHERE EXISTS (
    SELECT 1
    FROM usage_operation_events AS event
    WHERE event.event_id = NEW.event_id
      AND (
        (NEW.stream = 'stdout' AND NEW.chunk_index >= event.stdout_chunks)
        OR (NEW.stream = 'stderr' AND NEW.chunk_index >= event.stderr_chunks)
      )
  );
END;
