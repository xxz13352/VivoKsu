import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createAuditPage } from "../pages/audit.js";

const TRACE_REF = "v2:019d9c40-7b3c-7000-8000-000000000002";
const RUN_ID = TRACE_REF.slice(3);
const EVENT_ID = "019d9c40-7b3c-7000-8000-000000000003";
const doms = [];

function run(overrides = {}) {
  return {
    source_schema: 2,
    trace_ref: TRACE_REF,
    run_id: RUN_ID,
    legacy_id: null,
    user_id: 7,
    username: "alice",
    user_name: "Alice Zhang",
    operation_kind: "fastboot_flash",
    title: "VIVO line flash",
    outcome: "unknown",
    client_version: "1.4.0",
    started_at_ms: 1_787_500_000_000,
    ended_at_ms: 1_787_500_002_500,
    duration_ms: 2_500,
    trace_complete: true,
    trace_loss_reason: null,
    ...overrides,
  };
}

function event(overrides = {}) {
  return {
    event_id: EVENT_ID,
    run_id: RUN_ID,
    sequence: 2,
    kind: "command",
    step_name: "Flash boot_a",
    partition_name: "boot_a",
    status: "unknown",
    started_at_ms: 1_787_500_000_100,
    ended_at_ms: 1_787_500_002_400,
    duration_ms: 2_300,
    command: {
      program: '<img src=x onerror="globalThis.pwned=true">fastboot.exe',
      argv: ["flash", "boot_a", "</pre><script>globalThis.pwned=true</script>"],
      display_command: "fastboot flash boot_a C:\\firmware\\boot.img",
      working_directory: "C:/nwflash",
      paths: ["C:\\firmware\\boot.img"],
      urls: ["https://downloads.example/boot.img"],
      serial: "9A7F23BC10D4",
    },
    exit_code: 0,
    stdout_chunks: 3,
    stderr_chunks: 1,
    verification: null,
    device_state: "fastboot",
    retry_safe: false,
    remedies: ["Inspect the persisted verification result."],
    error_class: "remote",
    error_code: "UNVERIFIED",
    error_message: "Server did not persist a success verdict.",
    credential_redactions: [{ kind: "bearer", count: 1 }],
    ...overrides,
  };
}

function createHarness(api, extras = {}) {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "https://admin.example.test/?view=audit",
  });
  dom.window.scrollTo = vi.fn();
  doms.push(dom);
  const navigate = vi.fn().mockResolvedValue(undefined);
  const download = vi.fn();
  const page = createAuditPage({
    document: dom.window.document,
    window: dom.window,
    api,
    navigate,
    download,
    announce: vi.fn(),
    alert: vi.fn(),
    ...extras,
  });
  dom.window.document.body.append(page.element);
  return { dom, navigate, download, page };
}

afterEach(() => {
  while (doms.length > 0) doms.pop().window.close();
});

describe("authoritative event detail", () => {
  it("does not synthesize success from exit code zero and renders hostile evidence as text", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event() }),
      getTraceOutput: vi.fn(),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", userId: "7", runId: TRACE_REF, eventId: EVENT_ID,
    });

    expect(api.getTraceEvent).toHaveBeenCalledWith(TRACE_REF, EVENT_ID, { signal: expect.anything() });
    expect(page.element.querySelector('[data-status="unknown"]')?.textContent).toBe("UNKNOWN");
    expect([...page.element.querySelectorAll('[data-status="success"]')]).toHaveLength(0);
    expect(page.element.querySelector('[data-evidence-field="verification"]')?.textContent).toContain("未提供");
    expect(page.element.textContent).toContain("退出码：0");
    expect(page.element.textContent).toContain('<img src=x onerror="globalThis.pwned=true">fastboot.exe');
    expect(page.element.textContent).toContain("</pre><script>globalThis.pwned=true</script>");
    expect(page.element.querySelector("img")).toBeNull();
    expect(page.element.querySelector("script")).toBeNull();
    expect(api.getTraceOutput).not.toHaveBeenCalled();
  });

  it("rejects event statuses outside the frozen event enum", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event({ status: "running" }) }),
      getTraceExportUrl: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID });

    expect(page.element.textContent).toContain("步骤与运行响应不一致");
    expect(page.element.querySelector('[data-status="running"]')).toBeNull();
  });

  it("binds event evidence to the exact V2 run id and route user", async () => {
    const wrongRunId = "019d9c40-7b3c-7000-8000-000000000099";
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({
        run: run({ run_id: wrongRunId, user_id: 8, title: "Wrong event evidence" }),
        event: event({ run_id: wrongRunId }),
      }),
      getTraceExportUrl: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", userId: "7", runId: TRACE_REF, eventId: EVENT_ID,
    });

    expect(page.element.textContent).toContain("步骤详情与审计链接不一致");
    expect(page.element.textContent).not.toContain("Wrong event evidence");
  });
});

