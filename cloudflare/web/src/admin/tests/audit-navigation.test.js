import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createAuditPage } from "../pages/audit.js";

const TRACE_REF = "v2:019d9c40-7b3c-7000-8000-000000000002";
const EVENT_ID = "019d9c40-7b3c-7000-8000-000000000003";
const doms = [];

function createDom() {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "https://admin.example.test/?view=audit",
  });
  dom.window.scrollTo = vi.fn();
  doms.push(dom);
  return dom;
}

function run(overrides = {}) {
  return {
    source_schema: 2,
    trace_ref: TRACE_REF,
    run_id: TRACE_REF.slice(3),
    legacy_id: null,
    user_id: 7,
    username: "alice",
    user_name: "Alice Zhang",
    operation_kind: "fastboot_flash",
    title: "VIVO line flash",
    outcome: "success",
    client_version: "1.4.0",
    started_at_ms: 1_787_500_000_000,
    ended_at_ms: 1_787_500_002_500,
    duration_ms: 2_500,
    trace_complete: true,
    trace_loss_reason: null,
    ...overrides,
  };
}

function event(sequence, overrides = {}) {
  return {
    event_id: sequence === 1 ? EVENT_ID : `019d9c40-7b3c-7000-8000-${String(sequence).padStart(12, "0")}`,
    run_id: TRACE_REF.slice(3),
    sequence,
    kind: "stage",
    step_name: `Persisted step ${sequence}`,
    partition_name: null,
    status: "success",
    started_at_ms: 1_787_500_000_000 + sequence,
    ended_at_ms: 1_787_500_000_100 + sequence,
    duration_ms: 100,
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
    ...overrides,
  };
}

function createHarness(api) {
  const dom = createDom();
  const navigate = vi.fn().mockResolvedValue(undefined);
  const page = createAuditPage({
    document: dom.window.document,
    window: dom.window,
    api,
    navigate,
    announce: vi.fn(),
    alert: vi.fn(),
  });
  dom.window.document.body.append(page.element);
  return { dom, navigate, page };
}

afterEach(() => {
  while (doms.length > 0) doms.pop().window.close();
});

