import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

import {
  createTask12ApiState,
  installTask12Api,
  task12EventId,
  task12MaliciousText,
  task12RunId,
} from "./admin-api-fixtures";

const views = ["overview", "versions", "users", "sessions", "audit", "rom"];
const readySelectors: Record<string, string> = {
  overview: ".overview-workspace",
  versions: ".version-list",
  users: ".user-list",
  sessions: ".session-list",
  audit: "[data-audit-action='open-user']",
  rom: ".rom-list",
};

function monitorRuntime(page: Page) {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(`console: ${message.text()}`);
  });
  page.on("pageerror", (error) => errors.push(`pageerror: ${error.message}`));
  page.on("requestfailed", (request) => {
    const failure = request.failure()?.errorText ?? "request failed";
    if (!/aborted|cancelled|ERR_ABORTED/i.test(failure)) errors.push(`${failure}: ${request.url()}`);
  });
  return errors;
}

async function expectNoAxeViolations(page: Page) {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(result.violations, JSON.stringify(result.violations, null, 2)).toEqual([]);
}

test("passes axe on login, every primary workspace, and a destructive dialog", async ({ page }) => {
  const state = createTask12ApiState({ authenticated: false });
  const errors = monitorRuntime(page);
  await installTask12Api(page, state);

  await page.goto("/?view=overview");
  await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();
  await expectNoAxeViolations(page);

  state.authenticated = true;
  for (const view of views) {
    await page.goto(`/?view=${view}`);
    await expect(page.getByRole("main")).toBeVisible();
    await expect(page.locator(readySelectors[view])).toBeVisible();
    await expectNoAxeViolations(page);
  }

  for (const { route, ready } of [
    { route: "?view=audit&level=user&userId=7", ready: "[data-audit-action='open-run']" },
    { route: `?view=audit&level=run&runId=${encodeURIComponent(task12RunId)}`, ready: "[data-audit-action='open-event']" },
    { route: `?view=audit&level=command&runId=${encodeURIComponent(task12RunId)}&eventId=${task12EventId}`, ready: "[data-command-field='paths']" },
    { route: `?view=audit&level=output&runId=${encodeURIComponent(task12RunId)}&eventId=${task12EventId}&stream=stdout`, ready: "[data-output-stream='stdout']" },
  ]) {
    await page.goto(`/${route}`);
    await expect(page.locator(ready)).toBeVisible();
    await expectNoAxeViolations(page);
  }

  await page.goto("/?view=versions");
  const trigger = page.locator('[data-action="delete-version"]').first();
  await trigger.click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute("aria-describedby", /confirmation-message-/);
  await expect(dialog.getByRole("button", { name: "取消" })).toBeFocused();
  await expectNoAxeViolations(page);
  await page.keyboard.press("Shift+Tab");
  await expect(dialog.getByRole("button", { name: "删除", exact: true })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(dialog.getByRole("button", { name: "取消" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(trigger).toBeFocused();

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});

test("restores focus from the account password form and keeps malicious API text inert", async ({ page }) => {
  const state = createTask12ApiState();
  const errors = monitorRuntime(page);
  await installTask12Api(page, state);
  await page.goto("/?view=overview");

  const account = page.getByRole("button", { name: /账户/ });
  await account.click();
  await page.getByRole("button", { name: "修改密码" }).click();
  await expect(page.getByLabel("新密码", { exact: true })).toBeFocused();
  await page.getByRole("button", { name: "取消" }).click();
  await expect(account).toBeFocused();

  for (const view of ["versions", "users", "rom"]) {
    await page.getByRole("button", { name: view === "versions" ? "版本策略" : view === "users" ? "用户管理" : "ROM 查询" }).click();
    await expect(page.locator(".workspace-card")).toBeVisible();
  }
  await expect(page.locator("#admin-app script:not([src]), #admin-app img, #admin-app iframe, #admin-app svg"))
    .toHaveCount(0);
  await expect(page.locator("#admin-app [onerror], #admin-app [onclick], #admin-app [onload]"))
    .toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as typeof globalThis & { pwned?: boolean }).pwned)).toBeUndefined();
  await expect(page.getByText(task12MaliciousText, { exact: true }).first()).toBeVisible();
  const links = await page.locator("#admin-app a[href]").evaluateAll((elements) => elements.map((element) => {
    const link = element as HTMLAnchorElement;
    return { href: link.href, rel: link.rel, target: link.target };
  }));
  expect(links.every((link) => {
    const url = new URL(link.href);
    return ["http:", "https:"].includes(url.protocol)
      && !url.username
      && !url.password
      && (link.target !== "_blank" || (link.rel.includes("noopener") && link.rel.includes("noreferrer")));
  })).toBe(true);
  const accessibleNames = await page.locator(".rom-url a").evaluateAll((elements) =>
    elements.map((element) => element.getAttribute("aria-label")));
  expect(new Set(accessibleNames).size).toBe(accessibleNames.length);
  expect(await page.evaluate(async () => {
    const { createSafeElement } = await import("/admin/components.js");
    try {
      createSafeElement(document, "a", { href: "/\\evil.example/path" }, "mixed slash");
      return false;
    } catch {
      return true;
    }
  })).toBe(true);

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});

test("uses keyboard-only audit pagination, drill-down, and breadcrumb navigation", async ({ page }) => {
  const state = createTask12ApiState();
  const errors = monitorRuntime(page);
  await installTask12Api(page, state);
  await page.goto("/?view=audit");

  const next = page.getByRole("navigation", { name: "审计分页" }).getByRole("button", { name: "下一页" });
  await next.focus();
  await next.press("Enter");
  await expect(page).toHaveURL(/cursor=task12-users-next/);
  await page.goBack();
  const user = page.locator('[data-audit-action="open-user"]');
  await user.focus();
  await user.press("Enter");
  await expect(page).toHaveURL(/level=user/);
  const run = page.locator('[data-audit-action="open-run"]');
  await run.focus();
  await run.press("Enter");
  await expect(page).toHaveURL(/level=run/);
  const operations = page.getByRole("navigation", { name: "审计层级" }).getByRole("button", { name: "操作" });
  await operations.focus();
  await operations.press("Enter");
  await expect(page).toHaveURL(/level=user/);

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});