describe("independent command output streams", () => {
  it("keeps stdout and stderr cursors independent and appends strict persisted order", async () => {
    const getTraceOutput = vi.fn(async (_runId, _eventId, query) => {
      if (query.stream === "stdout" && query.afterChunk === -1) {
        return {
          run_id: RUN_ID, event_id: EVENT_ID, stream: "stdout",
          chunks: [
            { chunk_id: "a", event_id: EVENT_ID, stream: "stdout", chunk_index: 0, text: "chunk-0", byte_count: 7, sha256: "0".repeat(64) },
            { chunk_id: "b", event_id: EVENT_ID, stream: "stdout", chunk_index: 1, text: "</pre><script>chunk-1</script>", byte_count: 30, sha256: "1".repeat(64) },
          ],
          next_after_chunk: 1,
          output_complete: false,
        };
      }
      if (query.stream === "stdout" && query.afterChunk === 1) {
        return {
          run_id: RUN_ID, event_id: EVENT_ID, stream: "stdout",
          chunks: [{ chunk_id: "c", event_id: EVENT_ID, stream: "stdout", chunk_index: 2, text: "chunk-2", byte_count: 7, sha256: "2".repeat(64) }],
          next_after_chunk: null,
          output_complete: true,
        };
      }
      return {
        run_id: RUN_ID, event_id: EVENT_ID, stream: "stderr",
        chunks: [{ chunk_id: "d", event_id: EVENT_ID, stream: "stderr", chunk_index: 0, text: "stderr-0", byte_count: 8, sha256: "3".repeat(64) }],
        next_after_chunk: null,
        output_complete: true,
      };
    });
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run({ outcome: "success" }), event: event({ status: "success" }) }),
      getTraceOutput,
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", userId: "7", runId: TRACE_REF, eventId: EVENT_ID, stream: "stdout",
    });

    expect(getTraceOutput.mock.calls.map((call) => call[2])).toEqual([
      { stream: "stdout", afterChunk: -1, limit: 50 },
      { stream: "stderr", afterChunk: -1, limit: 50 },
    ]);
    expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent)
      .toBe("chunk-0</pre><script>chunk-1</script>");
    expect(page.element.querySelector('[data-output-stream="stderr"]')?.textContent).toBe("stderr-0");
    expect(page.element.querySelector('[data-output-stream="stdout"] script')).toBeNull();

    page.element.querySelector('[data-load-output="stdout"]').click();
    await vi.waitFor(() => expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent)
      .toBe("chunk-0</pre><script>chunk-1</script>chunk-2"));
    expect(getTraceOutput.mock.calls.at(-1)[2]).toEqual({ stream: "stdout", afterChunk: 1, limit: 50 });
    expect(getTraceOutput.mock.calls.filter((call) => call[2].stream === "stderr")).toHaveLength(1);
  });

  it("shows (empty) only for an authoritative complete empty stream", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event({ stdout_chunks: 0, stderr_chunks: 0 }) }),
      getTraceOutput: vi.fn(async (_runId, _eventId, query) => ({
        run_id: RUN_ID,
        event_id: EVENT_ID,
        stream: query.stream,
        chunks: [],
        next_after_chunk: null,
        output_complete: query.stream === "stderr",
      })),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID, stream: "stderr",
    });

    expect(page.element.querySelector('[data-output-stream="stderr"]')?.textContent).toBe("(empty)");
    expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent).not.toBe("(empty)");
    expect(page.element.querySelector('[data-output-state="stdout"]')?.textContent).toContain("尚未完整");
  });

  it("fails closed on a gap instead of reordering or displaying incomplete output", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event() }),
      getTraceOutput: vi.fn(async (_runId, _eventId, query) => ({
        run_id: RUN_ID,
        event_id: EVENT_ID,
        stream: query.stream,
        chunks: query.stream === "stdout" ? [
          { chunk_id: "a", event_id: EVENT_ID, stream: "stdout", chunk_index: 0, text: "first", byte_count: 5, sha256: "0".repeat(64) },
          { chunk_id: "c", event_id: EVENT_ID, stream: "stdout", chunk_index: 2, text: "gap", byte_count: 3, sha256: "1".repeat(64) },
        ] : [],
        next_after_chunk: null,
        output_complete: true,
      })),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID, stream: "stdout",
    });

    expect(page.element.querySelector('[data-output-error="stdout"]')?.textContent).toContain("分页响应不连续");
    expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent).toBe("");
  });

  it("does not claim complete output until the exact declared total is present", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event({ stdout_chunks: 3, stderr_chunks: 0 }) }),
      getTraceOutput: vi.fn(async (_runId, _eventId, query) => ({
        run_id: RUN_ID,
        event_id: EVENT_ID,
        stream: query.stream,
        chunks: query.stream === "stdout" ? [
          { chunk_id: "a", event_id: EVENT_ID, stream: "stdout", chunk_index: 0, text: "only-one", byte_count: 8, sha256: "0".repeat(64) },
        ] : [],
        next_after_chunk: null,
        output_complete: true,
      })),
      getTraceExportUrl: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({
      view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID, stream: "stdout",
    });

    expect(page.element.querySelector('[data-output-error="stdout"]')?.textContent).toContain("声明总数");
    expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent).toBe("");
    expect(page.element.querySelector('[data-output-state="stdout"]')?.textContent).not.toBe("输出完整");
  });

  it("retries only the failed stream page and preserves already verified chunks", async () => {
    let stdoutAfterZeroAttempts = 0;
    const getTraceOutput = vi.fn(async (_runId, _eventId, query) => {
      if (query.stream === "stderr") {
        return {
          run_id: RUN_ID, event_id: EVENT_ID, stream: "stderr", chunks: [],
          next_after_chunk: null, output_complete: true,
        };
      }
      if (query.afterChunk === -1) {
        return {
          run_id: RUN_ID, event_id: EVENT_ID, stream: "stdout",
          chunks: [{ chunk_id: "a", event_id: EVENT_ID, stream: "stdout", chunk_index: 0, text: "chunk-0", byte_count: 7, sha256: "0".repeat(64) }],
          next_after_chunk: 0, output_complete: false,
        };
      }
      stdoutAfterZeroAttempts += 1;
      if (stdoutAfterZeroAttempts === 1) throw new Error("temporary output failure");
      return {
        run_id: RUN_ID, event_id: EVENT_ID, stream: "stdout",
        chunks: [{ chunk_id: "b", event_id: EVENT_ID, stream: "stdout", chunk_index: 1, text: "chunk-1", byte_count: 7, sha256: "1".repeat(64) }],
        next_after_chunk: null, output_complete: true,
      };
    });
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event({ stdout_chunks: 2, stderr_chunks: 0 }) }),
      getTraceOutput,
      getTraceExportUrl: vi.fn(),
    };
    const { page } = createHarness(api);
    await page.activate({
      view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID, stream: "stdout",
    });
    const load = page.element.querySelector('[data-load-output="stdout"]');

    load.click();
    await vi.waitFor(() => expect(page.element.querySelector('[data-output-error="stdout"]')?.textContent)
      .toContain("temporary output failure"));
    expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent).toBe("chunk-0");
    expect(load.hidden).toBe(false);
    expect(load.textContent).toContain("重试");

    load.click();
    await vi.waitFor(() => expect(page.element.querySelector('[data-output-stream="stdout"]')?.textContent)
      .toBe("chunk-0chunk-1"));
    expect(getTraceOutput.mock.calls.filter((call) => call[2].stream === "stderr")).toHaveLength(1);
  });
});

