import { expect, test } from "./admin-test";
import { createTask12ApiState, installTask12Api } from "./admin-api-fixtures";

test("renders the authoritative overview trend and drills a recent failure", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=overview");

  await expect(page.locator("[data-trend-bucket]")).toHaveCount(1);
  await expect(page.locator('[data-trend-value="operations"]')).toHaveText("12");
  await page.locator(".overview-failure-button").click();
  await expect.poll(() => {
    const url = new URL(page.url());
    return url.searchParams.get("view") === "audit"
      && url.searchParams.get("level") === "run"
      && url.searchParams.get("runId")?.startsWith("v2:");
  }).toBe(true);
  expect(state.unmocked).toEqual([]);
});

test("creates, filters, toggles, and edits version policy with authoritative reloads", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=versions");

  const search = page.locator('[data-filter="version-q"]');
  await search.fill("missing");
  await expect(page.locator(".version-list")).not.toContainText("2.0.0");
  await search.fill("2.0");
  await page.locator('[name="version"]').fill("3.0.0");
  await page.locator('[data-form="create-version"] [name="min_version"]').fill("2.0.0");
  await page.locator('[data-form="create-version"] [name="download_url"]').fill("https://example.test/v3.zip");
  await page.locator('[data-form="create-version"] [name="note"]').fill("next");
  await page.locator('[data-form="create-version"]').getByRole("button", { name: "登记版本" }).click();
  await expect(page.getByText("3.0.0", { exact: true })).toBeVisible();

  await page.locator('[data-version-id="9"]').locator("xpath=ancestor::li").locator('[data-action="toggle-version"]').click();
  await expect.poll(() => Number(state.versions.find((version) => version.id === 9)?.enabled)).toBe(0);
  const row = page.locator('[data-version-id="9"]').locator("xpath=ancestor::li");
  await row.locator('[data-action="edit-version"]').click();
  await row.locator('[data-form="edit-version"] [name="min_version"]').fill("1.5.0");
  await row.locator('[data-form="edit-version"]').getByRole("button", { name: "保存" }).click();
  await expect.poll(() => state.versions.find((version) => version.id === 9)?.min_version).toBe("1.5.0");

  expect(state.requests.some((request) => request.method === "POST" && request.pathname === "/api/app-versions")).toBe(true);
  expect(state.requests.filter((request) => request.method === "PUT" && request.pathname === "/api/app-versions/9")).toHaveLength(2);
  expect(state.unmocked).toEqual([]);
});

test("creates, searches, resets, and toggles users without leaking the one-time token", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=users");

  const search = page.locator('[data-filter="user-q"]');
  await search.fill("missing");
  await expect(page.locator(".user-list")).not.toContainText("alice");
  await search.fill("alice");
  await page.locator('[data-form="create-user"] [name="username"]').fill("charlie");
  await page.locator('[data-form="create-user"] [name="name"]').fill("Charlie");
  await page.locator('[data-form="create-user"] [name="password"]').fill("password-123");
  await page.locator('[data-form="create-user"]').getByRole("button", { name: "创建用户" }).click();
  await expect(page.getByText("一次性令牌：task13-created-token")).toBeVisible();

  const alice = page.getByText("alice", { exact: true }).locator("..");
  await alice.locator('[data-user-password="7"]').fill("replacement-123");
  await alice.locator('[data-action="reset-password"]').click();
  await expect.poll(() => state.requests.some((request) => request.pathname === "/api/users/7" && request.body?.includes("replacement-123"))).toBe(true);
  await page.getByText("alice", { exact: true }).locator("..").locator('[data-action="toggle-user-enabled"]').click();
  await expect.poll(() => Number(state.users.find((user) => user.id === 7)?.enabled)).toBe(0);
  await expect(page.getByText("一次性令牌：task13-created-token")).toBeVisible();

  await page.getByRole("button", { name: "在线会话" }).click();
  await expect(page.getByText("一次性令牌：task13-created-token")).toHaveCount(0);
  expect(state.unmocked).toEqual([]);
});

test("shows complete session evidence and sends an operator kick reason", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=sessions");

  await expect(page.getByText("会话：session-alice-001")).toBeVisible();
  await expect(page.getByText("IP：203.0.113.7")).toBeVisible();
  await expect(page.getByText(/上线：2026/).first()).toBeVisible();
  await page.locator('[data-kick-reason="session-alice-001"]').fill("operator requested logout");
  await page.locator('[data-action="kick-session"]').first().click();
  await page.getByRole("dialog").getByRole("button", { name: "下线", exact: true }).click();
  await expect.poll(() => state.requests.some((request) => request.pathname === "/api/online/kick" && request.body?.includes("operator requested logout"))).toBe(true);
  expect(state.unmocked).toEqual([]);
});

test("shows ROM user/time evidence and round-trips documented filters", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=rom");

  await expect(page.getByText("用户：Alice（7）")).toBeVisible();
  await expect(page.getByText(/时间：2026/).first()).toBeVisible();
  for (const [name, value] of [["userId", "7"], ["pd", "PD1"], ["version", "1.0"], ["status", "200"], ["q", "rom.zip"]]) {
    await page.locator(`[data-form="rom-filters"] [name="${name}"]`).fill(value);
  }
  await page.locator('[data-form="rom-filters"]').getByRole("button", { name: "应用筛选" }).click();
  await expect(page).toHaveURL(/view=rom.*userId=7.*pd=PD1.*version=1.0.*status=200.*q=rom.zip/);
  await expect.poll(() => state.requests.some((request) => request.url.includes("/api/rom-logs/v2?") && request.url.includes("pd=PD1"))).toBe(true);
  expect(state.unmocked).toEqual([]);
});

test("submits and resets audit filters through the URL and API contract", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=audit");

  const form = page.locator('[data-audit-filter-form="true"]');
  await form.locator('[name="from"]').fill("2026-08-01T00:00:00Z");
  await form.locator('[name="status"]').fill("failed");
  await form.locator('[name="kind"]').fill("fastboot_flash");
  await form.locator('[name="partition"]').fill("boot_a");
  await form.locator('[name="errorCode"]').fill("FLASH_FAILED");
  await form.locator('[name="q"]').fill("device failed");
  await form.getByRole("button", { name: "应用筛选" }).click();
  await expect(page).toHaveURL(/view=audit.*status=failed.*kind=fastboot_flash.*partition=boot_a.*errorCode=FLASH_FAILED.*q=device\+failed/);
  await expect.poll(() => state.requests.some((request) => request.url.includes("/api/usage-logs/v2/runs?")
    && request.url.includes("status=failed")
    && request.url.includes("kind=fastboot_flash")
    && request.url.includes("partition=boot_a")
    && request.url.includes("errorCode=FLASH_FAILED"))).toBe(true);

  await page.locator('[data-audit-filter-form="true"]').getByRole("button", { name: "重置筛选" }).click();
  await expect(page).toHaveURL(/\?view=audit&level=overview$/);
  expect(state.unmocked).toEqual([]);
});
