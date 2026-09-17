# Cloudflare 管理后台 / API 契约验证报告

> 当前状态说明：本文是 `11a7aba` 时的缺陷基线。F-02 已由 `59fe97d` 修复，F-01 已由 `741ef97` 与 `819cf25` 修复；公共 API 版本头缺失绕过已由 `e17bd81` 修复。其余 CRUD authority、会话清理、force-exit、查询边界、ownership、时区、ROM 原因和部署文档问题继续由 [当前迭代计划](2026-09-04-iteration-plan.md) 跟踪。下文原始发现保留作证据，不等于当前源码仍存在全部问题。

任务编号：WEB-API  
审计 owner：`/root`（受接力任务卡委托）  
验证时间：2026-09-04（Asia/Shanghai）  
基线：分支 `codex/vmp-release-completion`，HEAD `11a7aba4a4d40125b31b827976789d9335e69f1c`。  

## 范围与限制

本次只读核对了：

- `cloudflare/web/src/admin/api.js`、`app.js`、`router.js`、`components.js` 及 `pages/*.js`；
- `cloudflare/web/src/index.ts`、`cloudflare/web/src/trace-v2-query.ts`；
- `cloudflare/web/README.md`、`cloudflare/API.md`，以及相关 admin unit、Workerd、browser 测试；
- 为解释跨 host 契约而只读参考了 `cloudflare/src/index.ts` 和桌面更新对话框；这部分不在本卡实现范围。

工作树在审计开始前已经包含其他 owner 的未提交改动（包括 `cloudflare/API.md`、schema、Safe Flash 和文档）。本任务没有改源码、测试、Safe Flash 在途文件、分支/ref、部署或删除文件；本报告是唯一有意创建的文件。浏览器门禁的默认输出目录产生/保留了 `cloudflare/web/.artifacts/` 测试生成物，按本卡“禁止删文件”要求未清理，交由总指挥按工作区卫生规则处理。

## 可行测试与结果

以下命令均在当前脏工作树运行，未发生测试失败：

| 命令 | 结果 |
| --- | --- |
| `npm run test:admin:unit`（`cloudflare/`） | 9 files / 135 tests 通过 |
| `npm run test:admin:workerd` | 2 files / 51 tests 通过；Wrangler 报代理环境变量 warning |
| `npm run test:admin:browser` | 31 / 31 通过，1 worker；约 1.1 分钟 |
| `npm test` | 3 files / 77 tests 通过 |
| `npm run test:workerd` | 8 files / 192 tests 通过；Wrangler 报代理环境变量 warning |
| `npm run typecheck` | 两轮 strict `tsc`、`dry-run:api`、`dry-run:web` 均通过；Wrangler 报代理/版本升级提示，未部署 |

`npm run test:admin` 没有再作为单一命令重复执行；其三个子套件已分别通过。另有一次只读辅助 `rg` 使用了 PowerShell 不支持的 glob 形式并返回路径错误，未影响代码或测试，也未重试该扫描。

## 按真实页面动作顺序的契约核对

