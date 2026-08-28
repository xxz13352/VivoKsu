import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createRomPage } from "../pages/rom.js";
import { createOverviewPage } from "../pages/overview.js";
import { createSessionsPage } from "../pages/sessions.js";
import { createUsersPage } from "../pages/users.js";
import { createVersionsPage } from "../pages/versions.js";

const doms = [];

function harness(api) {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", { url: "https://admin.example.test/" });
  doms.push(dom);
  const confirmations = [];
  const context = {
    document: dom.window.document,
    window: dom.window,
    api,
    navigate: vi.fn(),
    announce: vi.fn(),
    alert: vi.fn(),
    confirm: vi.fn((options) => confirmations.push(options)),
  };
  return { dom, context, confirmations };
}

afterEach(() => {
  while (doms.length > 0) doms.pop().window.close();
});

describe("complete administrator workspaces", () => {
  it.each([
    ["overview", createOverviewPage, { getTraceOverview: vi.fn().mockResolvedValue({}) }],
    ["versions", createVersionsPage, { getAppVersions: vi.fn().mockResolvedValue({}), getVersionSummary: vi.fn().mockResolvedValue({}) }],
    ["users", createUsersPage, { getUsers: vi.fn().mockResolvedValue({}) }],
    ["sessions", createSessionsPage, { getOnlineSessions: vi.fn().mockResolvedValue({}) }],
    ["rom", createRomPage, { getRomLogs: vi.fn().mockResolvedValue({}) }],
  ])("renders a real partial state for malformed %s success payloads", async (view, createPage, api) => {
    const { context } = harness(api);
    Object.defineProperty(context.document, "visibilityState", { configurable: true, value: "visible" });
    const page = createPage(context);
    await page.activate({ view }, new AbortController().signal);
    expect(page.element.dataset.pageState).toBe("partial");
    expect(page.element.querySelector('[role="alert"]')).not.toBeNull();
    page.destroy();
  });

  it("filters, creates, edits, toggles, and reloads version policy from authoritative APIs", async () => {
    const versions = [{ id: 9, version: "2.0.0", min_version: "1.0.0", download_url: "https://example.test/v2.zip", note: "stable", enabled: 1 }];
    const api = {
      getAppVersions: vi.fn().mockResolvedValue({ versions }),
      getVersionSummary: vi.fn().mockResolvedValue({ current_version: "2.0.0", minimum_version: "1.0.0", supported_versions: ["2.0.0"], today_426: 0 }),
      createAppVersion: vi.fn().mockResolvedValue({ ok: true }),
      updateAppVersion: vi.fn().mockResolvedValue({ ok: true }),
      deleteAppVersion: vi.fn().mockResolvedValue({ ok: true }),
    };
    const { context, confirmations } = harness(api);
    const page = createVersionsPage(context);
    await page.activate({ view: "versions" }, new AbortController().signal);
    expect(page.element.textContent).toContain("支持版本：2.0.0");
    page.element.querySelector('[data-action="delete-version"]').click();
    expect(confirmations[0].message).toContain("客户端准入");
    confirmations[0].onCancel();

    const search = page.element.querySelector('[data-filter="version-q"]');
    search.value = "missing";
    search.dispatchEvent(new context.window.Event("input", { bubbles: true }));
    expect(page.element.querySelector(".version-list").textContent).not.toContain("2.0.0");
    search.value = "2.0";
    search.dispatchEvent(new context.window.Event("input", { bubbles: true }));
    expect(page.element.querySelector(".version-list").textContent).toContain("2.0.0");

    page.element.querySelector('[name="version"]').value = "3.0.0";
    page.element.querySelector('[name="min_version"]').value = "2.0.0";
    page.element.querySelector('[name="download_url"]').value = "https://example.test/v3.zip";
    page.element.querySelector('[name="note"]').value = "next";
    page.element.querySelector('[data-form="create-version"]').dispatchEvent(new context.window.Event("submit", { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(api.createAppVersion).toHaveBeenCalledWith({
      version: "3.0.0", min_version: "2.0.0", download_url: "https://example.test/v3.zip", note: "next",
    }, expect.anything()));
    await vi.waitFor(() => expect(page.element.querySelector(".version-list")).not.toBeNull());

    page.element.querySelector('[data-action="toggle-version"]').click();
    await vi.waitFor(() => expect(api.updateAppVersion).toHaveBeenCalledWith(9, { enabled: false }, expect.anything()));
    await vi.waitFor(() => expect(page.element.querySelector('[data-action="edit-version"]')?.disabled).toBe(false));
    page.element.querySelector('[data-action="edit-version"]').click();
    const edit = page.element.querySelector('[data-form="edit-version"]');
    edit.querySelector('[name="min_version"]').value = "1.5.0";
    edit.dispatchEvent(new context.window.Event("submit", { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(api.updateAppVersion).toHaveBeenCalledWith(9, expect.objectContaining({ min_version: "1.5.0" }), expect.anything()));
  });

  it("searches, creates, resets, toggles, and shows authoritative user detail", async () => {
    const users = [{ id: 7, username: "alice", name: "Alice", enabled: 1, banned: 0, note: "operator", created_at: "2026-08-28" }];
    const api = {
      getUsers: vi.fn().mockResolvedValue({ users }),
      createUser: vi.fn().mockImplementation(async () => {
        users.push({ id: 8, username: "bob", name: "Bob", enabled: 1, banned: 0, note: "", created_at: "2026-08-28" });
        return { ok: true, id: 8, username: "bob", name: "Bob", token: "new-user-token" };
      }),
      updateUser: vi.fn().mockResolvedValue({ ok: true }),
      deleteUser: vi.fn().mockResolvedValue({ ok: true }),
      rotateUserToken: vi.fn().mockResolvedValue({ ok: true, token: "rotated-token" }),
    };
    const { context } = harness(api);
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(context.window.navigator, "clipboard", { configurable: true, value: { writeText } });
    const page = createUsersPage(context);
    await page.activate({ view: "users" }, new AbortController().signal);
    expect(page.element.textContent).toContain("operator");
    expect(page.element.textContent).toContain("2026-08-28");

    const search = page.element.querySelector('[data-filter="user-q"]');
    search.value = "missing";
    search.dispatchEvent(new context.window.Event("input", { bubbles: true }));
    expect(page.element.textContent).not.toContain("alice");
    search.value = "alice";
    search.dispatchEvent(new context.window.Event("input", { bubbles: true }));

    page.element.querySelector('[name="username"]').value = "bob";
    page.element.querySelector('[name="name"]').value = "Bob";
    page.element.querySelector('[name="password"]').value = "password-123";
    page.element.querySelector('[data-form="create-user"]').dispatchEvent(new context.window.Event("submit", { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(api.createUser).toHaveBeenCalledWith(expect.objectContaining({ username: "bob", name: "Bob", password: "password-123" }), expect.anything()));
    await vi.waitFor(() => expect(page.element.querySelector('[data-user-password="7"]')).not.toBeNull());
    page.element.querySelector('[data-action="copy-token"]').click();
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith("new-user-token"));
    writeText.mockRejectedValueOnce(new Error("clipboard denied"));
    page.element.querySelector('[data-action="copy-token"]').click();
    await vi.waitFor(() => expect(context.alert).toHaveBeenCalledWith("clipboard denied", { title: "复制失败" }));

    page.element.querySelector('[data-user-password="7"]').value = "replacement-123";
    page.element.querySelector('[data-action="reset-password"]').click();
    await vi.waitFor(() => expect(api.updateUser).toHaveBeenCalledWith(7, { newPassword: "replacement-123" }, expect.anything()));
    await vi.waitFor(() => expect(page.element.querySelector('[data-action="toggle-user-enabled"]')).not.toBeNull());
    page.element.querySelector('[data-action="toggle-user-enabled"]').click();
    await vi.waitFor(() => expect(api.updateUser).toHaveBeenCalledWith(7, { enabled: false }, expect.anything()));
  });

  it("renders complete session evidence and submits a bounded kick reason", async () => {
    const session = {
      session_id: "session-001", user_id: 7, username: "alice", name: "Alice", client_version: "3.10.4",
      ip: "203.0.113.7", connected_at: 1_787_500_000, last_seen_at: 1_787_500_100, duration_seconds: 100,
    };
    const api = {
      getOnlineSessions: vi.fn().mockResolvedValue({ sessions: [session] }),
      kickSession: vi.fn().mockResolvedValue({ ok: true }),
    };
    const { context, confirmations } = harness(api);
    Object.defineProperty(context.document, "visibilityState", { configurable: true, value: "visible" });
    const page = createSessionsPage(context);
    await page.activate({ view: "sessions" }, new AbortController().signal);
    for (const value of ["session-001", "203.0.113.7", "2026", "100 秒"]) {
      expect(page.element.textContent).toContain(value);
    }
    const reason = page.element.querySelector('[data-kick-reason="session-001"]');
    expect(reason.maxLength).toBe(200);
    reason.value = "operator requested logout";
    page.element.querySelector('[data-action="kick-session"]').click();
    await confirmations[0].onConfirm();
    expect(api.kickSession).toHaveBeenCalledWith({ sessionId: "session-001", reason: "operator requested logout" }, expect.anything());
    page.destroy();
  });

  it("shows ROM user/time evidence and writes documented filters into route state", async () => {
    const api = {
      getRomLogs: vi.fn().mockResolvedValue({
        items: [{ id: 1, user_id: 7, user_name: "Alice", pd: "PD1", version: "1.0", status: 500, url: null, failure_reason: "not found", detail_unavailable_reason: null, created_at_ms: 1_787_500_000_000 }],
        next_cursor: null,
      }),
    };
    const { context } = harness(api);
    const page = createRomPage(context);
    await page.activate({ view: "rom" }, new AbortController().signal);
    expect(page.element.textContent).toContain("Alice");
    expect(page.element.textContent).toContain("2026");

    for (const [name, value] of [["userId", "7"], ["pd", "PD1"], ["version", "1.0"], ["status", "500"], ["q", "not found"]]) {
      page.element.querySelector(`[name="${name}"]`).value = value;
    }
    page.element.querySelector('[data-form="rom-filters"]').dispatchEvent(new context.window.Event("submit", { bubbles: true, cancelable: true }));
    expect(context.navigate).toHaveBeenCalledWith({
      view: "rom", userId: "7", pd: "PD1", version: "1.0", status: "500", q: "not found", cursor: null,
    });
  });
});
