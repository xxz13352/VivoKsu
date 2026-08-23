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
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_usage_user ON usage_logs(api_user_id);
CREATE INDEX IF NOT EXISTS idx_usage_kind ON usage_logs(operation_kind);
CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_logs(created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_event ON usage_logs(event_key);

-- 登录限流(用户门户 /api/login;窗口滑动计数,防止枚举轰炸)
CREATE TABLE IF NOT EXISTS login_attempts (
  k TEXT NOT NULL,                    -- ip|username(小写)
  window_start INTEGER NOT NULL,      -- 限流窗口起点(epoch 秒)
  count INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (k, window_start)
);
CREATE INDEX IF NOT EXISTS idx_login_attempts_window ON login_attempts(window_start);

-- 完整性 event_id 的事务状态。pending 只存在于 D1 batch 内,提交后仅为 accepted/rejected。
CREATE TABLE IF NOT EXISTS integrity_event_claims (
  event_id TEXT PRIMARY KEY,
  claim_token TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('pending', 'accepted', 'rejected')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_integrity_claims_outcome ON integrity_event_claims(outcome, updated_at);

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
  PRIMARY KEY (ip_hash, window_start)
);
CREATE INDEX IF NOT EXISTS idx_integrity_rate_window ON integrity_rate_limits(window_start);
