import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createAuditPage } from "../pages/audit.js";
import { createOverviewPage } from "../pages/overview.js";

const TRACE_REF = "v2:019d9c40-7b3c-7000-8000-000000000042";
const doms = [];

function createDom(search = "?view=overview") {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: `https://admin.example.test/${search}`,
  });
  doms.push(dom);
  return dom;
}

function validRun(overrides = {}) {
  return {
    source_schema: 2,
    trace_ref: TRACE_REF,
    run_id: TRACE_REF.slice(3),
    legacy_id: null,
    user_id: 7,
    username: "alice",
    user_name: "Alice",
    operation_kind: "fastboot_flash",
    title: "Flash super",
    outcome: "failed",
    client_version: "1.4.0",
    started_at_ms: 1_787_500_000_000,
    ended_at_ms: 1_787_500_001_000,
    duration_ms: 1_000,
    trace_complete: true,
    trace_loss_reason: null,
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  while (doms.length > 0) doms.pop().window.close();
});

describe("Task 13 authoritative overview", () => {
  it("renders returned 24-hour trend buckets and navigates recent failures by server trace_ref", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_787_548_800_000);
    const dom = createDom();
    const navigate = vi.fn().mockResolvedValue(undefined);
    const getTraceOverview = vi.fn().mockResolvedValue({
      totals: { api_users: 3, online_sessions: 2, operations: 19, failed: 4 },
      trend: [
        { bucket_start_ms: 1_787_500_000_000, operations: 7, failed: 3 },
        { bucket_start_ms: 1_787_503_600_000, operations: 2, failed: 0 },
      ],
      recent_failures: [validRun({ title: '<img src=x onerror="globalThis.pwned=true">' })],
    });
    const page = createOverviewPage({
      document: dom.window.document,
      window: dom.window,
      navigate,
      alert: vi.fn(),
      api: { getTraceOverview },
    });
    dom.window.document.body.append(page.element);

    await page.activate({ view: "overview" }, new dom.window.AbortController().signal);

    expect(getTraceOverview).toHaveBeenCalledTimes(2);
    expect(getTraceOverview.mock.calls[0][0]).toMatchObject({ from: expect.any(Number), to: 1_787_548_800_000, bucket: "hour" });
    expect(getTraceOverview.mock.calls[1][0]).toEqual({ from: 1_787_462_400_000, to: 1_787_548_800_000, bucket: "hour" });

    const buckets = [...page.element.querySelectorAll("[data-trend-bucket]")];
    expect(buckets).toHaveLength(2);
    expect(buckets.map((bucket) => [
      bucket.querySelector('[data-trend-value="operations"]').textContent,
      bucket.querySelector('[data-trend-value="failed"]').textContent,
    ])).toEqual([["7", "3"], ["2", "0"]]);
    expect(page.element.textContent).toContain("2026-08-23T15:46:40.000Z");
    expect(page.element.querySelector("img")).toBeNull();

    const failure = page.element.querySelector('[data-overview-action="open-run"]');
    const focusId = failure.dataset.routerFocusId;
    expect(failure.textContent).toContain(TRACE_REF);
    expect(focusId).toMatch(/^overview-failure-[0-9a-f]{16}$/);
    failure.click();
    expect(navigate).toHaveBeenCalledWith(
      {
        view: "audit",
        level: "run",
        userId: "7",
        runId: TRACE_REF,
        eventId: null,
        stream: null,
        cursor: null,
      },
      { focusId },
    );
  });

  it("shows explicit empty states for both authoritative trend and recent failures", async () => {
    const dom = createDom();
    const page = createOverviewPage({
      document: dom.window.document,
      window: dom.window,
      navigate: vi.fn(),
      api: {
        getTraceOverview: vi.fn().mockResolvedValue({
          totals: { api_users: 0, online_sessions: 0, operations: 0, failed: 0 },
          trend: [],
          recent_failures: [],
        }),
      },
    });

    await page.activate({ view: "overview" }, new dom.window.AbortController().signal);

    expect(page.element.querySelector("[data-trend-empty]").textContent).toContain("最近 24 小时暂无趋势数据");
    expect(page.element.querySelector("[data-failures-empty]").textContent).toContain("当前没有失败记录");
  });
});