describe("audited NDJSON export", () => {
  it("uses one native same-origin download with only current non-output filters", async () => {
    const getTraceExportUrl = vi.fn().mockReturnValue(
      "/api/usage-logs/v2/export?userId=7&kind=fastboot_flash&status=failed&partition=boot_a",
    );
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({
        run: run(),
        event: event({ stdout_chunks: 0, stderr_chunks: 0 }),
      }),
      getTraceOutput: vi.fn(async (_runId, _eventId, query) => ({
        run_id: RUN_ID,
        event_id: EVENT_ID,
        stream: query.stream,
        chunks: [],
        next_after_chunk: null,
        output_complete: true,
      })),
      getTraceExportUrl,
    };
    const { page, dom, download } = createHarness(api);
    let clickedAnchor = null;
    const click = vi.spyOn(dom.window.HTMLAnchorElement.prototype, "click").mockImplementation(function () {
      clickedAnchor = this;
    });
    await page.activate({
      view: "audit",
      level: "command",
      userId: "7",
      runId: TRACE_REF,
      eventId: EVENT_ID,
      stream: "stderr",
      kind: "fastboot_flash",
      status: "failed",
      from: "1787500000000",
      to: "1787500009000",
      partition: "boot_a",
      errorCode: "LOCKED_DEVICE",
      q: "device locked",
    });
    const button = page.element.querySelector('[data-audit-action="export"]');

    button.click();
    await Promise.resolve();
    button.click();
    expect(getTraceExportUrl).toHaveBeenCalledOnce();
    expect(getTraceExportUrl).toHaveBeenCalledWith({
      userId: "7",
      kind: "fastboot_flash",
      status: "failed",
      from: "1787500000000",
      to: "1787500009000",
      partition: "boot_a",
      errorCode: "LOCKED_DEVICE",
      q: "device locked",
    });
    expect(click).toHaveBeenCalledOnce();
    expect(clickedAnchor?.getAttribute("href"))
      .toBe("/api/usage-logs/v2/export?userId=7&kind=fastboot_flash&status=failed&partition=boot_a");
    expect(clickedAnchor?.getAttribute("download")).toBe("nwflash-traces.ndjson");
    expect(dom.window.document.querySelector('a[download="nwflash-traces.ndjson"]')).toBeNull();
    expect(download).not.toHaveBeenCalled();
    expect(button.disabled).toBe(true);
  });

  it("does not retain a temporary download anchor across route activation", async () => {
    const api = {
      getTraceEvent: vi.fn().mockResolvedValue({ run: run(), event: event() }),
      getTraceRuns: vi.fn().mockResolvedValue({ items: [run()], next_cursor: null }),
      getTraceExportUrl: vi.fn().mockReturnValue("/api/usage-logs/v2/export?status=failed"),
    };
    const { page, dom } = createHarness(api);
    vi.spyOn(dom.window.HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    await page.activate({ view: "audit", level: "command", runId: TRACE_REF, eventId: EVENT_ID });
    page.element.querySelector('[data-audit-action="export"]').click();
    expect(dom.window.document.querySelector('a[download="nwflash-traces.ndjson"]')).toBeNull();

    await page.activate({ view: "audit", level: "user", userId: "7" });

    expect(dom.window.document.querySelector('a[download="nwflash-traces.ndjson"]')).toBeNull();
  });
});
