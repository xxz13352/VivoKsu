# WEB CRUD authority 实施计划

基线：`codex/vmp-release-completion` @ `a27a843d37910478bb158a24312a3b479b797efa`
范围：管理员 app-version / API-user create、update、delete、rotate-token；本轮计划后立即实现，不部署、不提交。

## 契约

- item 路由只接受精确 `/api/app-versions/{id}`、`/api/users/{id}`、`/api/users/{id}/rotate-token`；`id` 必须为无前导零的 canonical 正十进制安全整数，额外段返回 404，非法 id 返回 400。
- create 在任何 PBKDF2/D1 写入前校验字段类型；缺必填/短密码返回 400，重复 version/username 返回 409。版本创建继续兼容缺省或空 `min_version` → `0.0.0`。
- update 必须至少包含一个可识别字段；所有出现的可识别字段先完整校验。版本更新空 `min_version`、用户更新短 `newPassword`、空/未知-only body 返回 400，且不允许先更新其它字段。
- 多字段 update 构造固定列白名单的单条动态 `UPDATE ... WHERE id = ?`；`meta.changes === 0` 返回 404。delete/rotate 同样检查 `meta.changes`，未知目标不再返回成功或无效 token。
- 普通 admin 错误继续使用 `{error:string}`，与现有 `AdminApiError` 兼容；成功状态保持 create 201、其它 200。

## 实施与测试

1. 先在现有 admin Workerd 套件加入失败测试：精确/畸形路径、非法 id、unknown 404、空/非法 mutation 400、原子多字段拒绝、duplicate 409、正常 CRUD/rotate。
2. 最小修改 `cloudflare/web/src/index.ts`：精确 route regex、共享正整数解析、create 预校验、单条 update、changes authority。
3. 如页面 mock 不能证明真实请求契约，再最小补 admin API/browser 测试；不改 UI 业务状态机。
4. 更新 `cloudflare/web/README.md`（必要时 `cloudflare/API.md`）的管理 API authority 状态码。
5. 验收：定向 Workerd → admin unit/Workerd/browser → typecheck/dry-run → `git diff --check`，再复查无部分写入、token 泄漏或范围外修改。

不触碰：`cloudflare/src/index.ts`、`cloudflare/web/schema.sql`、`cloudflare/user/**`、`src/Nwflash.Desktop/**`、文件传输、部署/config、分支/ref。
