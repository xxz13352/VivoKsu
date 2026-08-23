CREATE TABLE IF NOT EXISTS usage_operation_runs (
  run_id TEXT PRIMARY KEY,
  api_user_id INTEGER NOT NULL,
  api_user_name TEXT NOT NULL,
  schema_version INTEGER NOT NULL CHECK(schema_version = 2),
  operation_kind TEXT NOT NULL,
  title TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('running','success','failed','canceled','denied','aborted','unknown')),
  device_serial TEXT,
  source_ip TEXT,
  source_paths_json TEXT NOT NULL DEFAULT '[]',
  source_urls_json TEXT NOT NULL DEFAULT '[]',
  client_version TEXT NOT NULL DEFAULT '',
  started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
  ended_at_ms INTEGER CHECK(ended_at_ms IS NULL OR ended_at_ms >= 0),
  duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
  error_class TEXT,
  error_code TEXT,
  error_message TEXT,
  final_sequence INTEGER CHECK(final_sequence IS NULL OR final_sequence >= 1),
  trace_complete INTEGER NOT NULL DEFAULT 0 CHECK(trace_complete IN (0,1)),
  trace_loss_reason TEXT,
  credential_redactions_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

CREATE TABLE IF NOT EXISTS usage_operation_events (
  event_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  sequence INTEGER NOT NULL CHECK(sequence >= 1),
  event_kind TEXT NOT NULL CHECK(event_kind IN ('authorization','stage','partition','command','skip','verification','terminal')),
  step_name TEXT NOT NULL,
  partition_name TEXT,
  status TEXT NOT NULL CHECK(status IN ('started','success','failed','canceled','skipped','unknown')),
  started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
  ended_at_ms INTEGER CHECK(ended_at_ms IS NULL OR ended_at_ms >= 0),
  duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
  command_program TEXT,
  command_argv_json TEXT,
  command_line TEXT,
  working_directory TEXT,
  paths_json TEXT NOT NULL DEFAULT '[]',
  urls_json TEXT NOT NULL DEFAULT '[]',
  serial TEXT,
  exit_code INTEGER,
  stdout_chunks INTEGER NOT NULL DEFAULT 0 CHECK(stdout_chunks >= 0),
  stderr_chunks INTEGER NOT NULL DEFAULT 0 CHECK(stderr_chunks >= 0),
  verification TEXT,
  device_state TEXT,
  retry_safe INTEGER CHECK(retry_safe IS NULL OR retry_safe IN (0,1)),
  remedies_json TEXT NOT NULL DEFAULT '[]',
  error_class TEXT,
  error_code TEXT,
  error_message TEXT,
  credential_redactions_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  UNIQUE(run_id, sequence)
);

CREATE TABLE IF NOT EXISTS usage_output_chunks (
  chunk_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL,
  stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr')),
  chunk_index INTEGER NOT NULL CHECK(chunk_index >= 0),
  text TEXT NOT NULL,
  byte_count INTEGER NOT NULL CHECK(byte_count >= 0),
  sha256 TEXT NOT NULL,
  credential_redactions_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  UNIQUE(event_id, stream, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_trace_runs_time
  ON usage_operation_runs(started_at_ms DESC, run_id DESC);
CREATE INDEX IF NOT EXISTS idx_trace_runs_user_time
  ON usage_operation_runs(api_user_id, started_at_ms DESC, run_id DESC);
CREATE INDEX IF NOT EXISTS idx_trace_runs_kind_status_time
  ON usage_operation_runs(operation_kind, outcome, started_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trace_events_run_seq
  ON usage_operation_events(run_id, sequence);
CREATE INDEX IF NOT EXISTS idx_trace_events_partition_status
  ON usage_operation_events(partition_name, status, started_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_trace_output_event_stream
  ON usage_output_chunks(event_id, stream, chunk_index);

CREATE TABLE IF NOT EXISTS usage_trace_ingest_guards (
  guard_id TEXT PRIMARY KEY,
  valid INTEGER NOT NULL CHECK(valid = 1)
);

CREATE TRIGGER IF NOT EXISTS trg_trace_events_reject_completed_run
BEFORE INSERT ON usage_operation_events
WHEN EXISTS (
  SELECT 1
  FROM usage_operation_runs
  WHERE run_id = NEW.run_id AND trace_complete = 1
)
BEGIN
  SELECT RAISE(ABORT, 'trace run is complete');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_chunks_reject_completed_run
BEFORE INSERT ON usage_output_chunks
WHEN EXISTS (
  SELECT 1
  FROM usage_operation_events AS event
  JOIN usage_operation_runs AS run ON run.run_id = event.run_id
  WHERE event.event_id = NEW.event_id AND run.trace_complete = 1
)
BEGIN
  SELECT RAISE(ABORT, 'trace run is complete');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_runs_validate_completion
BEFORE UPDATE OF trace_complete ON usage_operation_runs
WHEN OLD.trace_complete = 0 AND NEW.trace_complete = 1
BEGIN
  SELECT RAISE(ABORT, 'trace run is incomplete')
  WHERE NEW.final_sequence IS NULL
     OR (
       SELECT COUNT(*)
       FROM usage_operation_events
       WHERE run_id = NEW.run_id
     ) <> NEW.final_sequence
     OR (
       SELECT MIN(sequence)
       FROM usage_operation_events
       WHERE run_id = NEW.run_id
     ) <> 1
     OR (
       SELECT MAX(sequence)
       FROM usage_operation_events
       WHERE run_id = NEW.run_id
     ) <> NEW.final_sequence
     OR EXISTS (
       SELECT 1
       FROM usage_operation_events AS event
       WHERE event.run_id = NEW.run_id
         AND (
           event.stdout_chunks <> (
             SELECT COUNT(*)
             FROM usage_output_chunks
             WHERE event_id = event.event_id AND stream = 'stdout'
           )
           OR (
             event.stdout_chunks > 0
             AND (
               (
                 SELECT MIN(chunk_index)
                 FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stdout'
               ) <> 0
               OR (
                 SELECT MAX(chunk_index)
                 FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stdout'
               ) <> event.stdout_chunks - 1
             )
           )
           OR event.stderr_chunks <> (
             SELECT COUNT(*)
             FROM usage_output_chunks
             WHERE event_id = event.event_id AND stream = 'stderr'
           )
           OR (
             event.stderr_chunks > 0
             AND (
               (
                 SELECT MIN(chunk_index)
                 FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stderr'
               ) <> 0
               OR (
                 SELECT MAX(chunk_index)
                 FROM usage_output_chunks
                 WHERE event_id = event.event_id AND stream = 'stderr'
               ) <> event.stderr_chunks - 1
             )
           )
         )
     );
END;
