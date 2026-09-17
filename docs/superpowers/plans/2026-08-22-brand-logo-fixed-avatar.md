# 品牌 Logo 与固定头像 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在已登录主界面显示登录页同款品牌 Logo，并显示不可修改的统一头像。

**Architecture:** 主框架继续导入既有 `logo.jpg`，不创建第二个 Logo。指定 JPG 被复制到前端资产目录，由 `Sidebar` 直接导入并渲染为静态圆形头像；账号、会话和登出数据流维持原样。

**Tech Stack:** React、TypeScript、Vite、Vitest、CSS。

## Global Constraints

- 固定头像源文件：`C:\Users\17254\Downloads\CEACC1A77693466C769A4EE631F407A2.jpg`。
- 头像只能是前端打包的本地静态资源；没有上传、选择、编辑、远程下载或配置入口。
- Logo 复用 `src/VivoKsu.App/Assets/logo.jpg`。
- 不改动会话、认证、设备或刷写逻辑。

---

### Task 1: 固定头像资源与侧栏呈现

**Files:**
- Create: `src/Nwflash.Desktop/src/assets/default-avatar.jpg`
- Modify: `src/Nwflash.Desktop/src/components/Sidebar.tsx`
- Modify: `src/Nwflash.Desktop/src/styles/components.css`
- Test: `src/Nwflash.Desktop/src/components/AppShell.test.tsx`

**Interfaces:**
- Consumes: `Sidebar` 的现有 `username`、`currentTime`、`onLogout` props。
- Produces: 账号区内说明为“默认头像”的只读 `<img>`，不新增 props 或交互事件。

- [x] **Step 1: 写入失败测试**

```tsx
const avatar = host.querySelector('img[alt="默认头像"]') as HTMLImageElement;
expect(avatar).not.toBeNull();
expect(avatar.classList.contains('nw-sidebar-avatar')).toBe(true);
expect(host.querySelector('input[type="file"]')).toBeNull();
```

- [x] **Step 2: 运行测试确认失败**

Run: `npm --prefix src/Nwflash.Desktop test -- AppShell.test.tsx`

Expected: FAIL，因为侧栏当前未渲染固定头像。

- [x] **Step 3: 添加最小实现**

复制指定 JPG 到 `src/assets/default-avatar.jpg`；在 `Sidebar.tsx` 导入资源并把下列无交互图片放到账号名旁：

```tsx
<img className="nw-sidebar-avatar" src={defaultAvatarUrl} alt="默认头像" />
```

在 `components.css` 为 `.nw-sidebar-avatar` 设置固定圆形尺寸、`object-fit: cover` 和不可收缩布局；不添加按钮或输入框。

- [x] **Step 4: 运行测试确认通过**

Run: `npm --prefix src/Nwflash.Desktop test -- AppShell.test.tsx`

Expected: PASS。

### Task 2: 品牌与完整前端验证

**Files:**
- Modify: `src/Nwflash.Desktop/src/components/AppShell.test.tsx`
- Verify: `src/Nwflash.Desktop/src/components/AppShell.tsx`

**Interfaces:**
- Consumes: `AppShell` 对 `logo.jpg` 的既有静态导入。
- Produces: 主框架品牌区仍含实际 Logo 图片，且固定头像改动不影响现有登出行为。

- [x] **Step 1: 写入失败测试**

在品牌区测试中断言实际品牌图片存在；若现有 DOM 已满足，该现有覆盖即为绿色基线。

- [x] **Step 2: 运行完整验证**

Run: `npm --prefix src/Nwflash.Desktop test`

Run: `npm --prefix src/Nwflash.Desktop run build`

Expected: 所有前端测试和 Vite 构建通过。

- [x] **Step 3: 提交完成的界面改动**

Run: `git add src/Nwflash.Desktop/src/assets/default-avatar.jpg src/Nwflash.Desktop/src/components/Sidebar.tsx src/Nwflash.Desktop/src/components/AppShell.test.tsx src/Nwflash.Desktop/src/styles/components.css docs/superpowers/plans/2026-08-22-brand-logo-fixed-avatar.md`

Run: `git commit -m "feat(ui): add fixed account avatar"`