describe("audit hierarchy and route state", () => {
  it("loads user summaries with an AbortSignal and renders hostile names as text", async () => {
    const getTraceUsers = vi.fn().mockResolvedValue({
      items: [{
        user_id: 7,
        username: "alice",
        name: '<img src=x onerror="globalThis.pwned=true">',
        operation_count: 12,
        failed_count: 1,
        last_operation: run(),
        last_activity_at_ms: 1_787_500_002_500,
      }],
      next_cursor: "next-user-page",
    });
    const { page, dom, navigate } = createHarness({ getTraceUsers, exportTrace: vi.fn() });

    await page.activate({ view: "audit", status: "failed", q: "locked", cursor: null });

    expect(getTraceUsers).toHaveBeenCalledWith(
      { status: "failed", q: "locked", limit: 50 },
      { signal: expect.any(dom.window.AbortSignal) },
    );
    expect(page.element.textContent).toContain('<img src=x onerror="globalThis.pwned=true">');
    expect(page.element.querySelector("img")).toBeNull();
    const user = page.element.querySelector('[data-audit-action="open-user"]');
    expect(user.dataset.routerFocusId).toBe("audit-user-7");

    user.click();
    expect(navigate).toHaveBeenCalledWith(
      expect.objectContaining({ view: "audit", level: "user", userId: "7", cursor: null }),
      { focusId: "audit-user-7" },
    );
  });

  it("drills through the opaque trace_ref and stops explicitly for V1", async () => {
    const legacy = run({
      source_schema: 1,
      trace_ref: "v1:42",
      run_id: null,
      legacy_id: 42,
      title: "Legacy flash",
      outcome: "unknown",
      trace_complete: false,
      trace_loss_reason: "legacy_client_no_step_data",
    });
    const api = {
      getTraceRuns: vi.fn().mockResolvedValue({ items: [legacy], next_cursor: null }),
      getTraceRun: vi.fn().mockResolvedValue({
        source_schema: 1,
        detail_available: false,
        detail_unavailable_reason: "legacy_client_no_step_data",
        run: legacy,
        events: [],
      }),
      getTraceEvent: vi.fn(),
      getTraceOutput: vi.fn(),
      exportTrace: vi.fn(),
    };
    const { page, navigate } = createHarness(api);

    await page.activate({ view: "audit", level: "user", userId: "7" });
    const runButton = page.element.querySelector('[data-audit-action="open-run"]');
    const runFocusId = runButton.dataset.routerFocusId;
    runButton.click();
    expect(navigate).toHaveBeenCalledWith(
      expect.objectContaining({ runId: "v1:42", level: "run", eventId: null, cursor: null }),
      { focusId: runFocusId },
    );

    await page.activate({ view: "audit", level: "run", userId: "7", runId: "v1:42" });
    expect(api.getTraceRun).toHaveBeenLastCalledWith("v1:42", { signal: expect.anything() });
    expect(api.getTraceRun.mock.calls.at(-1)[1].signal.aborted).toBe(false);
    expect(page.element.textContent).toContain("旧客户端未上传步骤数据");
    expect(page.element.querySelector("[data-event-sequence]")).toBeNull();
    expect(api.getTraceEvent).not.toHaveBeenCalled();
    expect(api.getTraceOutput).not.toHaveBeenCalled();
  });

  it("renders persisted V2 events in server order and exposes partial trace state", async () => {
    const partial = run({ outcome: "failed", trace_complete: false, trace_loss_reason: "client_shutdown" });
    const api = {
      getTraceRun: vi.fn().mockResolvedValue({
        source_schema: 2,
        detail_available: true,
        detail_unavailable_reason: null,
        run: partial,
        events: [event(1), event(2, { status: "failed" }), event(3, { status: "skipped" })],
      }),
      exportTrace: vi.fn(),
    };
    const { page, navigate } = createHarness(api);

    await page.activate({ view: "audit", level: "run", userId: "7", runId: TRACE_REF });

    expect([...page.element.querySelectorAll("[data-event-sequence]")].map((node) => node.textContent))
      .toEqual(["1", "2", "3"]);
    expect(page.element.textContent).toContain("追踪不完整");
    expect(page.element.textContent).toContain("client_shutdown");
    const details = page.element.querySelectorAll('[data-audit-action="open-event"]');
    const eventFocusId = details[1].dataset.routerFocusId;
    details[1].click();
    expect(navigate).toHaveBeenCalledWith(
      expect.objectContaining({ runId: TRACE_REF, eventId: event(2).event_id, level: "command", stream: null }),
      { focusId: eventFocusId },
    );
  });

  it("fails closed when a run detail response is not strictly ordered by sequence", async () => {
    const api = {
      getTraceRun: vi.fn().mockResolvedValue({
        source_schema: 2,
        detail_available: true,
        detail_unavailable_reason: null,
        run: run(),
        events: [event(2), event(1)],
      }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", level: "run", runId: TRACE_REF });

    expect(page.element.textContent).toContain("步骤响应顺序无效");
    expect(page.element.querySelector("[data-event-sequence]")).toBeNull();
  });

  it("binds a run detail response to the exact requested trace_ref", async () => {
    const other = run({
      trace_ref: "v2:019d9c40-7b3c-7000-8000-000000000099",
      run_id: "019d9c40-7b3c-7000-8000-000000000099",
      title: "Wrong run evidence",
    });
    const api = {
      getTraceRun: vi.fn().mockResolvedValue({
        source_schema: 2,
        detail_available: true,
        detail_unavailable_reason: null,
        run: other,
        events: [],
      }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", level: "run", runId: TRACE_REF });

    expect(page.element.textContent).toContain("运行详情与审计链接不一致");
    expect(page.element.textContent).not.toContain("Wrong run evidence");
  });

  it.each([
    ["run level with event", { level: "run", runId: TRACE_REF, eventId: EVENT_ID }],
    ["user level with run", { level: "user", userId: "7", runId: TRACE_REF }],
    ["overview level with user", { level: "overview", userId: "7" }],
    ["run level with list cursor", { level: "run", runId: TRACE_REF, cursor: "list-cursor" }],
  ])("rejects the contradictory deep link: %s", async (_name, route) => {
    const api = {
      getTraceUsers: vi.fn(),
      getTraceRuns: vi.fn(),
      getTraceRun: vi.fn(),
      getTraceEvent: vi.fn(),
      getTraceOutput: vi.fn(),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", ...route });

    expect(page.element.textContent).toContain("审计链接参数不完整");
    expect(api.getTraceUsers).not.toHaveBeenCalled();
    expect(api.getTraceRuns).not.toHaveBeenCalled();
    expect(api.getTraceRun).not.toHaveBeenCalled();
    expect(api.getTraceEvent).not.toHaveBeenCalled();
    expect(api.getTraceOutput).not.toHaveBeenCalled();
  });

  it("keeps user and run focus ids bound to entities when rows reorder", async () => {
    const user7 = {
      user_id: 7, username: "alice", name: "Alice", operation_count: 1, failed_count: 0,
      last_operation: null, last_activity_at_ms: 1_787_500_000_000,
    };
    const user8 = { ...user7, user_id: 8, username: "bob", name: "Bob" };
    const runA = run({ title: "Run A" });
    const runB = run({
      trace_ref: "v2:019d9c40-7b3c-7000-8000-000000000099",
      run_id: "019d9c40-7b3c-7000-8000-000000000099",
      title: "Run B",
    });
    const api = {
      getTraceUsers: vi.fn()
        .mockResolvedValueOnce({ items: [user7, user8], next_cursor: null })
        .mockResolvedValueOnce({ items: [user8, user7], next_cursor: null }),
      getTraceRuns: vi.fn()
        .mockResolvedValueOnce({ items: [runA, runB], next_cursor: null })
        .mockResolvedValueOnce({ items: [runB, runA], next_cursor: null }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit" });
    const firstUsers = Object.fromEntries([...page.element.querySelectorAll('[data-audit-action="open-user"]')]
      .map((button) => [button.textContent.includes("Alice") ? "Alice" : "Bob", button.dataset.routerFocusId]));
    await page.activate({ view: "audit" });
    const reorderedUsers = Object.fromEntries([...page.element.querySelectorAll('[data-audit-action="open-user"]')]
      .map((button) => [button.textContent.includes("Alice") ? "Alice" : "Bob", button.dataset.routerFocusId]));
    expect(reorderedUsers).toEqual(firstUsers);

    await page.activate({ view: "audit", level: "user", userId: "7" });
    const firstRuns = Object.fromEntries([...page.element.querySelectorAll('[data-audit-action="open-run"]')]
      .map((button) => [button.textContent.includes("Run A") ? "Run A" : "Run B", button.dataset.routerFocusId]));
    await page.activate({ view: "audit", level: "user", userId: "7" });
    const reorderedRuns = Object.fromEntries([...page.element.querySelectorAll('[data-audit-action="open-run"]')]
      .map((button) => [button.textContent.includes("Run A") ? "Run A" : "Run B", button.dataset.routerFocusId]));
    expect(reorderedRuns).toEqual(firstRuns);
    expect(new Set(Object.values(firstRuns)).size).toBe(2);
  });

  it("rejects run outcomes outside the frozen run enum", async () => {
    const api = {
      getTraceRuns: vi.fn().mockResolvedValue({
        items: [run({ outcome: "started" })],
        next_cursor: null,
      }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", level: "user", userId: "7" });

    expect(page.element.textContent).toContain("运行摘要响应格式无效");
    expect(page.element.querySelector('[data-audit-action="open-run"]')).toBeNull();
  });

  it("does not turn an opaque direct cursor URL into an unsafe history Back action", async () => {
    const api = {
      getTraceUsers: vi.fn().mockResolvedValue({ items: [], next_cursor: null }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", cursor: "direct-cursor" });

    const previous = page.element.querySelector('[aria-label="审计分页"] button');
    expect(previous.disabled).toBe(true);
  });

  it("rejects inconsistent deep links without issuing a detail request", async () => {
    const api = {
      getTraceUsers: vi.fn(),
      getTraceRun: vi.fn(),
      getTraceEvent: vi.fn(),
      getTraceOutput: vi.fn(),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    await page.activate({ view: "audit", level: "command", eventId: EVENT_ID, stream: "stderr" });

    expect(page.element.textContent).toContain("审计链接参数不完整");
    expect(api.getTraceUsers).not.toHaveBeenCalled();
    expect(api.getTraceRun).not.toHaveBeenCalled();
    expect(api.getTraceEvent).not.toHaveBeenCalled();
    expect(api.getTraceOutput).not.toHaveBeenCalled();
  });

  it("aborts the previous activation and ignores its late response", async () => {
    let resolveFirst;
    const first = new Promise((resolve) => { resolveFirst = resolve; });
    const api = {
      getTraceUsers: vi.fn().mockReturnValue(first),
      getTraceRuns: vi.fn().mockResolvedValue({ items: [run({ title: "Current run" })], next_cursor: null }),
      exportTrace: vi.fn(),
    };
    const { page } = createHarness(api);

    const staleActivation = page.activate({ view: "audit" });
    const firstSignal = api.getTraceUsers.mock.calls[0][1].signal;
    await page.activate({ view: "audit", level: "user", userId: "7" });
    expect(firstSignal.aborted).toBe(true);
    resolveFirst({
      items: [{ user_id: 9, username: "stale", name: "STALE RESPONSE", operation_count: 1, failed_count: 0 }],
      next_cursor: null,
    });
    await staleActivation;

    expect(page.element.textContent).toContain("Current run");
    expect(page.element.textContent).not.toContain("STALE RESPONSE");
    page.deactivate();
    expect(api.getTraceRuns.mock.calls[0][1].signal.aborted).toBe(true);
  });
});