| 页面动作 | 浏览器请求（路径 / 方法 / 字段） | `web` Worker 实际契约 | 核对结论 |
| --- | --- | --- | --- |
| 会话恢复 | `GET /api/me`；same-origin cookie，无请求体 | `web/src/index.ts:138-140,290-294`；无/过期 cookie 仍为 `200 {loggedIn:false}`，有效 cookie 为 `200 {loggedIn:true,username}` | 路径、方法、字段和 UI 恢复逻辑匹配；没有真实 Workerd 已认证恢复测试 |
| 登录 | `POST /api/login`；JSON `{username,password}`，API client 自动 `X-Requested-With` | `web/src/index.ts:139,265-288`；成功 `200` + 7 天 HttpOnly cookie，缺字段 `400`，失败 `401` | 匹配；登录 `401` 只留在表单，不触发 shell 清理 |
| 注销 / 改密 | `POST /api/logout`（无体）／`POST /api/change-password` `{newPassword}` | `web/src/index.ts:151-164,296-344`；注销过期会话也 `200`，改密 `<8` 为 `400` 并吊销所有管理员会话 | 匹配；注销 CSRF 有 Workerd 覆盖，改密仅浏览器 mock 覆盖 |
| 概览 | 两次 `GET /api/usage-logs/v2/overview?from&to&bucket=hour` | `web/src/index.ts:192-199` 转 `getTraceOverviewV2`；成功含 `totals/trend/recent_failures`，过滤/权限/内部错误为 V2 envelope | 路径与字段匹配；“今日”时区口径不一致见 F-08 |
| 版本策略 | `GET /api/app-versions` + `GET /api/app-versions/summary`；创建 `POST /api/app-versions` `{version,min_version,download_url,note}`；编辑 `PUT /api/app-versions/:id`；删除 `DELETE` | `web/src/index.ts:166-175,350-407`；创建 `201`，重复 `409`，其它成功多为 `200 {ok:true}` | 基本匹配；目标不存在/空更新和 URL 校验缺口见 F-03/F-04 |
| 用户管理 | `GET /api/users`；创建 `POST /api/users` `{username,name,password,note}`；`PUT /api/users/:id`（`enabled/banned/note/newPassword`）；`DELETE`；`POST /api/users/:id/rotate-token` | `web/src/index.ts:176-182,413-485`；创建 `201` 回 token，轮换 `200` 回 token | 路径、方法匹配；UI 未验证 token，后端未知 ID 仍成功见 F-02/F-03 |
| 在线会话 | `GET /api/online`；强制下线 `POST /api/online/kick` `{sessionId,reason}` | `web/src/index.ts:187-189,516-581`；管理员列表含 username/IP/session_id/force_exit，kick 成功 `200`、目标不存在 `404`、缺目标 `400` | 请求匹配；UI 忽略服务端 `force_exit`，见 F-05 |
| 审计用户/运行列表 | `GET /api/usage-logs/v2/users?from&to&status&q&limit&cursor`；`GET /runs?userId&kind&status&from&to&partition&errorCode&q&limit&cursor` | `trace-v2-query.ts:220-275`、`269-275`；默认 50、上限 200、opaque keyset cursor | 路径和分页字段匹配；`q` 50 UTF-8 字节限制未传达到 UI，见 F-06 |
| 审计详情 / 输出 | `GET /runs/{traceRef}`、`GET /runs/{traceRef}/events/{eventId}`、`GET .../output?stream&afterChunk&limit` | `web/src/index.ts:201-217`；V1 明确降级，V2 输出分 stream 分页且先写审计 | 基本匹配；run detail 未校验 route userId，见 F-07 |
| ROM 查询 | `GET /api/rom-logs/v2?userId&pd&version&status&q&cursor` | `web/src/index.ts:220-221`、`trace-v2-query.ts:475-551`；HTTP status、opaque cursor、URL 字段 | 路径和字段匹配；失败原因实际恒为 legacy unavailable，见 F-09 |
| 导出 | UI 构造同源 `GET /api/usage-logs/v2/export`，只带非分页 runs 过滤；服务器返回 NDJSON attachment 并先写 `export_trace` 审计 | `audit.js:651-679`；`trace-v2-query.ts:424-450,596-640` | 成功路径匹配；错误与中途 stream 失败不可被 UI 感知，见 F-01 |

普通 admin 错误体为 `{error:string}`；冻结 V2/ROM 路径的 `401/403/400/404/500` 为 `{ok:false,error:{code,message,request_id}}`。`api.js:99-119,259-286` 能归一化这两类，且普通页面的 401 会触发集中回登录；导出原生 anchor 是唯一绕过该集中处理的路径。

## 值得修复的缺陷（按影响等级）

### F-01 [P1] 原生导出绕过 401/403/500 处理，可能把错误体当作成功下载

证据：`cloudflare/web/src/admin/pages/audit.js:651-679` 直接创建隐藏 `<a download>` 并 `click()`，不调用 `api.request`；集中 401 处理只存在于 `cloudflare/web/src/admin/api.js:99-108`。服务器导出在 `cloudflare/web/src/trace-v2-query.ts:424-450` 先返回 200/NDJSON，后续批次异常则在 `:606-639` 对已经开始的 stream `controller.error()`。

复现：会话过期或返回 403/500 时点击“导出当前筛选 NDJSON”。前端仍立即将按钮文字改为“导出已开始”并永久禁用；不会调用 `onUnauthorized`、不会显示 retry/错误状态。中途 D1 失败时 HTTP 状态已经是 200，原生下载也无法告诉操作员文件不完整。

预期：导出必须把认证、状态码和下载失败作为可观察的 UI 状态；401 应回登录，403/500 应保留上下文并可重试，不能把 JSON 错误或截断 NDJSON 误报为成功。

