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
