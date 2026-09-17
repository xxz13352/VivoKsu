-- D1 迁移:移除 ROM 白名单 versions 表 → 新增 VivoKsu 版本控制 app_versions
-- 执行:npx wrangler d1 execute nwflash-db --remote --file=cloudflare/web/migrate-app-versions.sql

DROP TABLE IF EXISTS versions;

CREATE TABLE app_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  version TEXT NOT NULL,                      -- 客户端版本号,如 "1.2.0"
  min_version TEXT NOT NULL DEFAULT '0.0.0',  -- 最低允许版本,低于此强制更新
  download_url TEXT NOT NULL DEFAULT '',      -- 更新下载链接
  note TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(version)
);