建议：由受控下载流程先完成带 cookie 的状态检查并处理 401/403/5xx，再启动下载；或设计带可验证完成标记/临时下载票据的下载协议。服务器 stream 失败也应有可被客户端检测的完整性信号。补真实浏览器非 2xx 与中途失败测试。

### F-02 [P1] 创建/轮换用户未验证一次性 token，已提交 mutation 时可能丢失唯一凭据

证据：`cloudflare/web/src/admin/pages/users.js:190-221` 只在 `typeof result?.token === "string"` 时保存 token，但无 token 仍执行成功 announcement；创建 API 的契约在 `cloudflare/web/src/index.ts:420-440` 明确返回 token，轮换在 `:478-484` 先写新 token 再返回。

只读 DOM probe：让 `rotateUserToken()` 返回 `{ok:true}`，页面仍记录“令牌已轮换，请立即保存”，但 DOM 没有一次性令牌；让创建返回 `{ok:true,id:2}`，页面仍记录“用户已创建；一次性令牌仅显示一次”。

影响：轮换会立即使旧 token 失效；响应丢字段、代理篡改或服务端异常包装为 200 时，管理员拿不到新 token，用户被锁在不可用状态。创建则产生无法交付 token 的账号。

建议：把 `{ok:true,token}`（创建再加安全正整数 `id`）作为严格成功结构；缺字段进入“提交结果未知/需刷新”状态，不发成功 toast、不隐藏问题，并提供安全的重新轮换恢复路径。增加真实响应畸形测试。

### F-03 [P2] CRUD 目标/字段校验不足，未知 ID 和无效 mutation 普遍返回 200

证据：`cloudflare/web/src/index.ts:374-407,443-485` 对 id 只做 `Number.isFinite(Number(...))`，没有正安全整数/精确路径校验，也不检查 D1 `meta.changes`。因此不存在的版本或用户的 update/delete/rotate 可返回 `200 {ok:true}`；`rotate` 甚至会生成一个未写入任何行的 token。`updateUser` 对短于 6 位的 `newPassword` 静默忽略；版本编辑表单 `cloudflare/web/src/admin/pages/versions.js:264-284` 允许清空 `min_version`，后端 `:384-387` 静默不更新但 UI 宣布成功。`startsWith`/`endsWith` 路由匹配还接受额外路径段（`:173-182`）。

影响：并发删除/陈旧页面操作会产生虚假成功；错误 target、短密码或空最低版本不会被操作员察觉；宽松路径扩大了未声明 API 表面。

建议：使用 `^/api/(app-versions|users)/([1-9][0-9]*)$` 等精确路由；拒绝非正安全整数、空/无可识别字段和短密码；检查 `meta.changes`，未知资源返回 404，必要时将多字段更新放入单一事务。前端对不可清空字段和结果结构做同样校验。

### F-04 [P2] 删除用户不清理在线会话/租约，管理员列表与概览/客户端列表会分裂

证据：`web/src/index.ts:471-475` 的 `deleteUser` 只删除 `api_users`；`web/schema.sql:63-89` 没有 FK/CASCADE。管理员在线列表 `web/src/index.ts:520-545` 通过 `JOIN api_users` 会隐藏孤儿行，但概览 `trace-v2-query.ts:386-410` 和 API Worker 客户端在线查询 `cloudflare/src/index.ts:723-743` 仍按 `online_sessions` 计数/返回。

影响：删除账号后，最长一个在线窗口内仍可能显示该账号的旧在线投影；概览在线数可能大于后台列表，残留 `session_leases` 也会继续占用数据，直到清理任务运行。

建议：删除用户时以 D1 batch/事务同时清理 `session_leases`、`online_sessions`（或统一 orphan 过滤策略），并补删除后 admin/API/overview 一致性测试。

### F-05 [P2] 在线会话页面忽略服务端 `force_exit`，刷新后可重复踢同一会话

证据：服务端在 `web/src/index.ts:533-545` 返回 `force_exit`；页面 `cloudflare/web/src/admin/pages/sessions.js:154-171` 只依据本地 `pendingKickIds` 决定按钮，不读取 `session.force_exit`。

只读 DOM probe：`getOnlineSessions()` 返回 `{sessions:[{session_id:"s1",force_exit:true}]}` 时，页面按钮仍为“强制下线”且 `disabled=false`。

预期：服务端已标记的会话应显示“下线请求已发送/等待确认”并禁用重复提交，直到该行不再由权威列表返回；另一管理员或页面刷新也必须保留该状态。

建议：渲染时将 `force_exit === true` 合并进 pending 集合，显示受限原因（若契约允许返回 `force_exit_reason`），并覆盖刷新/跨管理员场景。

