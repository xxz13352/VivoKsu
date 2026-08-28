import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createOverviewPage } from "../pages/overview.js";
import { createVersionsPage } from "../pages/versions.js";
import { createUsersPage } from "../pages/users.js";
import { createSessionsPage } from "../pages/sessions.js";
import { createRomPage } from "../pages/rom.js";

const doms = [];

function createDom() {
  const dom = new JSDOM("<!doctype html><html><body><main id=workspace></main></body></html>", {
    url: "https://admin.example.test/?view=overview",
  });
  doms.push(dom);
  return dom;
}

function context({ api = {}, document, confirm = null } = {}) {
  return {
    document,
    window: document.defaultView,
    api: {
      getTraceOverview: vi.fn().mockResolvedValue({ totals: { api_users: 1, online_sessions: 1, operations: 1, failed: 0 }, trend: [], recent_failures: [] }),
      getAppVersions: vi.fn().mockResolvedValue({ versions: [] }),
      getVersionSummary: vi.fn().mockResolvedValue({ current_version: null, minimum_version: null, supported_versions: [], today_426: 0, as_of_ms: 0 }),
      deleteAppVersion: vi.fn().mockResolvedValue({ ok: true }),
      getUsers: vi.fn().mockResolvedValue({ users: [] }),
      updateUser: vi.fn().mockResolvedValue({ ok: true }),
      deleteUser: vi.fn().mockResolvedValue({ ok: true }),
      rotateUserToken: vi.fn().mockResolvedValue({ ok: true, token: "one-time-token" }),
      getOnlineSessions: vi.fn().mockResolvedValue({ sessions: [] }),
      kickSession: vi.fn().mockResolvedValue({ ok: true, affected: 1 }),
      getRomLogs: vi.fn().mockResolvedValue({ items: [], next_cursor: null }),
      ...api,
    },
    confirm: confirm ?? (async ({ onConfirm }) => onConfirm()),
    navigate: vi.fn(),
    announce: vi.fn(),
    alert: vi.fn(),
  };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

afterEach(() => {
  vi.useRealTimers();
  while (doms.length > 0) doms.pop().window.close();
});

describe.each([
  ["overview", createOverviewPage, "getTraceOverview"],
  ["versions", createVersionsPage, "getAppVersions"],
  ["users", createUsersPage, "getUsers"],
  ["sessions", createSessionsPage, "getOnlineSessions"],
  ["rom", createRomPage, "getRomLogs"],
])("%s page lifecycle", (_name, createPage, failingMethod) => {
  it("renders loading, failure, and retry without inserting API text as markup", async () => {
    const { window } = createDom();
    const api = { [failingMethod]: vi.fn().mockRejectedValue(new Error('<img src=x onerror=alert(1)>')) };
    const ctx = context({ document: window.document, api });
    const page = createPage(ctx);

    const activation = page.activate({ view: _name }, new AbortController().signal);
    expect(page.element.getAttribute("data-page-state")).toBe("loading");
    await activation;
    expect(page.element.getAttribute("data-page-state")).toBe("retry");
    expect(page.element.querySelector("img")).toBeNull();
    expect(page.element.querySelector('[role="alert"]')).not.toBeNull();

    api[failingMethod].mockResolvedValueOnce(successFor(_name));
    page.element.querySelector("button").click();
    await vi.waitFor(() => expect(page.element.getAttribute("data-page-state")).toBe("ready"));
    page.destroy();
  });
});

describe("operational workspace contracts", () => {
  it.each([
    ["overview", createOverviewPage, "getTraceOverview", { totals: { api_users: 1, online_sessions: 1, operations: 1, failed: 0 }, trend: [], recent_failures: [] }],
    ["versions", createVersionsPage, "getAppVersions", { versions: [] }],
    ["users", createUsersPage, "getUsers", { users: [] }],
    ["sessions", createSessionsPage, "getOnlineSessions", { sessions: [] }],
    ["rom", createRomPage, "getRomLogs", { items: [], next_cursor: null }],
  ])("does not render a late %s response after destroy", async (_name, createPage, method, result) => {
    const { window } = createDom();
    const request = deferred();
    const ctx = context({ document: window.document, api: { [method]: vi.fn().mockReturnValue(request.promise) } });
    const page = createPage(ctx);
    const activation = page.activate({ view: _name }, new AbortController().signal);

    page.destroy();
    request.resolve(result);
    await activation;

    expect(page.element.getAttribute("data-page-state")).not.toBe("ready");
    expect(page.element.textContent).not.toContain("当前没有");
  });

  it.each([
    ["overview", createOverviewPage, "getTraceOverview", { totals: { api_users: 1, online_sessions: 1, operations: 1, failed: 0 }, trend: [], recent_failures: [] }],
    ["versions", createVersionsPage, "getAppVersions", { versions: [] }],
    ["users", createUsersPage, "getUsers", { users: [] }],
    ["sessions", createSessionsPage, "getOnlineSessions", { sessions: [] }],
    ["rom", createRomPage, "getRomLogs", { items: [], next_cursor: null }],
  ])("aborts the page-owned %s request during teardown", async (_name, createPage, method, result) => {
    const { window } = createDom();
    const request = deferred();
    const apiMethod = vi.fn().mockReturnValue(request.promise);
    const ctx = context({ document: window.document, api: { [method]: apiMethod } });
    const page = createPage(ctx);
    const external = new AbortController();
    const activation = page.activate({ view: _name }, external.signal);

    await vi.waitFor(() => expect(apiMethod).toHaveBeenCalledOnce());
    const options = apiMethod.mock.calls[0].at(-1);
    expect(options.signal).not.toBe(external.signal);
    page.destroy();
    expect(options.signal.aborted).toBe(true);
    request.resolve(result);
    await activation;
  });

  it("uses only the authoritative overview endpoint and renders returned totals", async () => {
    const { window } = createDom();
    const ctx = context({
      document: window.document,
      api: {
        getTraceOverview: vi.fn().mockResolvedValue({
          totals: { api_users: 7, online_sessions: 3, operations: 44, failed: 2 },
          trend: [],
          recent_failures: [],
        }),
      },
    });
    const page = createOverviewPage(ctx);
    await page.activate({ view: "overview" }, new AbortController().signal);

    expect(ctx.api.getTraceOverview).toHaveBeenCalledOnce();
    expect(page.element.textContent).toContain("44");
    expect(page.element.textContent).toContain("失败");
  });

  it("confirms version deletion and reloads authoritative server state", async () => {
    const { window } = createDom();
    const confirmed = vi.fn(async ({ onConfirm }) => onConfirm());
    const versions = vi.fn()
      .mockResolvedValueOnce({ versions: [{ id: 9, version: "2.0.0", min_version: "1.0.0", enabled: 1 }] })
      .mockResolvedValueOnce({ versions: [] });
    const ctx = context({ document: window.document, confirm: confirmed, api: { getAppVersions: versions } });
    const page = createVersionsPage(ctx);
    await page.activate({ view: "versions" }, new AbortController().signal);

    page.element.querySelector('[data-action="delete-version"]').click();
    await vi.waitFor(() => expect(ctx.api.deleteAppVersion).toHaveBeenCalledWith(9, expect.anything()));
    expect(confirmed).toHaveBeenCalledOnce();
    expect(versions).toHaveBeenCalledTimes(2);
  });

  it("keeps a version deletion single-flight and restores its control after cancellation or failure", async () => {
    const { window } = createDom();
    const confirmations = [];
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: {
        getAppVersions: vi.fn().mockResolvedValue({ versions: [{ id: 9, version: "2.0.0", min_version: "1.0.0", enabled: 1 }] }),
        deleteAppVersion: vi.fn().mockRejectedValueOnce(new Error("delete failed")).mockResolvedValueOnce({ ok: true }),
      },
    });
    const page = createVersionsPage(ctx);
    await page.activate({ view: "versions" }, new AbortController().signal);
    const remove = page.element.querySelector('[data-action="delete-version"]');

    remove.click();
    remove.click();
    expect(confirmations).toHaveLength(1);
    confirmations[0].onCancel();
    expect(remove.disabled).toBe(false);

    remove.click();
    await expect(confirmations[1].onConfirm()).rejects.toThrow("delete failed");
    expect(remove.disabled).toBe(false);
  });

  it("clears a one-time rotated token during page teardown", async () => {
    const { window } = createDom();
    const ctx = context({
      document: window.document,
      api: { getUsers: vi.fn().mockResolvedValue({ users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }] }) },
    });
    const page = createUsersPage(ctx);
    await page.activate({ view: "users" }, new AbortController().signal);

    page.element.querySelector('[data-action="rotate-token"]').click();
    await vi.waitFor(() => expect(page.element.textContent).toContain("one-time-token"));
    page.deactivate();
    expect(page.element.textContent).not.toContain("one-time-token");
  });

  it("reloads authoritative users after token rotation, scopes the token to its epoch, and supports deletion", async () => {
    const { window } = createDom();
    const confirmations = [];
    const users = vi.fn().mockResolvedValue({ users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }] });
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: { getUsers: users },
    });
    const page = createUsersPage(ctx);
    await page.activate({ view: "users" }, new AbortController().signal);

    page.element.querySelector('[data-action="rotate-token"]').click();
    await confirmations[0].onConfirm();
    expect(ctx.api.rotateUserToken).toHaveBeenCalledOnce();
    expect(users).toHaveBeenCalledTimes(2);
    expect(page.element.textContent).toContain("one-time-token");

    page.element.querySelector('[data-action="delete-user"]').click();
    await confirmations[1].onConfirm();
    expect(ctx.api.deleteUser).toHaveBeenCalledWith(7, expect.anything());
    page.deactivate();
    expect(page.element.textContent).not.toContain("one-time-token");
  });

  it("never revives a one-time token when its rotation completes after deactivation", async () => {
    const { window } = createDom();
    const confirmations = [];
    const rotation = deferred();
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: {
        getUsers: vi.fn().mockResolvedValue({ users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }] }),
        rotateUserToken: vi.fn().mockReturnValue(rotation.promise),
      },
    });
    const page = createUsersPage(ctx);
    await page.activate({ view: "users" }, new AbortController().signal);
    page.element.querySelector('[data-action="rotate-token"]').click();
    const completion = confirmations[0].onConfirm();

    page.deactivate();
    rotation.resolve({ ok: true, token: "one-time-token" });
    await completion;
    await page.activate({ view: "users" }, new AbortController().signal);

    expect(page.element.textContent).not.toContain("one-time-token");
  });

  it("serializes rotate, ban, and delete behind one per-user destructive-action lock", async () => {
    const { window } = createDom();
    const confirmations = [];
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: { getUsers: vi.fn().mockResolvedValue({ users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }] }) },
    });
    const page = createUsersPage(ctx);
    await page.activate({ view: "users" }, new AbortController().signal);
    const rotate = page.element.querySelector('[data-action="rotate-token"]');
    const ban = page.element.querySelector('[data-action="toggle-ban"]');
    const remove = page.element.querySelector('[data-action="delete-user"]');

    rotate.click();
    expect(confirmations).toHaveLength(1);
    expect(ban.disabled).toBe(true);
    expect(remove.disabled).toBe(true);
    remove.click();
    expect(confirmations).toHaveLength(1);
    confirmations[0].onCancel();
    expect(rotate.disabled).toBe(false);
    expect(ban.disabled).toBe(false);
    expect(remove.disabled).toBe(false);
  });

  it.each([
    ["ban", '[data-action="toggle-ban"]', "updateUser", { users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 1 }] }],
    ["delete", '[data-action="delete-user"]', "deleteUser", { users: [] }],
  ])("clears a rotated token after the same user is %s and never paints it over the reloaded list", async (_name, selector, method, finalUsers) => {
    const { window } = createDom();
    const confirmations = [];
    const initialUsers = { users: [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0 }] };
    const users = vi.fn()
      .mockResolvedValueOnce(initialUsers)
      .mockResolvedValueOnce(initialUsers)
      .mockResolvedValueOnce(finalUsers);
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: { getUsers: users },
    });
    const page = createUsersPage(ctx);
    await page.activate({ view: "users" }, new AbortController().signal);
    page.element.querySelector('[data-action="rotate-token"]').click();
    await confirmations[0].onConfirm();
    expect(page.element.textContent).toContain("one-time-token");

    page.element.querySelector(selector).click();
    await confirmations[1].onConfirm();

    expect(ctx.api[method]).toHaveBeenCalledOnce();
    expect(page.element.textContent).not.toContain("one-time-token");
  });

  it("polls sessions only while active and visible, and keeps a kick pending until reload", async () => {
    vi.useFakeTimers();
    const { window } = createDom();
    Object.defineProperty(window.document, "visibilityState", { configurable: true, value: "visible" });
    const sessions = vi.fn()
      .mockResolvedValueOnce({ sessions: [{ session_id: "s-1", user_id: 7, username: "alice", name: "Alice" }] })
      .mockResolvedValue({ sessions: [] });
    const ctx = context({ document: window.document, api: { getOnlineSessions: sessions } });
    const page = createSessionsPage(ctx);
    await page.activate({ view: "sessions" }, new AbortController().signal);

    page.element.querySelector('[data-action="kick-session"]').click();
    await vi.waitFor(() => expect(ctx.api.kickSession).toHaveBeenCalledOnce());
    expect(page.element.textContent).toContain("正在等待服务器确认");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(sessions).toHaveBeenCalledTimes(3);
    page.deactivate();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(sessions).toHaveBeenCalledTimes(3);
  });

  it("does not start a polling timer when an initial session load resolves after destroy", async () => {
    vi.useFakeTimers();
    const { window } = createDom();
    const request = deferred();
    const setInterval = vi.spyOn(globalThis, "setInterval");
    const ctx = context({ document: window.document, api: { getOnlineSessions: vi.fn().mockReturnValue(request.promise) } });
    const page = createSessionsPage(ctx);
    const activation = page.activate({ view: "sessions" }, new AbortController().signal);

    page.destroy();
    request.resolve({ sessions: [] });
    await activation;
    await vi.advanceTimersByTimeAsync(10_000);

    expect(setInterval).not.toHaveBeenCalled();
  });

  it("restarts session polling when visibility returns while a poll request is still pending", async () => {
    vi.useFakeTimers();
    const { window } = createDom();
    Object.defineProperty(window.document, "visibilityState", { configurable: true, value: "visible", writable: true });
    const secondRead = deferred();
    const sessions = vi.fn()
      .mockResolvedValueOnce({ sessions: [] })
      .mockReturnValueOnce(secondRead.promise);
    const setInterval = vi.spyOn(globalThis, "setInterval");
    const ctx = context({ document: window.document, api: { getOnlineSessions: sessions } });
    const page = createSessionsPage(ctx);
    await page.activate({ view: "sessions" }, new AbortController().signal);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(sessions).toHaveBeenCalledTimes(2);

    window.document.visibilityState = "hidden";
    window.document.dispatchEvent(new window.Event("visibilitychange"));
    window.document.visibilityState = "visible";
    window.document.dispatchEvent(new window.Event("visibilitychange"));
    secondRead.resolve({ sessions: [] });
    await vi.waitFor(() => expect(setInterval).toHaveBeenCalledTimes(2));
  });

  it("keeps the full authoritative session list during a successful kick and restores retry after failure", async () => {
    const { window } = createDom();
    const confirmations = [];
    const allSessions = [
      { session_id: "s-1", user_id: 7, username: "alice", name: "Alice" },
      { session_id: "s-2", user_id: 8, username: "bob", name: "Bob" },
    ];
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: {
        getOnlineSessions: vi.fn().mockResolvedValue({ sessions: allSessions }),
        kickSession: vi.fn().mockRejectedValueOnce(new Error("kick failed")).mockResolvedValueOnce({ ok: true }),
      },
    });
    const page = createSessionsPage(ctx);
    await page.activate({ view: "sessions" }, new AbortController().signal);
    const kick = page.element.querySelector('[data-action="kick-session"]');
    kick.click();
    await expect(confirmations[0].onConfirm()).rejects.toThrow("kick failed");
    expect(kick.disabled).toBe(false);

    kick.click();
    await confirmations[1].onConfirm();
    expect(page.element.textContent).toContain("alice");
    expect(page.element.textContent).toContain("bob");
    expect(page.element.textContent).toContain("正在等待服务器确认");
  });

  it("preserves the last authoritative session list and offers retry when post-kick reload fails", async () => {
    const { window } = createDom();
    const confirmations = [];
    const allSessions = [
      { session_id: "s-1", user_id: 7, username: "alice", name: "Alice" },
      { session_id: "s-2", user_id: 8, username: "bob", name: "Bob" },
    ];
    const sessions = vi.fn()
      .mockResolvedValueOnce({ sessions: allSessions })
      .mockRejectedValueOnce(new Error("refresh failed"))
      .mockResolvedValueOnce({ sessions: allSessions });
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: { getOnlineSessions: sessions },
    });
    const page = createSessionsPage(ctx);
    await page.activate({ view: "sessions" }, new AbortController().signal);
    page.element.querySelector('[data-action="kick-session"]').click();
    await confirmations[0].onConfirm();

    expect(page.element.textContent).toContain("alice");
    expect(page.element.textContent).toContain("bob");
    expect(page.element.textContent).toContain("refresh failed");
    page.element.querySelector('[data-action="retry-sessions"]').click();
    await vi.waitFor(() => expect(sessions).toHaveBeenCalledTimes(3));
    expect(page.element.textContent).not.toContain("refresh failed");
  });

  it("tracks concurrent kicks per session and clears only IDs absent from authoritative reloads", async () => {
    vi.useFakeTimers();
    const { window } = createDom();
    Object.defineProperty(window.document, "visibilityState", { configurable: true, value: "visible" });
    const confirmations = [];
    let authoritativeSessions = [
      { session_id: "s-1", user_id: 7, username: "alice", name: "Alice" },
      { session_id: "s-2", user_id: 8, username: "bob", name: "Bob" },
    ];
    const sessions = vi.fn().mockImplementation(async () => ({ sessions: authoritativeSessions }));
    const ctx = context({
      document: window.document,
      confirm: vi.fn((options) => confirmations.push(options)),
      api: { getOnlineSessions: sessions },
    });
    const page = createSessionsPage(ctx);
    await page.activate({ view: "sessions" }, new AbortController().signal);
    const kickButtons = [...page.element.querySelectorAll('[data-action="kick-session"]')];
    kickButtons[0].click();
    kickButtons[1].click();
    expect(confirmations).toHaveLength(2);
    await Promise.all(confirmations.map((confirmation) => confirmation.onConfirm()));

    let pendingButtons = [...page.element.querySelectorAll('[data-action="kick-session"]')];
    expect(pendingButtons.every((button) => button.disabled && button.textContent === "等待确认")).toBe(true);
    pendingButtons[0].click();
    expect(confirmations).toHaveLength(2);

    authoritativeSessions = [authoritativeSessions[1]];
    await vi.advanceTimersByTimeAsync(10_000);
    pendingButtons = [...page.element.querySelectorAll('[data-action="kick-session"]')];
    expect(pendingButtons).toHaveLength(1);
    expect(pendingButtons[0].disabled).toBe(true);
    expect(pendingButtons[0].textContent).toBe("等待确认");

    authoritativeSessions = [];
    await vi.advanceTimersByTimeAsync(10_000);
    expect(page.element.querySelectorAll('[data-action="kick-session"]')).toHaveLength(0);
    expect(page.element.textContent).not.toContain("正在等待服务器确认");
  });

  it("renders ROM URLs only as safe links and uses the server cursor unchanged", async () => {
    const { window } = createDom();
    const ctx = context({
      document: window.document,
      api: {
        getRomLogs: vi.fn().mockResolvedValue({
          items: [
            { id: 1, user_name: "Alice", pd: "PD1", version: "1.0", status: 200, url: "https://example.test/rom.zip", failure_reason: null, detail_unavailable_reason: null },
            { id: 2, user_name: "Mallory", pd: "PD2", version: "1.0", status: 500, url: "javascript:alert(1)", failure_reason: null, detail_unavailable_reason: "legacy_record_no_failure_reason" },
            { id: 3, user_name: "Eve", pd: "PD3", version: "1.0", status: 200, url: "https://eve:secret@example.test/rom.zip", failure_reason: null, detail_unavailable_reason: null },
          ],
          next_cursor: "opaque-next-cursor",
        }),
      },
    });
    const page = createRomPage(ctx);
    await page.activate({ view: "rom" }, new AbortController().signal);

    const links = [...page.element.querySelectorAll("a")];
    expect(links).toHaveLength(1);
    expect(links[0].href).toBe("https://example.test/rom.zip");
    expect(page.element.textContent).toContain("https://example.test/rom.zip");
    expect(page.element.querySelector('[data-action="next-page"]')).not.toBeNull();
    page.element.querySelector('[data-action="next-page"]').click();
    expect(ctx.navigate).toHaveBeenCalledWith(expect.objectContaining({ cursor: "opaque-next-cursor" }));
  });
});

function successFor(view) {
  if (view === "overview") return { totals: { api_users: 0, online_sessions: 0, operations: 0, failed: 0 }, trend: [], recent_failures: [] };
  if (view === "versions") return { versions: [] };
  if (view === "users") return { users: [] };
  if (view === "sessions") return { sessions: [] };
  return { items: [], next_cursor: null };
}
