# VivoKsu Cloudflare Worker(ROM OTA 代理)

> **接口契约见 [API.md](API.md)** —— 端点、参数、响应、错误码、计费、功能记录。

把 VivoKsu 的服务端搬到 Cloudflare Workers,部署在 **`api.nwflash.cc.cd`**。
桌面应用把 `OtaApiClient` 的 `DefaultBaseUrl` 指到 `https://api.nwflash.cc.cd` 即可,无需本地起 .NET 服务端。

上游仍是 **VOTA `https://api.otau.cc.cd`**(不改动),Worker 只做代理,凭据(VOTA_API_TOKEN)留在服务端。

## 端点
- `GET /health` → `{ status: "ok", source: "VotaApiRomSource" }`
- `GET /api/rom?pd=X&version=Y` → `{ pd, version, url, name, sizeBytes, sha256 }`
- 错误映射:NOT_FOUND/`not found`→404, AUTH_FAIL→401, INSUFFICIENT_CREDITS→402, FORBIDDEN→403, RATE_LIMITED→429, 其它→502。

## 首次部署
```bash
cd cloudflare
npm install
npx wrangler login            # 浏览器登录 Cloudflare 账户(域名 nwflash.cc.cd 需已在账户内)
npx wrangler secret put VOTA_API_TOKEN    # 粘贴 VOTA 的 API Token
npx wrangler deploy           # 部署并绑定自定义域 api.nwflash.cc.cd
```

部署后验证:
```bash
curl "https://api.nwflash.cc.cd/health"
curl "https://api.nwflash.cc.cd/api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1"
```

## 桌面应用切换
`src/VivoKsu.App/Services/OtaApiClient.cs` 的 `DefaultBaseUrl` 改为 `https://api.nwflash.cc.cd`。

## 机密
- `VOTA_API_TOKEN` 用 secret 存,不进代码。非机密项在 `wrangler.toml [vars]`:
  `VOTA_BASE_URL`(默认 https://api.otau.cc.cd)、`VOTA_ACTION`(resolve_url / resolve_flash_url)、`VOTA_VER`(0.1.0)。
