# 统一固件来源与操作栏 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 合并固件页面的本地与 HTTP 来源入口，并提供读取信息、提取文件、停止操作三按钮工作流。

**Architecture:** `FirmwareExtractPage` 用一个来源状态保存本地选择或远程 URL。来源变更只重置检查结果；统一的 `inspectSource` 依据来源分派到既有 Tauri command，提取分派保持原有边界。

**Tech Stack:** React、TypeScript、Vitest、Tauri dialog/invoke。

## Global Constraints

- 不新增 Rust command 或浏览器路径输入能力。
- 本地绝对路径不进入页面可见文本。
- 远程地址继续只接受 HTTP(S)。

---

### Task 1: 来源选择与读取信息

**Files:**
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.tsx`
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx`

- [x] **Step 1: 写失败测试**

验证本地选择不会立即调用检查命令，点击“读取信息”后才调用 `firmware_inspect_local`；验证 HTTP(S) 输入点击同一按钮后调用 `firmware_inspect_remote`。

- [x] **Step 2: 运行测试确认失败**

Run: `npm --prefix src/Nwflash.Desktop test -- FirmwareExtractPage.test.tsx`

Expected: FAIL，因为当前本地选择自动检查且远程读取由独立按钮触发。

- [x] **Step 3: 最小实现**

删除来源模式切换和独立远程检查按钮；实现统一来源状态及 `inspectSource` 分派。保留原生本地文件选择和 HTTP(S) 格式检查。

- [x] **Step 4: 运行测试确认通过**

Run: `npm --prefix src/Nwflash.Desktop test -- FirmwareExtractPage.test.tsx`

Expected: PASS。

### Task 2: 三按钮操作栏与完整验证

**Files:**
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.tsx`
- Modify: `src/Nwflash.Desktop/src/pages/FirmwareExtractPage.test.tsx`

- [x] **Step 1: 写失败测试**

验证操作栏同时含“读取信息”“提取文件”“停止操作”，且提取按钮在未读取或未选择分区时禁用。

- [x] **Step 2: 运行测试确认失败**

Run: `npm --prefix src/Nwflash.Desktop test -- FirmwareExtractPage.test.tsx`

Expected: FAIL，因为当前没有统一读取信息按钮。

- [x] **Step 3: 最小实现**

在既有操作栏加入 `读取信息`；提取按钮文案改为 `提取文件`，并以来源已检查、选中分区和工作状态控制可用性。

- [x] **Step 4: 完整验证与提交**

Run: `npm --prefix src/Nwflash.Desktop test`

Run: `npm --prefix src/Nwflash.Desktop run build`

Run: `git commit -m "feat(ui): unify firmware source actions"`
