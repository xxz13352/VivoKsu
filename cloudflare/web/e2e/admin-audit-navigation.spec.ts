import { expect, test, type Page } from "@playwright/test";

const TRACE_REF = "v2:019d9c40-7b3c-7000-8000-000000000002";
const RUN_ID = TRACE_REF.slice(3);
const EVENT_ID = "019d9c40-7b3c-7000-8000-000000000003";

const run = {
  source_schema: 2,
  trace_ref: TRACE_REF,
  run_id: RUN_ID,
  legacy_id: null,
  user_id: 7,
  username: "alice",
  user_name: "Alice Zhang",
  operation_kind: "fastboot_flash",
  title: "VIVO 线刷",
  outcome: "success",
  client_version: "1.4.0",
  started_at_ms: 1_787_500_000_000,
  ended_at_ms: 1_787_500_002_500,
  duration_ms: 2_500,
  trace_complete: true,
  trace_loss_reason: null,
};

const events = Array.from({ length: 4 }, (_, index) => ({
  event_id: index === 0
    ? EVENT_ID
    : `019d9c40-7b3c-7000-8000-${String(index + 1).padStart(12, "0")}`,
  run_id: RUN_ID,
  sequence: index + 1,
  kind: "stage",
  step_name: `Persisted step ${index + 1}`,
  partition_name: null,
  status: "success",
  started_at_ms: 1_787_500_000_000 + index,
  ended_at_ms: null,
  duration_ms: null,
  command: null,
  exit_code: null,
  stdout_chunks: 0,
  stderr_chunks: 0,
  verification: null,
  device_state: null,
  retry_safe: null,
  remedies: [],
  error_class: null,
  error_code: null,
  error_message: null,
  credential_redactions: [],
}));

async function installAuditApi(page: Page) {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = decodeURIComponent(url.pathname);
    if (path === "/api/me") {
      await route.fulfill({ json: { loggedIn: true, username: "operator" } });
      return;
    }
    if (path === "/api/usage-logs/v2/users") {
      await route.fulfill({
        json: {
          items: [{
            user_id: 7,
            username: "alice",
            name: '<img src=x onerror="globalThis.pwned=true">Alice Zhang',
            operation_count: 1,
            failed_count: 0,
            last_operation: run,
            last_activity_at_ms: run.ended_at_ms,
          }],
          next_cursor: null,
        },
      });
      return;
    }
    if (path === "/api/usage-logs/v2/runs") {
      expect(url.searchParams.get("userId")).toBe("7");
      await route.fulfill({ json: { items: [run], next_cursor: null } });
      return;
    }
    if (path === `/api/usage-logs/v2/runs/${TRACE_REF}`) {
      await route.fulfill({
        json: { source_schema: 2, detail_available: true, detail_unavailable_reason: null, run, events },
      });
      return;
    }
    await route.fulfill({ status: 404, json: { error: "Unmocked audit API route" } });
  });
}

test("drills real app routes from user to ordered persisted events and restores run focus on Back", async ({ page }) => {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  await installAuditApi(page);
  await page.goto("/?view=audit");

  const userButton = page.getByRole("button", { name: /Alice Zhang/ });
  await expect(userButton).toBeVisible();
  await expect(page.locator("img")).toHaveCount(0);
  await userButton.click();
  await expect(page).toHaveURL(/view=audit.*userId=7.*level=user/);

  const runButton = page.getByRole("button", { name: /VIVO 线刷/ });
  const runFocusId = await runButton.getAttribute("data-router-focus-id");
  await runButton.click();
  await expect(page).toHaveURL(/view=audit.*runId=v2%3A.*level=run/);
  await expect(page.locator("[data-event-sequence]")).toHaveText(["1", "2", "3", "4"]);

  await page.goBack();
  await expect(page).toHaveURL(/view=audit.*userId=7.*level=user/);
  await expect(page.locator(`[data-router-focus-id="${runFocusId}"]`)).toBeFocused();
  expect(errors).toEqual([]);
});

test("stops a legacy run at its persisted summary without requesting event or output APIs", async ({ page }) => {
  let forbiddenDetailRequests = 0;
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    if (path === "/api/me") {
      await route.fulfill({ json: { loggedIn: true, username: "operator" } });
      return;
    }
    if (path === "/api/usage-logs/v2/runs/v1:42") {
      await route.fulfill({
        json: {
          source_schema: 1,
          detail_available: false,
          detail_unavailable_reason: "legacy_client_no_step_data",
          run: {
            ...run,
            source_schema: 1,
            trace_ref: "v1:42",
            run_id: null,
            legacy_id: 42,
            title: "Legacy VIVO flash",
            outcome: "unknown",
            trace_complete: false,
            trace_loss_reason: "legacy_client_no_step_data",
          },
          events: [],
        },
      });
      return;
    }
    if (path.includes("/events/") || path.endsWith("/output")) forbiddenDetailRequests += 1;
    await route.fulfill({ status: 404, json: { error: "Unmocked audit API route" } });
  });

  await page.goto("/?view=audit&runId=v1%3A42&level=run");

  await expect(page.getByText("旧客户端未上传步骤数据", { exact: true })).toBeVisible();
  await expect(page.locator("[data-event-sequence]")).toHaveCount(0);
  expect(forbiddenDetailRequests).toBe(0);
});
