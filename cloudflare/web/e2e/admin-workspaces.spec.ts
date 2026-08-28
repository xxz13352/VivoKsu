import { expect, test, type Page } from "@playwright/test";

type WorkspaceState = {
  versionDeletes: number;
  tokenRotations: number;
  userDeletes: number;
  kicks: number;
  onlineReads: number;
  romQueries: string[];
};

async function installWorkspaceApi(page: Page, state: WorkspaceState = emptyState()) {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const { pathname } = url;
    if (pathname === "/api/me") return route.fulfill({ json: { loggedIn: true, username: "operator" } });
    if (pathname === "/api/usage-logs/v2/overview") return route.fulfill({ json: {
      totals: { api_users: 7, online_sessions: 2, operations: 44, failed: 2 }, trend: [],
      recent_failures: [{ title: "flash-device", outcome: "failed" }],
    } });
    if (pathname === "/api/app-versions/9" && request.method() === "DELETE") {
      state.versionDeletes += 1;
      return route.fulfill({ json: { ok: true } });
    }
    if (pathname === "/api/app-versions") return route.fulfill({ json: {
      versions: state.versionDeletes ? [] : [{ id: 9, version: "2.0.0", min_version: "1.0.0", enabled: 1 }],
    } });
    if (pathname === "/api/app-versions/summary") return route.fulfill({ json: {
      current_version: "2.0.0", minimum_version: "1.0.0", supported_versions: ["2.0.0"], today_426: 3,
    } });
    if (pathname === "/api/users/7/rotate-token" && request.method() === "POST") {
      state.tokenRotations += 1;
      return route.fulfill({ json: { ok: true, token: "one-time-token" } });
    }
    if (pathname === "/api/users/7" && request.method() === "DELETE") {
      state.userDeletes += 1;
      return route.fulfill({ json: { ok: true } });
    }
    if (pathname === "/api/users") return route.fulfill({ json: {
      users: state.userDeletes ? [] : [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }],
    } });
    if (pathname === "/api/online/kick" && request.method() === "POST") {
      state.kicks += 1;
      return route.fulfill({ json: { ok: true, affected: 1 } });
    }
    if (pathname === "/api/online") {
      state.onlineReads += 1;
      const sessions = state.onlineReads >= 3
        ? [{ session_id: "s-2", user_id: 8, username: "bob", name: "Bob", client_version: "1.2.4" }]
        : [
          { session_id: "s-1", user_id: 7, username: "alice", name: "Alice", client_version: "1.2.3" },
          { session_id: "s-2", user_id: 8, username: "bob", name: "Bob", client_version: "1.2.4" },
        ];
      return route.fulfill({ json: { sessions } });
    }
    if (pathname === "/api/rom-logs/v2") {
      const cursor = url.searchParams.get("cursor") ?? "";
      state.romQueries.push(cursor);
      if (cursor) return route.fulfill({ json: { items: [
        { id: 4, user_name: "Bob", pd: "PD4", version: "2.0", status: 200, url: "https://example.test/second.zip", failure_reason: null, detail_unavailable_reason: null },
      ], next_cursor: null } });
      return route.fulfill({ json: { items: [
        { id: 1, user_name: "Alice", pd: "PD1", version: "1.0", status: 200, url: "https://example.test/rom.zip", failure_reason: null, detail_unavailable_reason: null },
        { id: 2, user_name: "Legacy", pd: "PD2", version: "1.0", status: 500, url: null, failure_reason: null, detail_unavailable_reason: "legacy_record_no_failure_reason" },
        { id: 3, user_name: "Unsafe", pd: "PD3", version: "1.0", status: 200, url: "https://user:password@example.test/secret.zip", failure_reason: null, detail_unavailable_reason: null },
      ], next_cursor: "opaque-cursor-123" } });
    }
    return route.fulfill({ status: 404, json: { error: "Unmocked API route" } });
  });
  return state;
}

function emptyState(): WorkspaceState {
  return { versionDeletes: 0, tokenRotations: 0, userDeletes: 0, kicks: 0, onlineReads: 0, romQueries: [] };
}

function deferred() {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((nextResolve) => { resolve = nextResolve; });
  return { promise, resolve };
}

