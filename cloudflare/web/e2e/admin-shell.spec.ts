import { expect, test, type Page } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import { isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { adminMe, loginSuccess, logoutSuccess } from "./admin-api-fixtures";

const menuLabels = ["概览", "版本策略", "用户管理", "在线会话", "操作审计", "ROM 查询"];

async function installApiFixtures(page: Page, state: { authenticated: boolean; loginAccepted?: boolean }) {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") {
      await route.fulfill({ json: adminMe(state.authenticated) });
      return;
    }
    if (path === "/api/login" && request.method() === "POST") {
      if (state.loginAccepted === false) {
        await route.fulfill({ status: 401, json: { error: "用户名或密码错误。" } });
        return;
      }
      state.authenticated = true;
      await route.fulfill({ json: loginSuccess });
      return;
    }
    if (path === "/api/logout" && request.method() === "POST") {
      state.authenticated = false;
      await route.fulfill({ json: logoutSuccess });
      return;
    }
    if (path === "/api/change-password" && request.method() === "POST") {
      await route.fulfill(state.authenticated
        ? { json: { ok: true } }
        : { status: 401, json: { error: "会话已过期。" } });
      return;
    }
    if (path === "/api/usage-logs/v2/overview") {
      await route.fulfill({ json: {
        totals: { api_users: 0, online_sessions: 0, operations: 0, failed: 0 },
        trend: [],
        recent_failures: [],
      } });
      return;
    }
    if (path === "/api/app-versions") {
      await route.fulfill({ json: { versions: [] } });
      return;
    }
    if (path === "/api/app-versions/summary") {
      await route.fulfill({ json: {
        current_version: null,
        minimum_version: null,
        supported_versions: [],
        today_426: 0,
      } });
      return;
    }
    if (path === "/api/users") {
      await route.fulfill({ json: { users: [] } });
      return;
    }
    if (path === "/api/online") {
      await route.fulfill({ json: { sessions: [], count: 0 } });
      return;
    }
    if (path === "/api/rom-logs/v2") {
      await route.fulfill({ json: { items: [], next_cursor: null } });
      return;
    }
    if (path === "/api/usage-logs/v2/users") {
      await route.fulfill({ json: { items: [], next_cursor: null } });
      return;
    }
    const runMatch = path.match(/^\/api\/usage-logs\/v2\/runs\/(.+)$/);
    if (runMatch && !path.includes("/events/")) {
      const traceRef = decodeURIComponent(runMatch[1]);
      const runId = traceRef.startsWith("v2:") ? traceRef.slice(3) : null;
      await route.fulfill({ json: {
        source_schema: 2,
        detail_available: true,
        detail_unavailable_reason: null,
        run: {
          source_schema: 2,
          trace_ref: traceRef,
          run_id: runId,
          legacy_id: null,
          user_id: 7,
          username: "fixture-user",
          user_name: "Fixture User",
          operation_kind: "unknown",
          title: "Fixture run",
          outcome: "unknown",
          client_version: "0.0.0",
          started_at_ms: 0,
          ended_at_ms: null,
          duration_ms: null,
          trace_complete: false,
          trace_loss_reason: "fixture",
        },
        events: [],
      } });
      return;
    }
    await route.fulfill({ status: 404, json: { error: "Unmocked API route" } });
  });
}