describe("Task 13 audit filters", () => {
  it("renders the authoritative last operation in each user summary row", async () => {
    const dom = createDom("?view=audit");
    const page = createAuditPage({
      document: dom.window.document,
      window: dom.window,
      navigate: vi.fn(),
      api: {
        getTraceUsers: vi.fn().mockResolvedValue({
          items: [{
            user_id: 7, username: "alice", name: "Alice", operation_count: 2, failed_count: 1,
            last_operation: validRun({ title: "Last authoritative flash", outcome: "failed" }),
            last_activity_at_ms: 1_787_500_000_000,
          }],
          next_cursor: null,
        }),
        getTraceExportUrl: vi.fn(),
      },
    });
    await page.activate({ view: "audit" }, new dom.window.AbortController().signal);
    const row = page.element.querySelector('[data-audit-action="open-user"]');
    expect(row.textContent).toContain("最近操作：Last authoritative flash");
    expect(row.textContent).toContain("FAILED");
  });

  it("routes run-only filters to the global run endpoint even without a user id", async () => {
    const dom = createDom("?view=audit");
    const navigate = vi.fn().mockResolvedValue(undefined);
    const page = createAuditPage({
      document: dom.window.document,
      window: dom.window,
      navigate,
      api: { getTraceUsers: vi.fn().mockResolvedValue({ items: [], next_cursor: null }), getTraceExportUrl: vi.fn() },
    });
    await page.activate({ view: "audit" }, new dom.window.AbortController().signal);
    const form = page.element.querySelector('[data-audit-filter-form="true"]');
    form.elements.namedItem("kind").value = "fastboot_flash";
    form.elements.namedItem("partition").value = "boot_a";
    form.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));

    expect(navigate).toHaveBeenCalledWith(expect.objectContaining({
      view: "audit", level: "user", userId: null, kind: "fastboot_flash", partition: "boot_a",
    }), { focusId: "audit-filter-submit" });
  });

  it("renders accessible canonical filters and submits to the matching list level without lower state", async () => {
    const dom = createDom("?view=audit");
    const navigate = vi.fn().mockResolvedValue(undefined);
    const page = createAuditPage({
      document: dom.window.document,
      window: dom.window,
      navigate,
      api: {
        getTraceRun: vi.fn().mockResolvedValue({
          source_schema: 2,
          detail_available: true,
          detail_unavailable_reason: null,
          run: validRun(),
          events: [],
        }),
        getTraceExportUrl: vi.fn(),
      },
    });
    dom.window.document.body.append(page.element);

    await page.activate({
      view: "audit",
      level: "run",
      userId: "7",
      runId: TRACE_REF,
      eventId: null,
      stream: null,
      from: "2026-08-01T00:00:00Z",
      to: "2026-08-28T00:00:00Z",
      status: "failed",
      kind: "fastboot_flash",
      partition: "super",
      errorCode: "FLASH_FAILED",
      q: "device-7",
      cursor: null,
    }, new dom.window.AbortController().signal);

    const form = page.element.querySelector('form[aria-label="审计筛选"]');
    expect(form).not.toBeNull();
    expect([...form.querySelectorAll("label")].map((label) => label.textContent)).toEqual([
      "开始时间", "结束时间", "状态", "操作类型", "用户 ID", "分区", "错误码", "关键词",
    ]);
    expect(form.elements.namedItem("from").value).toBe("2026-08-01T00:00:00Z");
    expect(form.elements.namedItem("userId").value).toBe("7");

    form.elements.namedItem("to").value = "2026-08-28T23:59:59Z";
    form.elements.namedItem("q").value = "flash retry";
    form.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));

    expect(navigate).toHaveBeenCalledWith(
      expect.objectContaining({
        view: "audit",
        level: "user",
        userId: "7",
        runId: null,
        eventId: null,
        stream: null,
        cursor: null,
        from: "2026-08-01T00:00:00Z",
        to: "2026-08-28T23:59:59Z",
        status: "failed",
        kind: "fastboot_flash",
        partition: "super",
        errorCode: "FLASH_FAILED",
        q: "flash retry",
      }),
      { focusId: "audit-filter-submit" },
    );
  });

  it("resets all filters, cursor, and hierarchy identifiers to the audit overview", async () => {
    const dom = createDom("?view=audit");
    const navigate = vi.fn().mockResolvedValue(undefined);
    const page = createAuditPage({
      document: dom.window.document,
      window: dom.window,
      navigate,
      api: {
        getTraceRuns: vi.fn().mockResolvedValue({ items: [], next_cursor: "opaque-next" }),
        getTraceExportUrl: vi.fn(),
      },
    });
    dom.window.document.body.append(page.element);

    await page.activate({
      view: "audit",
      level: "user",
      userId: "7",
      runId: null,
      eventId: null,
      stream: null,
      from: "2026-08-01T00:00:00Z",
      to: "2026-08-28T00:00:00Z",
      status: "failed",
      kind: "fastboot_flash",
      partition: "super",
      errorCode: "FLASH_FAILED",
      q: "device-7",
      cursor: "opaque-cursor",
    }, new dom.window.AbortController().signal);

    page.element.querySelector('[data-audit-filter-action="reset"]').click();

    expect(navigate).toHaveBeenCalledWith(
      {
        view: "audit",
        level: "overview",
        userId: null,
        runId: null,
        eventId: null,
        stream: null,
        from: null,
        to: null,
        status: null,
        kind: null,
        partition: null,
        errorCode: null,
        q: null,
        cursor: null,
      },
      { focusId: "audit-filter-reset" },
    );
  });
});