test("renders authoritative fields and safe ROM evidence in all five workspaces", async ({ page }) => {
  await installWorkspaceApi(page);
  await page.goto("/?view=overview");
  await expect(page.getByRole("heading", { name: "权威运行概览" })).toBeVisible();
  await expect(page.getByText("44", { exact: true })).toBeVisible();
  await expect(page.getByText("flash-device · failed")).toBeVisible();

  await page.getByRole("button", { name: "版本策略" }).click();
  await expect(page.getByRole("heading", { name: "版本策略", level: 2 })).toBeVisible();
  await expect(page.getByText("当前版本：2.0.0 · 最低版本：1.0.0")).toBeVisible();
  await expect(page.getByText("今日更新拦截：3")).toBeVisible();

  await page.getByRole("button", { name: "用户管理" }).click();
  await expect(page.getByRole("heading", { name: "用户管理", level: 2 })).toBeVisible();
  await expect(page.getByText("Alice", { exact: true })).toBeVisible();
  await expect(page.getByText("正常", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "在线会话" }).click();
  await expect(page.getByRole("heading", { name: "在线会话", level: 2 })).toBeVisible();
  await expect(page.getByText("1.2.3", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "ROM 查询" }).click();
  await expect(page.getByRole("heading", { name: "ROM 查询", level: 2 })).toBeVisible();
  await expect(page.getByRole("link", { name: "打开下载地址" })).toHaveAttribute("href", "https://example.test/rom.zip");
  await expect(page.getByText("https://example.test/rom.zip", { exact: true })).toBeVisible();
  await expect(page.getByText("旧记录未保存失败原因。")).toBeVisible();
  await expect(page.getByText("PD3 · 1.0").locator("..").getByRole("link")).toHaveCount(0);
});

test("confirms mutations once, reloads authority, and clears the one-time token on navigation", async ({ page }) => {
  const state = await installWorkspaceApi(page);
  const deleteGate = deferred();
  await page.route("**/api/app-versions/9", async (route) => {
    if (route.request().method() !== "DELETE") return route.fallback();
    state.versionDeletes += 1;
    await deleteGate.promise;
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto("/?view=versions");
  const deleteVersion = page.locator('[data-action="delete-version"]');
  await deleteVersion.click();
  await expect(deleteVersion).toBeDisabled();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  const confirmDelete = dialog.getByRole("button", { name: "删除", exact: true });
  await confirmDelete.click();
  await expect.poll(() => state.versionDeletes).toBe(1);
  await expect(confirmDelete).toBeDisabled();
  await page.evaluate(() => document.querySelector<HTMLButtonElement>('[data-dialog-action="confirm"]')?.click());
  expect(state.versionDeletes).toBe(1);
  deleteGate.resolve();
  await expect(page.getByText("没有已配置的版本策略。")).toBeVisible();

  await page.getByRole("button", { name: "用户管理" }).click();
  await page.locator('[data-action="rotate-token"]').click();
  await page.getByRole("dialog").getByRole("button", { name: "轮换令牌", exact: true }).click();
  await expect.poll(() => state.tokenRotations).toBe(1);
  await expect(page.getByText("一次性令牌：one-time-token")).toBeVisible();
  await page.getByRole("button", { name: "在线会话" }).click();
  await expect(page.getByText("一次性令牌：one-time-token")).toHaveCount(0);

  await page.getByRole("button", { name: "用户管理" }).click();
  await page.locator('[data-action="delete-user"]').click();
  await page.getByRole("dialog").getByRole("button", { name: "删除", exact: true }).click();
  await expect.poll(() => state.userDeletes).toBe(1);
  await expect(page.getByText("没有 API 用户。")).toBeVisible();
});

test("destroys stale version confirmation on Back and menu navigation without mutation or alert", async ({ page }) => {
  const state = await installWorkspaceApi(page);
  await page.goto("/?view=overview");
  await page.getByRole("button", { name: "版本策略" }).click();
  await page.locator('[data-action="delete-version"]').click();
  const backDialog = page.getByRole("dialog");
  await expect(backDialog).toBeVisible();
  const staleBackConfirm = await backDialog.locator('[data-dialog-action="confirm"]').elementHandle();

  await page.goBack();
  await expect(page).toHaveURL(/\?view=overview$/);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await staleBackConfirm?.evaluate((button) => (button as HTMLButtonElement).click());
  expect(state.versionDeletes).toBe(0);
  await expect(page.locator('[role="alert"]')).toHaveCount(0);

  await page.getByRole("button", { name: "版本策略" }).click();
  await page.locator('[data-action="delete-version"]').click();
  const menuDialog = page.getByRole("dialog");
  await expect(menuDialog).toBeVisible();
  const staleMenuConfirm = await menuDialog.locator('[data-dialog-action="confirm"]').elementHandle();
  await page.evaluate(() => document.querySelector<HTMLButtonElement>('[data-menu-id="users"]')?.click());

  await expect(page).toHaveURL(/\?view=users$/);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await staleMenuConfirm?.evaluate((button) => (button as HTMLButtonElement).click());
  expect(state.versionDeletes).toBe(0);
  await expect(page.locator('[role="alert"]')).toHaveCount(0);
});

test("keeps session authority while kick is pending and pauses polling when hidden", async ({ page }) => {
  await page.addInitScript(() => {
    let visibility = "visible";
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => visibility });
    (window as Window & { setAdminTestVisibility: (value: string) => void }).setAdminTestVisibility = (value) => {
      visibility = value;
      document.dispatchEvent(new Event("visibilitychange"));
    };
  });
  await page.clock.install();
  const state = await installWorkspaceApi(page);
  await page.goto("/?view=sessions");
  await expect(page.getByText("alice", { exact: true })).toBeVisible();
  await expect.poll(() => state.onlineReads).toBe(1);

  await page.locator('[data-action="kick-session"]').first().click();
  await page.getByRole("dialog").getByRole("button", { name: "下线", exact: true }).click();
  await expect.poll(() => state.kicks).toBe(1);
  await expect(page.getByText("正在等待服务器确认会话移除。")).toBeVisible();
  await expect(page.getByText("alice", { exact: true })).toBeVisible();
  await expect(page.getByText("bob", { exact: true })).toBeVisible();
  await expect.poll(() => state.onlineReads).toBe(2);

  await page.evaluate(() => (window as Window & { setAdminTestVisibility: (value: string) => void }).setAdminTestVisibility("hidden"));
  await page.clock.fastForward(10_000);
  expect(state.onlineReads).toBe(2);
  await page.evaluate(() => (window as Window & { setAdminTestVisibility: (value: string) => void }).setAdminTestVisibility("visible"));
  await expect.poll(() => state.onlineReads).toBe(3);
  await expect(page.getByText("alice", { exact: true })).toHaveCount(0);
  await expect(page.getByText("bob", { exact: true })).toBeVisible();
});

