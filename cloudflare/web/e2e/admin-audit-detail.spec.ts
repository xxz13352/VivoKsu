import { expect, test, type Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const TRACE_REF = "v2:019d9c40-7b3c-7000-8000-000000000002";
const RUN_ID = TRACE_REF.slice(3);
const EVENT_ID = "019d9c40-7b3c-7000-8000-000000000003";
const LONG_PATH = `C:\\firmware\\${"very-long-segment-".repeat(28)}boot.img`;

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
  outcome: "unknown",
  client_version: "1.4.0",
  started_at_ms: 1_787_500_000_000,
  ended_at_ms: 1_787_500_000_100,
  duration_ms: 100,
  trace_complete: true,
  trace_loss_reason: null,
};

const event = {
  event_id: EVENT_ID,
  run_id: RUN_ID,
  sequence: 1,
  kind: "command",
  step_name: "Flash boot_a",
  partition_name: "boot_a",
  status: "unknown",
  started_at_ms: 1_787_500_000_000,
  ended_at_ms: 1_787_500_000_100,
  duration_ms: 100,
  command: {
    program: '<img src=x onerror="globalThis.pwned=true">fastboot.exe',
    argv: ["flash", "boot_a", "</pre><script>not markup</script>"],
    display_command: "fastboot flash boot_a",
    working_directory: null,
    paths: [LONG_PATH],
    urls: ["https://downloads.example/boot.img"],
    serial: "FAKE-SERIAL",
  },
  exit_code: 0,
  stdout_chunks: 3,
  stderr_chunks: 0,
  verification: null,
  device_state: "fastboot",
  retry_safe: false,
  remedies: [],
  error_class: null,
  error_code: null,
  error_message: null,
  credential_redactions: [],
};

async function installDetailApi(page: Page, exportedUrls: string[]) {
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    if (path === "/api/me") {
      await route.fulfill({ json: { loggedIn: true, username: "operator" } });
      return;
    }
    if (path === `/api/usage-logs/v2/runs/${TRACE_REF}/events/${EVENT_ID}`) {
      await route.fulfill({ json: { run, event } });
      return;
    }
    if (path.endsWith(`/events/${EVENT_ID}/output`)) {
      const stream = url.searchParams.get("stream");
      const afterChunk = Number(url.searchParams.get("afterChunk"));
      if (stream === "stderr") {
        await route.fulfill({
          json: {
            run_id: RUN_ID,
            event_id: EVENT_ID,
            stream: "stderr",
            chunks: [],
            next_after_chunk: null,
            output_complete: true,
          },
        });
        return;
      }
      const first = afterChunk === -1;
      await route.fulfill({
        json: {
          run_id: RUN_ID,
          event_id: EVENT_ID,
          stream: "stdout",
          chunks: first ? [
            { chunk_id: "a", event_id: EVENT_ID, stream: "stdout", chunk_index: 0, text: "chunk-0", byte_count: 7, sha256: "0".repeat(64) },
            { chunk_id: "b", event_id: EVENT_ID, stream: "stdout", chunk_index: 1, text: "</pre><script>chunk-1</script>", byte_count: 30, sha256: "1".repeat(64) },
          ] : [
            { chunk_id: "c", event_id: EVENT_ID, stream: "stdout", chunk_index: 2, text: "chunk-2", byte_count: 7, sha256: "2".repeat(64) },
          ],
          next_after_chunk: first ? 1 : null,
          output_complete: !first,
        },
      });
      return;
    }
    await route.fulfill({ status: 404, json: { error: "Unmocked audit API route" } });
  });
}

async function createControlledExportServer() {
  const adminRoot = fileURLToPath(new URL("../src/admin/", import.meta.url));
  const rootPrefix = adminRoot.endsWith(sep) ? adminRoot : `${adminRoot}${sep}`;
  const exportedUrls: string[] = [];
  const mime: Record<string, string> = {
    ".html": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "127.0.0.1"}`);
    if (url.pathname === "/api/usage-logs/v2/export") {
      exportedUrls.push(url.href);
      response.writeHead(200, {
        "Cache-Control": "no-store",
        "Content-Disposition": 'attachment; filename="nwflash-traces.ndjson"',
        "Content-Type": "application/x-ndjson; charset=utf-8",
      });
      response.end(`${JSON.stringify({ trace_ref: TRACE_REF })}\n`);
      return;
    }
    let relative;
    try {
      const decoded = decodeURIComponent(url.pathname);
      if (decoded === "/" || decoded === "/admin/") relative = "index.html";
      else if (decoded.startsWith("/admin/")) relative = decoded.slice("/admin/".length);
      else {
        response.writeHead(404).end("Not found");
        return;
      }
    } catch {
      response.writeHead(400).end("Bad request");
      return;
    }
    const target = resolve(adminRoot, relative);
    if (target !== resolve(adminRoot, "index.html") && !target.startsWith(rootPrefix)) {
      response.writeHead(404).end("Not found");
      return;
    }
    try {
      response.writeHead(200, {
        "Cache-Control": "no-store",
        "Content-Type": mime[extname(target)] ?? "application/octet-stream",
        "X-Content-Type-Options": "nosniff",
      });
      response.end(await readFile(target));
    } catch {
      response.writeHead(404).end("Not found");
    }
  });
  await new Promise<void>((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolveListen());
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("controlled export server did not bind TCP");
  return {
    exportedUrls,
    origin: `http://127.0.0.1:${address.port}`,
    close: () => new Promise<void>((resolveClose, reject) => {
      server.close((error) => error ? reject(error) : resolveClose());
    }),
  };
}

