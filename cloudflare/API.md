# api.nwflash.cc.cd —— API 文档

`api.nwflash.cc.cd` 是 VivoKsu 的 ROM OTA 链接代理服务(Cloudflare Worker `nwflash-rom`)。它唯一持有 VOTA API Token,接收客户端的 **PD + 版本号**,转发到 VOTA 取回 OTA 下载链接。客户端无需任何鉴权/凭据。

- **Base URL**: `https://api.nwflash.cc.cd`
- **上游**: `https://api.otau.cc.cd`(VOTA,不对外暴露,不改动)
- **协议**: HTTPS + JSON
- **CORS**: 已允许跨域(`Access-Control-Allow-Origin: *`),浏览器 / 桌面 / 脚本均可直连

## 端点

### `GET /health`

健康检查。

**响应 200**
```json
{ "status": "ok", "source": "VotaApiRomSource" }
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `status` | string | 固定 `ok` |
| `source` | string | 数据源类型(当前恒为 `VotaApiRomSource`,即真实 VOTA 代理) |

---

### `GET /api/rom?pd=<PD>&version=<版本>`

按 **PD 码 + 版本号** 解析 OTA 下载链接。

**参数**

| 参数 | 必填 | 说明 | 示例 |
| --- | --- | --- | --- |
| `pd` | ✅ | 设备 PD 码(`ro.product.device`) | `PD2417` |
| `version` | ✅ | 固件版本号(VOTA 平台记录的值,通常来自设备 `ro.build.version.bbk` 最后一段) | `16.2.12.0.W10.V000L1` |

**成功响应 200**
```json
{
  "pd": "PD2417",
  "version": "16.2.12.0.W10.V000L1",
  "url": "https://sysuptxdl.vivo.com.cn/upgrade/oem/files/20260723141715...zip?sign=...&t=...",
  "name": null,
  "sizeBytes": null,
  "sha256": null
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `pd` | string | 回显请求的 PD |
| `version` | string | 回显请求的版本号 |
| `url` | string | OTA 包下载直链(带 `sign`/`t` 签名参数,**有时效性,拿到后尽快下载**) |
| `name` | string \| null | 包名(上游常为空) |
| `sizeBytes` | long \| null | 包大小字节(上游常为空) |
| `sha256` | string \| null | 包 SHA-256(上游常为空) |

> `url` 的 `sign`/`t` 参数由 VOTA 生成,可能有时效;一次查询拿到的链接应尽快使用。

**错误响应**(`Content-Type: application/json`)

| HTTP | `error` 示例 | 含义 |
| --- | --- | --- |
| `400` | `缺少 pd 或 version 查询参数。` | 缺参数 |
| `401` | `AUTH_FAIL` 文本 | VOTA 认证失败(worker 的 token 无效 / 被吊销) |
| `402` | `INSUFFICIENT_CREDITS` 文本 | 账户信用点不足(每次成功查询扣信用点) |
| `403` | `FORBIDDEN` 文本 | VOTA 拒绝(VOTA_VER 不在白名单等) |
| `404` | `record not found` | **该 PD + 版本在平台无记录**(最常见) |
| `429` | `RATE_LIMITED` 文本 | 请求过于频繁 |
| `500` | `服务端未配置 VOTA 凭据。` / `内部错误。` | worker 缺 token / 未捕获异常 |
| `502` | `无法连接上游 ROM API。` / `上游返回异常。` | 连不上 VOTA 或上游响应异常 |

**示例**

```bash
curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1"
# → 200,带真实 OTA 链接

curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=99.99"
# → 404 {"error":"record not found"}
```

## 计费 / 信用点

每次成功调用 `resolve_url` 扣 **1 信用点**;`resolve_flash_url`(线刷包)扣 **3 信用点**。信用点归属 worker 所持 token 的账户。`record not found` / 参数错误不扣点。当前余额可在 VOTA 平台查看。

## 配置

非机密项在 `cloudflare/wrangler.toml` 的 `[vars]`:

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `VOTA_BASE_URL` | `https://api.otau.cc.cd` | 上游地址 |
| `VOTA_ACTION` | `resolve_url` | `resolve_url`(OTA,-1)/ `resolve_flash_url`(线刷,-3) |
| `VOTA_VER` | `0.1.0` | 平台客户端版本白名单 |

机密(不进代码,`wrangler secret put` 设置):

| 机密 | 说明 |
| --- | --- |
| `VOTA_API_TOKEN` | VOTA 的 API Token(Authorization: Bearer)。**只在 worker 上,绝不下发客户端** |

## 部署

```bash
cd cloudflare
npm install
npx wrangler login
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 token
npx wrangler deploy                       # 绑定 api.nwflash.cc.cd
```

## 功能记录(Changelog)

| 日期 | 变更 |
| --- | --- |
| 2026-08-13 | 初始部署:worker `nwflash-rom`,自定义域 `api.nwflash.cc.cd`;`/health` + `/api/rom` 代理 VOTA `resolve_url`;token 存 worker 机密;错误映射 400/401/402/403/404/429/500/502 |
| (规划) | 登录系统、后台系统等将加在此 API 上 |

## 代码结构

```
cloudflare/
├─ src/index.ts        # Worker 入口:路由 + resolveRom + 错误映射
├─ wrangler.toml       # 变量与自定义域路由
├─ README.md           # 部署/使用说明
└─ API.md              # 本文档(接口契约)
```