### F-06 [P2] 查询 `q` 的 50-byte 服务端边界没有映射到表单，合法 URL 会稳定得到 400 retry

证据：`cloudflare/web/src/trace-v2-query.ts:911-915` 对转义后的 LIKE pattern 限制 50 UTF-8 bytes；路由 `cloudflare/web/src/admin/router.js:59-60` 允许 q 长 256 字符；审计输入 `audit.js:576-584` 也设为 `maxlength=256`，ROM 输入 `rom.js:67-74,143-147` 没有等价 byte 限制。

只读 probe：20 个中文字符（60 UTF-8 bytes）通过审计页面仍发出 `{q,limit:50}`；真实 Worker 会按 `likePattern` 返回 400，页面只进入“重试”状态。ASCII 也要扣除 pattern 两侧 `%` 及转义开销。

建议：把限制提取为共享常量/byte-aware validator，在输入旁说明剩余字节并在提交前阻止；status/userId 等同样应使用 number/枚举或本地校验，避免把可预见的 400 伪装成网络失败。

### F-07 [P2] 从用户层进入运行详情时未验证返回 run 的 userId

证据：`cloudflare/web/src/admin/pages/audit.js:809-823` 的 `requireRunDetail` 校验 `trace_ref/source_schema/run_id`，但没有像同文件 `:862-871` 的 `requireEventDetail` 那样比较 `route.userId` 与 `value.run.user_id`。用户层点击运行时 `userId` 会保留在路由，而 `GET /runs/{traceRef}` 本身不带 userId。

影响：若出现陈旧、错配或代理返回，UI 可在某用户上下文展示另一用户的运行摘要，层级证据边界不一致。

建议：在 run detail 与后续 event/detail 链路统一执行 userId 一致性检查，失败关闭并提供 retry；补错配 fixture。

### F-08 [P2] 概览“今日”与版本 summary 的时区口径不一致

证据：`cloudflare/web/src/admin/pages/overview.js:46-51` 用浏览器本地午夜计算 daily `from`；`cloudflare/web/src/trace-v2-query.ts:459-470` 的 `today_426` 固定按 UTC 日起点计算。当前时区为 Asia/Shanghai 时，daily 操作与“今日更新拦截”可能相差 8 小时。

建议：统一使用服务端返回的 UTC 日边界，或明确所有卡片按本地日统计并让 summary 同口径；在页面文案和 Workerd 测试中固定时区语义。

### F-09 [P2] ROM “搜索失败原因”与实际持久化字段不符

证据：ROM 表单 placeholder `cloudflare/web/src/admin/pages/rom.js:67-74` 宣称“搜索 URL/失败原因”；后端 `trace-v2-query.ts:492-495` 的 q 列不含 failure reason，映射 `:535-546` 对 access_logs 行恒为 `failure_reason:null`，失败只给 `legacy_record_no_failure_reason`。`web/README.md:14` 也宣称工作区有 failure reason。

影响：操作员会认为关键词能命中失败原因，实际任何新旧 ROM 记录都不能按该字段筛选；失败诊断信息缺失且文案误导。

建议：若产品需要该能力，新增并持久化受控 `failure_reason` 字段后再加入 q；否则移除 placeholder/README 的“失败原因”，明确当前仅有 legacy unavailable 标记。

### F-10 [P2] 首次管理员初始化与后台部署契约未写入文档

证据：Worker 依赖 `ADMIN_SEED_PASSWORD`/`ADMIN_SEED_USERNAME`（`cloudflare/web/src/index.ts:38-45,251-263`），但 `cloudflare/web/README.md` 没有 seed secret、建表、首次登录、改密后删除 seed 的步骤，也没有后台 Worker 的 `wrangler deploy --config web/wrangler.toml` 说明。根 `cloudflare/README.md:3-5,51-53` 又把 D1/后台说明指向该 README。`cloudflare/API.md:482-485` 只列后台地址和功能，没有任何 admin API 路由、字段、状态码、分页或导出契约。

影响：新环境按现文档无法可靠创建第一个管理员；发布者可能只部署 API Worker，或把同名 `/api/login`、`/api/me`、`/api/online` 的 bearer/API 响应误当成 web cookie/admin 响应。

建议：补一份以 `https://web.nwflash.cc.cd` 为 host 的单一 admin API 表（含 auth、CRUD、V2/ROM、分页、导出和错误 envelope），并补 seed/deploy/回滚/移除 seed 的授权运维步骤；在 `API.md` 开头明确 API host 与 web host 的同名路径边界。