test("restores an authenticated six-menu shell with keyboard navigation", async ({ page }) => {
  const state = { authenticated: true };
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  await installApiFixtures(page, state);

  expect((await page.request.get("/admin/app.js")).status()).toBe(200);
  expect((await page.request.get("/app.js")).status()).toBe(404);

  await page.goto("/?view=overview");
  await expect(page.getByRole("main")).toBeVisible();
  const menu = page.getByRole("navigation", { name: "主菜单" });
  await expect(menu.getByRole("button")).toHaveCount(6);
  await expect(menu.getByRole("button").allTextContents()).resolves.toEqual(menuLabels);
  await expect(menu.locator('[aria-current="page"]')).toHaveCount(1);

  const first = menu.getByRole("button", { name: "概览" });
  await first.focus();
  await first.press("ArrowRight");
  await expect(menu.getByRole("button", { name: "版本策略" })).toBeFocused();
  await page.keyboard.press("End");
  await expect(menu.getByRole("button", { name: "ROM 查询" })).toBeFocused();
  await page.keyboard.press("Home");
  await expect(first).toBeFocused();

  await menu.getByRole("button", { name: "操作审计" }).click();
  await expect(page).toHaveURL(/\?view=audit/);
  await expect(menu.getByRole("button", { name: "操作审计" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("操作审计");
  expect(errors).toEqual([]);
});

test("supports safe deep links, login errors, Back focus, logout, and active 401 recovery", async ({ page }) => {
  const state = { authenticated: false, loginAccepted: false };
  await installApiFixtures(page, state);
  await page.goto("/?view=audit&level=run&runId=v2%3A019d0000-0000-7000-8000-000000000001&token=top-secret");
  await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();
  await expect(page).toHaveURL(/\?view=audit&runId=.*&level=run$/);
  await expect(page).not.toHaveURL(/token|top-secret/);

  await page.getByLabel("用户名").fill("operator");
  await page.getByLabel("密码").fill("wrong password");
  await page.getByRole("button", { name: "登录" }).click();
  await expect(page.getByRole("alert")).toContainText("用户名或密码错误");
  await expect(page.getByLabel("用户名")).toHaveValue("operator");

  state.loginAccepted = true;
  await page.getByLabel("密码").fill("correct horse battery staple");
  await page.getByRole("button", { name: "登录" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("操作审计");

  const romButton = page.getByRole("navigation", { name: "主菜单" }).getByRole("button", { name: "ROM 查询" });
  await romButton.click();
  await expect(page).toHaveURL(/\?view=rom/);
  await page.goBack();
  await expect(page).toHaveURL(/\?view=audit/);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("操作审计");
  await expect(romButton).toBeFocused();

  await page.getByRole("button", { name: "账户" }).click();
  await page.getByRole("button", { name: "退出登录" }).click();
  await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();

  state.authenticated = true;
  await page.reload();
  await expect(page.getByRole("main")).toBeVisible();
  state.authenticated = false;
  await page.getByRole("button", { name: "账户" }).click();
  await page.getByRole("button", { name: "修改密码" }).click();
  await page.getByLabel("新密码", { exact: true }).fill("replacement-password");
  await page.getByLabel("确认新密码").fill("replacement-password");
  await page.getByRole("button", { name: "保存并重新登录" }).click();
  await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();
  await expect(page.getByRole("alert")).toContainText("会话已失效");
});

test("keeps labels, targets, and layout intact at base responsive widths", async ({ page }) => {
  const state = { authenticated: true };
  await installApiFixtures(page, state);
  const screenshotRoot = validatedScreenshotRoot();
  if (screenshotRoot) await mkdir(screenshotRoot, { recursive: true });

  for (const width of [320, 360, 768, 1024, 1440]) {
    await page.setViewportSize({ width, height: 820 });
    await page.goto("/?view=overview");
    const menu = page.getByRole("navigation", { name: "主菜单" });
    const buttons = menu.getByRole("button");
    await expect(buttons).toHaveCount(6);
    for (const label of menuLabels) {
      const button = menu.getByRole("button", { name: label });
      await expect(button).toBeVisible();
      if (width === 320) {
        const box = await button.boundingBox();
        expect(box?.x ?? -1).toBeGreaterThanOrEqual(0);
        expect((box?.x ?? width) + (box?.width ?? 1)).toBeLessThanOrEqual(width);
      }
    }
    const target = await buttons.first().boundingBox();
    expect(target?.height ?? 0).toBeGreaterThanOrEqual(44);
    expect(await page.evaluate(() => ({
      body: document.body.scrollWidth - document.body.clientWidth,
      html: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    }))).toEqual({ body: 0, html: 0 });
    await buttons.first().focus();
    if (screenshotRoot) {
      await page.screenshot({ path: resolve(screenshotRoot, `admin-shell-${width}.png`), fullPage: true });
    }
  }
});

function validatedScreenshotRoot(): string | null {
  const requested = process.env.NWFLASH_ADMIN_SCREENSHOT_DIR;
  if (!requested) return null;
  if (!isAbsolute(requested)) throw new Error("NWFLASH_ADMIN_SCREENSHOT_DIR must be absolute");
  const workspaceRoot = fileURLToPath(new URL("../../../", import.meta.url));
  const relation = relative(workspaceRoot, requested);
  if (!relation.startsWith("..") || isAbsolute(relation)) {
    throw new Error("NWFLASH_ADMIN_SCREENSHOT_DIR must be outside the workspace");
  }
  return requested;
}
