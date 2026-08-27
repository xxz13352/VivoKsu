DROP TRIGGER IF EXISTS trg_trace_runs_reject_complete_running_insert;
CREATE TRIGGER trg_trace_runs_reject_complete_running_insert
BEFORE INSERT ON usage_operation_runs
BEGIN
  SELECT RAISE(ABORT, 'trace completion requires terminal outcome')
  WHERE NEW.trace_complete = 1 AND NEW.outcome = 'running';
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

DROP TRIGGER IF EXISTS trg_trace_events_validate_sequence_update;
CREATE TRIGGER trg_trace_events_validate_sequence_update
BEFORE UPDATE OF sequence ON usage_operation_events
BEGIN
  SELECT RAISE(ABORT, 'trace event sequence outside run quota')
  WHERE NEW.sequence < 1 OR NEW.sequence > 100;
END;

DROP TRIGGER IF EXISTS trg_trace_runs_validate_completion;
CREATE TRIGGER trg_trace_runs_validate_completion
BEFORE UPDATE OF trace_complete ON usage_operation_runs
WHEN OLD.trace_complete = 0 AND NEW.trace_complete = 1
BEGIN
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
