# nwflash.cc.cd —— 奶蛙Flash 官网

Nwflash(奶蛙Flash,NWF)对外营销落地页(Cloudflare Worker `nwflash-site`,高级白 + 液态玻璃设计)。纯静态单页,不鉴权、不连 D1;Worker 仅托管 `index.html`(任何路径统一返回)。

## 界面「高级白 + 液态玻璃」

对外营销面 = **冷白画布 `#F5F7F9` + 液态玻璃**(Apple Liquid Glass 网页近似),与暗色管理台、用户门户形成明暗双面:

- **液态玻璃面板**:磨砂半透明白 `linear-gradient(135deg, rgba(255,255,255,.68), rgba(255,255,255,.28))` + `backdrop-filter: blur(22px) saturate(170%)` + 内高光 `inset 0 1px 0 rgba(255,255,255,.9)` + 发丝描边 + 顶部渐变光沿;`prefers-reduced-transparency` 回落为近实白。
- **单一深青强调 `#0C7C74`**(文字/链接/CTA 对比安全)+ 亮青 `#2FD6C8`(发光/填充/非文字层);violet 仅品牌点缀;数据一律 Cascadia Mono。
- **签名**:HERO 液态玻璃设备控制台(真实迷你 UI:设备卡 + 分区进度条循环 + OKAY 日志流水 + 荧光光束)+ 动能排字标题。
- WCAG AA(ink 14:1 / muted 5.8:1)、焦点环、`prefers-reduced-motion` 全部冻结、移动端单列折叠、零 em-dash。

## 区块

| 区块 | 内容 |
| --- | --- |
| HERO | 动能排字 + 设备控制台迷你 UI + 下载 CTA |
| 技术栈条 | adb / fastboot / KernelSU / magiskboot / scrcpy / payload_dumper |
| 功能 Bento | 8 格非对称(设备概览 / ROOT / 快速刷写 / 可视刷写 / 投屏 / 固件提取 / 文件管理 / VIVO 线刷) |
| 更新日志 | 版本登记册(发版时在此登记版本号与变更) |
| 下载 CTA | 下载入口 + 商用授权说明 |

## 更新日志维护

发版时在 `src/index.html` 的「更新日志」区块登记新版本:复制上一版 `.rel` 玻璃卡片,更新版本号 / 日期 / 变更条目,并把最新的置为「当前」。

## 目录

```
website/
├─ wrangler.toml       # 自定义域 nwflash.cc.cd(根域)
├─ package.json        # wrangler 依赖
└─ src/
   ├─ index.ts         # Worker:托管 index.html + 安全头 + HTTPS 跳转
   └─ index.html       # 官网单页(内联 CSS/JS,高级白液态玻璃)
```

## 部署

```bash
cd cloudflare/website
npx wrangler deploy     # 绑定 nwflash.cc.cd
```

> 根域 `nwflash.cc.cd` 需已在 Cloudflare 账户内;api./web./user. 子域为独立 Worker,互不影响。
