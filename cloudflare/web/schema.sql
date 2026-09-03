-- nwflash-db schema —— api.nwflash.cc.cd / web.nwflash.cc.cd 共用

-- 管理员(web 后台登录)
CREATE TABLE IF NOT EXISTS admins (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT UNIQUE NOT NULL,
  salt TEXT NOT NULL,                 -- PBKDF2 随机盐(hex)
  password_hash TEXT NOT NULL,        -- PBKDF2-SHA256(hex)
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 管理员登录会话
CREATE TABLE IF NOT EXISTS admin_sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  admin_id INTEGER NOT NULL,
  token TEXT UNIQUE NOT NULL,         -- 随机 session token(cookie)
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- API 用户(调用 api.nwflash.cc.cd 的客户端 / 桌面端登录账号)
CREATE TABLE IF NOT EXISTS api_users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT UNIQUE NOT NULL,          -- 登录 ID(桌面端登录用)
  name TEXT NOT NULL,                     -- 显示名
  token TEXT UNIQUE NOT NULL,             -- API token(Authorization: Bearer)
  password TEXT,                          -- PBKDF2 密码哈希(hex)
  salt TEXT,                              -- PBKDF2 随机盐(hex)
  enabled INTEGER NOT NULL DEFAULT 1,
  banned INTEGER NOT NULL DEFAULT 0,      -- 封禁:禁止登录与查询
  note TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- VivoKsu 客户端版本控制(强制更新)
CREATE TABLE IF NOT EXISTS app_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  version TEXT NOT NULL,                      -- 客户端版本号,如 "1.2.0"
  min_version TEXT NOT NULL DEFAULT '0.0.0',  -- 最低允许版本,低于此强制更新
  download_url TEXT NOT NULL DEFAULT '',      -- 更新下载链接
  note TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(version)
);

-- 访问日志(每次 API 查询)
CREATE TABLE IF NOT EXISTS access_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  api_user_id INTEGER,
  api_user_name TEXT,
  pd TEXT,
  version TEXT,
  url TEXT,                           -- 返回的 OTA 链接(失败为 null)
  status INTEGER,                     -- 200 / 404 / 403 ...
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_logs_created ON access_logs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_logs_user ON access_logs(api_user_id);

-- 在线会话(心跳 + 强制下线)。时间戳一律 INTEGER epoch 秒(与 JS 对齐,避免 T/Z 时区坑)。
CREATE TABLE IF NOT EXISTS online_sessions (
  session_id TEXT PRIMARY KEY,            -- 客户端每次启动生成的 GUID
  user_id INTEGER NOT NULL,               -- api_users.id(归属,upsert/goodbye 都绑定它)
  user_name TEXT NOT NULL,                -- 显示名(冗余,免 JOIN)
  client_version TEXT NOT NULL DEFAULT '',
  ip TEXT NOT NULL DEFAULT '',            -- CF-Connecting-IP,仅展示用,不作鉴权依据;过期即随行删除
  connected_at INTEGER NOT NULL,          -- 首跳时间(epoch 秒);此后永不被 upsert 触碰(时长基准)
  last_seen_at INTEGER NOT NULL,          -- 最近心跳(epoch 秒)
  force_exit_at INTEGER,                  -- 非空 = 服务端已要求该会话强制下线
  force_exit_reason TEXT
);
CREATE INDEX IF NOT EXISTS idx_online_last_seen ON online_sessions(last_seen_at);
CREATE INDEX IF NOT EXISTS idx_online_user ON online_sessions(user_id);

