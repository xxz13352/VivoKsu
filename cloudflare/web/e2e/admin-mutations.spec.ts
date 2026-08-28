import { expect, test, type Page } from "@playwright/test";

import { createTask12ApiState, installTask12Api } from "./admin-api-fixtures";

function monitorRuntime(page: Page, allowedHttpStatuses: number[] = []) {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    const expectedResourceFailure = message.text().startsWith("Failed to load resource:")
      && allowedHttpStatuses.some((status) => message.text().includes(`status of ${status}`));
    if (!expectedResourceFailure) errors.push(`console: ${message.text()}`);
  });
  page.on("pageerror", (error) => errors.push(`pageerror: ${error.message}`));
  return errors;
}

test("keeps a 403 mutation contextual, single-flight, and keyboard recoverable", async ({ page }) => {
  const state = createTask12ApiState({ mutationStatus: { deleteVersion: 403 } });
  const errors = monitorRuntime(page, [403]);
  await installTask12Api(page, state);
  await page.goto("/?view=versions");

  const trigger = page.locator('[data-action="delete-version"]').first();
  await trigger.click();
  const dialog = page.getByRole("dialog");
  const confirm = dialog.locator('[data-dialog-action="confirm"]');
  await page.evaluate(() => {
    const button = document.querySelector<HTMLButtonElement>('[data-dialog-action="confirm"]');
    button?.click();
    button?.click();
  });
  await expect(dialog.getByRole("alert")).toContainText("无权执行");
  await expect(confirm).toBeFocused();
  await expect(page.getByRole("heading", { name: "版本策略", level: 1 })).toBeVisible();
  const deletes = state.requests.filter((request) => request.method === "DELETE" && request.pathname.startsWith("/api/app-versions/"));
  expect(deletes).toHaveLength(1);
  expect(deletes[0].headers["x-requested-with"]).toBe("XMLHttpRequest");
  await dialog.getByRole("button", { name: "取消" }).click();
  await expect(trigger).toBeFocused();

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});

test("fails closed to login on a protected 401 and clears token and dialog state", async ({ page }) => {
  const state = createTask12ApiState({ mutationStatus: { rotateToken: 401 } });
  const errors = monitorRuntime(page, [401]);
  await installTask12Api(page, state);
  await page.goto("/?view=users");

  await page.locator('[data-action="rotate-token"]').first().click();
  await page.getByRole("dialog").getByRole("button", { name: "轮换令牌", exact: true }).click();
  await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText(/task12-one-time-token/)).toHaveCount(0);

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});

test("retains a 500 failure without optimistic removal and does not duplicate the request", async ({ page }) => {
  const state = createTask12ApiState({ mutationStatus: { deleteUser: 500 } });
  const errors = monitorRuntime(page, [500]);
  await installTask12Api(page, state);
  await page.goto("/?view=users");

  await page.locator('[data-action="delete-user"]').first().click();
  const dialog = page.getByRole("dialog");
  await page.evaluate(() => {
    const button = document.querySelector<HTMLButtonElement>('[data-dialog-action="confirm"]');
    button?.click();
    button?.click();
  });
  await expect(dialog.getByRole("alert")).toContainText("服务器暂时无法处理请求");
  await expect(page.getByText("alice", { exact: true })).toBeVisible();
  expect(state.requests.filter((request) => request.method === "DELETE" && request.pathname === "/api/users/7"))
    .toHaveLength(1);

  expect(state.unmocked).toEqual([]);
  expect(errors).toEqual([]);
});