test("keeps unknown command evidence authoritative and pages stdout independently in the real app", async ({ page }) => {
  const exportedUrls: string[] = [];
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("response", (response) => {
    if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`);
  });
  await installDetailApi(page, exportedUrls);
  await page.goto(
    `/?view=audit&runId=${encodeURIComponent(TRACE_REF)}&eventId=${EVENT_ID}&level=output&stream=stdout`,
  );

  await expect.poll(() => errors).toEqual([]);

  await expect(page.locator('[data-status="unknown"]')).toHaveCount(2);
  await expect(page.getByText("成功", { exact: true })).toHaveCount(0);
  await expect(page.locator('[data-evidence-field="verification"]')).toContainText("未提供");
  await expect(page.getByText("退出码：0", { exact: true })).toBeVisible();
  await expect(page.locator("[data-audit-page] img, [data-audit-page] script")).toHaveCount(0);
  await expect(page.locator('[data-output-stream="stdout"]'))
    .toHaveText("chunk-0</pre><script>chunk-1</script>");
  await expect(page.locator('[data-output-stream="stderr"]')).toHaveText("(empty)");

  await page.locator('[data-load-output="stdout"]').click();
  await expect(page.locator('[data-output-stream="stdout"]'))
    .toHaveText("chunk-0</pre><script>chunk-1</script>chunk-2");
  expect(exportedUrls).toEqual([]);
  expect(errors).toEqual([]);
});

test("contains long command evidence inside local scroll regions at 320 and 360 pixels", async ({ page }) => {
  await installDetailApi(page, []);
  for (const width of [320, 360]) {
    await page.setViewportSize({ width, height: 820 });
    await page.goto(
      `/?view=audit&runId=${encodeURIComponent(TRACE_REF)}&eventId=${EVENT_ID}&level=command`,
    );
    const code = page.locator(".audit-code").filter({ hasText: LONG_PATH });
    await expect(code).toContainText(LONG_PATH);
    const metrics = await code.evaluate((element) => ({
      localOverflow: element.scrollWidth > element.clientWidth,
      insideViewport: element.getBoundingClientRect().right <= document.documentElement.clientWidth,
      bodyOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      fieldMinWidth: getComputedStyle(element.parentElement!).minWidth,
      commandOverflow: element.parentElement!.parentElement!.scrollWidth - element.parentElement!.parentElement!.clientWidth,
      overflowX: getComputedStyle(element).overflowX,
      whiteSpace: getComputedStyle(element).whiteSpace,
    }));
    expect(metrics).toEqual({
      localOverflow: true,
      insideViewport: true,
      bodyOverflow: 0,
      fieldMinWidth: "0px",
      commandOverflow: 0,
      overflowX: "auto",
      whiteSpace: "pre",
    });
  }
});

test("uses native same-origin NDJSON download without route or output identifiers in the export query", async ({ page }) => {
  const server = await createControlledExportServer();
  try {
    await installDetailApi(page, server.exportedUrls);
    await page.goto(
      `${server.origin}/?view=audit&runId=${encodeURIComponent(TRACE_REF)}&eventId=${EVENT_ID}&level=command&status=failed&partition=boot_a`,
    );

    const downloadPromise = page.waitForEvent("download");
    await page.getByRole("button", { name: "导出当前筛选 NDJSON" }).click();
    const download = await downloadPromise;
    expect(download.suggestedFilename()).toBe("nwflash-traces.ndjson");
    await expect.poll(() => server.exportedUrls.length).toBe(1);
    expect(await download.failure()).toBeNull();
    const downloadPath = await download.path();
    expect(downloadPath).not.toBeNull();
    expect(await readFile(downloadPath!, "utf8")).toBe(`${JSON.stringify({ trace_ref: TRACE_REF })}\n`);
    const exported = new URL(download.url());
    expect(Object.fromEntries(exported.searchParams)).toEqual({ status: "failed", partition: "boot_a" });
    expect(exported.href).not.toContain(RUN_ID);
    expect(exported.href).not.toContain(EVENT_ID);
    expect(exported.href).not.toContain("stream");
    expect(server.exportedUrls).toEqual([download.url()]);
  } finally {
    await server.close();
  }
});
