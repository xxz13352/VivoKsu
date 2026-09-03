-- 崩溃报告补传(P0):客户端下次启动上报上次 panic。event_id 幂等;
-- panic_message/backtrace 由客户端发送前脱敏,服务端只做结构校验与长度上限。
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
