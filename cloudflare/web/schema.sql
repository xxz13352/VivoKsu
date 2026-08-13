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
  note TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 版本号控制(允许解析的 PD + 版本)
CREATE TABLE IF NOT EXISTS versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  pd TEXT NOT NULL,
  version TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(pd, version)
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