-- 服务器签名租约状态。登录先签名再插入 sequence=1;活动心跳只可用完整绑定元组做原子 CAS。
CREATE TABLE IF NOT EXISTS session_leases (
  session_id TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL,
  username TEXT NOT NULL,
  client_version TEXT NOT NULL,
  build_id TEXT NOT NULL,
  process_nonce TEXT NOT NULL,
  sequence INTEGER NOT NULL CHECK (sequence >= 1),
  last_heartbeat_at INTEGER,               -- 该会话最近一次成功活动心跳;CAS 关联同 user_id 执行全局最小间隔
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_session_leases_user ON session_leases(user_id);
CREATE INDEX IF NOT EXISTS idx_session_leases_updated ON session_leases(updated_at);

-- 管理员操作审计(kick 等;查看入口后续再加)
CREATE TABLE IF NOT EXISTS admin_audit_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  admin_id INTEGER,
  admin_username TEXT,
  action TEXT NOT NULL,                   -- 'kick' 等
  target_user_id INTEGER,
  target_session_id TEXT,
  reason TEXT,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_audit_created ON admin_audit_log(created_at DESC);

-- 客户端使用日志(每操作一条,按 kind 分类;客户端批量上传,由客户端 token 归属用户)
CREATE TABLE IF NOT EXISTS usage_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  api_user_id INTEGER,                    -- 客户端 token 归属用户
  api_user_name TEXT,
  operation_kind TEXT NOT NULL,           -- 分类:Flashing / Rebooting / Transferring / Installing ...
  title TEXT,
  status TEXT NOT NULL DEFAULT 'started', -- success / failed / canceled
  event_key TEXT,                         -- 客户端每次操作生成的事件唯一键(重传幂等去重)
  started_at INTEGER NOT NULL,            -- epoch 秒
  ended_at INTEGER,
  duration_ms INTEGER,
  details_json TEXT NOT NULL DEFAULT '[]',
  source_schema INTEGER NOT NULL DEFAULT 1 CHECK(source_schema IN (1,2)),
  trace_run_id TEXT,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_usage_user ON usage_logs(api_user_id);
CREATE INDEX IF NOT EXISTS idx_usage_kind ON usage_logs(operation_kind);
CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_logs(created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_event_v1
  ON usage_logs(event_key) WHERE source_schema = 1;
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_projection_v2
  ON usage_logs(trace_run_id) WHERE source_schema = 2;

-- 登录限流(用户门户 /api/login;窗口滑动计数,防止枚举轰炸)
CREATE TABLE IF NOT EXISTS login_attempts (
  k TEXT NOT NULL,                    -- ip|username(小写)
  window_start INTEGER NOT NULL,      -- 限流窗口起点(epoch 秒)
  count INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (k, window_start)
);
CREATE INDEX IF NOT EXISTS idx_login_attempts_window ON login_attempts(window_start);

-- 完整性 event_id 的事务内 owner claim。batch 最后一条语句按 claim_token 删除,提交后必须为空。
CREATE TABLE IF NOT EXISTS integrity_event_claims (
  event_id TEXT PRIMARY KEY,
  claim_token TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

-- 客户端完整性事件。只保存闭集枚举和构建元数据;不保存 token/password/path/URL/serial/raw output。
CREATE TABLE IF NOT EXISTS integrity_events (
  event_id TEXT PRIMARY KEY,              -- 客户端随机事件 ID;重试幂等键
  api_user_id INTEGER,                    -- 匿名事件为 NULL
  trusted INTEGER NOT NULL DEFAULT 0 CHECK (trusted IN (0, 1)),
  phase TEXT NOT NULL,
  reason TEXT NOT NULL,
  client_version TEXT NOT NULL,
  build_id TEXT NOT NULL,
  occurred_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_integrity_created ON integrity_events(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_integrity_user ON integrity_events(api_user_id);
CREATE INDEX IF NOT EXISTS idx_integrity_reason ON integrity_events(reason);

-- 完整性上报 IP 窗口计数。仅保存原始 IP 的 SHA-256(base64url),不保存原始 IP。
CREATE TABLE IF NOT EXISTS integrity_rate_limits (
  ip_hash TEXT NOT NULL,
  window_start INTEGER NOT NULL,
  count INTEGER NOT NULL DEFAULT 1,
  last_event_id TEXT NOT NULL DEFAULT '', -- 同窗口最近一次计费 event;有界去重,不保存 rejected claim 行
  PRIMARY KEY (ip_hash, window_start)
);
CREATE INDEX IF NOT EXISTS idx_integrity_rate_window ON integrity_rate_limits(window_start);

-- 崩溃报告补传(P0):客户端下次启动上报上次 panic(crash.log)。
-- event_id 幂等;panic_message/backtrace 由客户端发送前脱敏,服务端只做结构校验。
CREATE TABLE IF NOT EXISTS crash_report_claims (
  event_id TEXT PRIMARY KEY,
  claim_token TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS crash_reports (
  event_id TEXT PRIMARY KEY,              -- 客户端随机事件 ID;重试幂等键
  api_user_id INTEGER,                    -- 匿名上报为 NULL
  trusted INTEGER NOT NULL DEFAULT 0 CHECK (trusted IN (0, 1)),
  client_version TEXT NOT NULL,
  build_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  panic_message TEXT NOT NULL,
  backtrace TEXT NOT NULL DEFAULT '',
  occurred_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_crash_reports_created ON crash_reports(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_crash_reports_user ON crash_reports(api_user_id);

-- 崩溃上报 IP 窗口计数。仅保存原始 IP 的 SHA-256(base64url),不保存原始 IP。
CREATE TABLE IF NOT EXISTS crash_report_rate_limits (
  ip_hash TEXT NOT NULL,
  window_start INTEGER NOT NULL,
  count INTEGER NOT NULL DEFAULT 1,
  last_event_id TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (ip_hash, window_start)
);
CREATE INDEX IF NOT EXISTS idx_crash_rate_window ON crash_report_rate_limits(window_start);

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
  retention_detail_cleared INTEGER NOT NULL DEFAULT 0 CHECK(retention_detail_cleared IN (0,1)),
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  CHECK(trace_complete = 0 OR outcome <> 'running')
);

CREATE TABLE IF NOT EXISTS usage_operation_events (
  event_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 100),
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
  retention_detail_cleared INTEGER NOT NULL DEFAULT 0 CHECK(retention_detail_cleared IN (0,1)),
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
CREATE INDEX IF NOT EXISTS idx_trace_runs_retention_detail_pending
  ON usage_operation_runs(started_at_ms, run_id)
  WHERE retention_detail_cleared = 0;
CREATE INDEX IF NOT EXISTS idx_trace_events_retention_detail_pending
  ON usage_operation_events(run_id, sequence, event_id)
  WHERE retention_detail_cleared = 0;

CREATE TABLE IF NOT EXISTS usage_trace_ingest_guards (
  guard_id TEXT PRIMARY KEY,
  valid INTEGER NOT NULL CHECK(valid = 1)
);

CREATE TRIGGER IF NOT EXISTS trg_usage_logs_validate_projection_insert
BEFORE INSERT ON usage_logs
BEGIN
  SELECT RAISE(ABORT, 'usage log projection provenance invalid')
  WHERE (NEW.source_schema = 1 AND NEW.trace_run_id IS NOT NULL)
     OR (NEW.source_schema = 2 AND (
       NEW.trace_run_id IS NULL
       OR NEW.event_key IS NOT NEW.trace_run_id
       OR NEW.api_user_id IS NULL
       OR NOT EXISTS (
         SELECT 1 FROM usage_operation_runs AS run
         WHERE run.run_id = NEW.trace_run_id
           AND run.api_user_id = NEW.api_user_id
           AND run.trace_complete = 1
       )
     ));
END;

CREATE TRIGGER IF NOT EXISTS trg_usage_logs_validate_projection_update
BEFORE UPDATE OF source_schema, trace_run_id, event_key, api_user_id ON usage_logs
BEGIN
  SELECT RAISE(ABORT, 'usage log projection provenance invalid')
  WHERE (NEW.source_schema = 1 AND NEW.trace_run_id IS NOT NULL)
     OR (NEW.source_schema = 2 AND (
       NEW.trace_run_id IS NULL
       OR NEW.event_key IS NOT NEW.trace_run_id
       OR NEW.api_user_id IS NULL
       OR NOT EXISTS (
         SELECT 1 FROM usage_operation_runs AS run
         WHERE run.run_id = NEW.trace_run_id
           AND run.api_user_id = NEW.api_user_id
           AND run.trace_complete = 1
       )
     ));
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_runs_reject_complete_running_insert
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

CREATE TRIGGER IF NOT EXISTS trg_trace_events_validate_sequence_update
BEFORE UPDATE OF sequence ON usage_operation_events
BEGIN
  SELECT RAISE(ABORT, 'trace event sequence outside run quota')
  WHERE NEW.sequence < 1 OR NEW.sequence > 100;
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

CREATE TRIGGER IF NOT EXISTS trg_trace_events_reject_sealed_detail_update
BEFORE UPDATE ON usage_operation_events
WHEN OLD.retention_detail_cleared = 1
BEGIN
  SELECT RAISE(ABORT, 'trace retention detail sealed');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_runs_reject_sealed_detail_update
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

CREATE TRIGGER IF NOT EXISTS trg_trace_runs_validate_completion
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