test("uses the opaque ROM cursor in history and ignores a deferred stale workspace response", async ({ page }) => {
  const state = await installWorkspaceApi(page);
  await page.goto("/?view=rom");
  await page.locator('[data-action="next-page"]').click();
  await expect(page).toHaveURL(/\?view=rom&cursor=opaque-cursor-123$/);
  await expect(page.getByText("PD4 · 2.0")).toBeVisible();
  await expect.poll(() => state.romQueries).toEqual(["", "opaque-cursor-123"]);
  await page.goBack();
  await expect(page).toHaveURL(/\?view=rom$/);
  await expect.poll(() => state.romQueries).toEqual(["", "opaque-cursor-123", ""]);

  let resolveUsers: (() => void) | null = null;
  await page.unroute("**/api/**");
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname === "/api/me") return route.fulfill({ json: { loggedIn: true, username: "operator" } });
    if (pathname === "/api/users") {
      await new Promise<void>((resolve) => { resolveUsers = resolve; });
      return route.fulfill({ json: { users: [{ id: 9, username: "late-user", name: "Late User", enabled: 1, banned: 0 }] } });
    }
    if (pathname === "/api/usage-logs/v2/overview") return route.fulfill({ json: { totals: { api_users: 1, online_sessions: 0, operations: 0, failed: 0 }, trend: [], recent_failures: [] } });
    return route.fulfill({ json: {} });
  });
  await page.goto("/?view=users");
  await expect(page.getByRole("heading", { name: "用户管理" })).toBeVisible();
  await page.getByRole("button", { name: "概览" }).click();
  await expect(page.getByRole("heading", { name: "概览", exact: true, level: 1 })).toBeVisible();
  await expect.poll(() => resolveUsers !== null).toBe(true);
  resolveUsers?.();
  await expect(page.getByText("late-user", { exact: true })).toHaveCount(0);
});

test("keeps task-eight workspace content inside the narrow viewport", async ({ page }) => {
  await installWorkspaceApi(page);
  await page.setViewportSize({ width: 320, height: 820 });
  await page.goto("/?view=rom");
  await expect(page.getByRole("heading", { name: "ROM 查询", level: 2 })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth === document.documentElement.clientWidth)).toBe(true);
});