## 跨 host 的契约风险（需转 API owner，不在本卡改动）

这些问题由 `cloudflare/API.md` 与共享 API Worker 交叉核对发现，记录在本报告以免管理员/客户端联调时遗漏：

1. `API.md:10,154-171` 声称版本头 `X-Nwflash-Version` 必填且无跳过；`cloudflare/src/index.ts:878-892` 只有 header 非空才比较，缺头会直接放行，构成旧客户端绕过最低版本的 P1 门禁缺陷。应补缺头行为（400 或 426）及 login/me/rom/heartbeat/authorize/usage/traces 测试，并明确 pins/telemetry/crash/online 的豁免范围。
2. `API.md:9` 仍写 Bearer 可选/匿名，和 `:365-405`、`:473` 的 ROM 必须登录相冲；示例 `:418-423` 的 curl 没有 Authorization 或版本头，却标成 200/404。示例会误导联调，应按 host/endpoint 重写。
3. `API.md` 完全没有 `POST /api/usage/traces/v2` 的生产者契约，无法解释后台 V2 审计数据来源；也没有记录 V1 usage log 的 `event_id`/`details` 字段（API Worker `cloudflare/src/index.ts:763-819`）。
4. `API.md:280-304` 将 `is_self` 定义为“当前 token 的会话”，但 `cloudflare/src/index.ts:733-740` 仅按 `user_id === auth.id`，同一用户多设备会全部标记 true；应改字段语义或增加 session identity。
5. API Worker 对声明为 GET 的 `/health`、`/api/me`、`/api/rom` 未统一检查方法（`cloudflare/src/index.ts:85-87,117-124,167-179`）；同时 app/version 等认证/策略响应没有统一 no-store。建议由 API owner 补 405/Allow、缓存和方法矩阵测试。

## 测试覆盖缺口与建议测试卡

门禁全绿不能证明普通 admin API 的后端契约完整，当前缺口如下：

- `cloudflare/test/admin-static.workerd.test.ts` 的 `/api/me` 200 是空库匿名 `{loggedIn:false}`，没有真实成功登录、错误密码、缺字段、cookie 属性或会话写入测试；`change-password` 也没有 Workerd+D1 矩阵。
- 普通版本/user CRUD、rotate、online list/kick 的真实 400/401/403/404/409/500 和 D1 mutation 结果大多只有页面 mock；应加入 table-driven Workerd 测试，验证 `meta.changes`、孤儿清理、CSRF 和 envelope。
- `api.test.js` 没有覆盖所有 wrapper 的精确 path/method/body/query，也缺普通 400/404/409 normalization；建议逐个 wrapper 断言编码、credentials、CSRF 和 `request_id/details`。
- 若干 Playwright fixtures 按 pathname 直接 fulfill，未严格校验方法/请求头/请求体（如 `admin-shell.spec.ts`、`admin-workspaces.spec.ts`），可能把错误方法误报为通过；应对 mutation 断言 JSON 字段和 `X-Requested-With`，未匹配则 fallback/404。
- 浏览器错误矩阵缺 overview/audit/ROM/session 初次加载失败、导出 401/403/500、V2 run ownership mismatch、畸形 200、长 Unicode q、`force_exit` 刷新恢复。
- V2/ROM 使用 keyset cursor，但 `/api/users`、`/api/app-versions`、管理员 `/api/online` 仍返回无界全量列表（`web/src/index.ts:350-417,516-545`）；应确定规模上限、分页或明确容量门槛。

## 本卡结论

路径/方法的正常成功链路总体一致，217 个相关测试（135 admin unit、51 admin Workerd、31 browser）以及 API 侧 269 个测试（77 Node、192 Workerd）均通过；但上述 F-01～F-10 中 F-01～F-04、F-05、F-06、F-10 足以进入后续修复筛选，尤其是导出错误可见性、版本下载 URL、token 丢失和 mutation authority。F-07～F-09 可作为同一轮审计/查询 UX 修复。公共 API 版本头缺失绕过（跨 host 第 1 项）应单独转交 API owner。

计划与实际差异：按任务卡完成只读测试、页面顺序核对、文档/实现比较和缺陷筛选；没有实现修复、部署、提交或删除生成物。  
修改文件：仅 `docs/2026-09-04-web-api-validation.md`。  
Commit：无（等待总指挥下一张实现卡）。
