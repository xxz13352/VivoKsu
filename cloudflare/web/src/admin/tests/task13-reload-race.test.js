import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import { createUsersPage } from "../pages/users.js";
import { createVersionsPage } from "../pages/versions.js";

const doms = [];

function createDom() {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "https://admin.example.test/?view=overview",
  });
  doms.push(dom);
  return dom;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function context(document, api) {
  return {
    document,
    window: document.defaultView,
    api,
    announce: vi.fn(),
    alert: vi.fn(),
    confirm: vi.fn((options) => {
      void options.onConfirm?.();
      return { destroy: vi.fn() };
    }),
  };
}

function findRow(root, selector, text) {
  return [...root.querySelectorAll(selector)].find((row) => row.textContent.includes(text));
}

afterEach(() => {
  while (doms.length > 0) doms.pop().window.close();
});

describe("management workspace authoritative reload ordering", () => {
  it("never lets older edit or toggle reloads overwrite a newer version delete snapshot", async () => {
    const initial = [
      { id: 1, version: "1.0.0", min_version: "1.0.0", download_url: "https://old/1", note: "old-one", enabled: 1 },
      { id: 2, version: "2.0.0", min_version: "1.0.0", download_url: "https://old/2", note: "old-two", enabled: 1 },
      { id: 3, version: "3.0.0", min_version: "1.0.0", download_url: "https://old/3", note: "delete-me", enabled: 1 },
    ];
    const editMutation = deferred();
    const toggleMutation = deferred();
    const deleteMutation = deferred();
    const editReload = deferred();
    const toggleReload = deferred();
    const deleteReload = deferred();
    const reloads = [
      { versions: initial },
      editReload.promise,
      toggleReload.promise,
      deleteReload.promise,
    ];
    const api = {
      getAppVersions: vi.fn(() => Promise.resolve(reloads.shift()).then((value) => value)),
      getVersionSummary: vi.fn().mockResolvedValue({ current_version: "3.0.0", minimum_version: "1.0.0", supported_versions: ["3.0.0"], today_426: 0 }),
      updateAppVersion: vi.fn((id, body) => id === 1 && body.note !== undefined ? editMutation.promise : toggleMutation.promise),
      deleteAppVersion: vi.fn(() => deleteMutation.promise),
    };
    const dom = createDom();
    const page = createVersionsPage(context(dom.window.document, api));
    dom.window.document.body.append(page.element);
    await page.activate({}, new dom.window.AbortController().signal);

    const first = findRow(page.element, ".version-row", "1.0.0");
    first.querySelector('[data-action="edit-version"]').click();
    const editForm = first.querySelector('[data-form="edit-version"]');
    editForm.querySelector('[name="note"]').value = "edited-one";
    editForm.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
    findRow(page.element, ".version-row", "2.0.0").querySelector('[data-action="toggle-version"]').click();
    findRow(page.element, ".version-row", "3.0.0").querySelector('[data-action="delete-version"]').click();

    editMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(2));
    toggleMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(3));
    deleteMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(4));

    const finalVersions = [
      { ...initial[0], note: "edited-one" },
      { ...initial[1], enabled: 0 },
    ];
    deleteReload.resolve({ versions: finalVersions });
    await vi.waitFor(() => expect(page.element.textContent).toContain("edited-one"));
    toggleReload.resolve({ versions: [{ ...initial[0], note: "edited-one" }, { ...initial[1], enabled: 0 }, initial[2]] });
    editReload.resolve({ versions: [{ ...initial[0], note: "edited-one" }, initial[1], initial[2]] });
    await Promise.all([editReload.promise, toggleReload.promise, deleteReload.promise]);
    await new Promise((resolve) => dom.window.setTimeout(resolve, 0));

    expect(page.element.textContent).toContain("edited-one");
    expect(findRow(page.element, ".version-row", "2.0.0")?.textContent).toContain("已停用");
    expect(page.element.textContent).not.toContain("delete-me");
  });

  it("keeps the newest user authority and never revives a deleted token owner through older reloads", async () => {
    const initial = [
      { id: 1, username: "alice", name: "Alice", note: "initial-one", enabled: 1, banned: 0, created_at: "2026-01-01" },
      { id: 2, username: "bob", name: "Bob", note: "initial-two", enabled: 1, banned: 0, created_at: "2026-01-02" },
      { id: 3, username: "carol", name: "Carol", note: "initial-three", enabled: 1, banned: 0, created_at: "2026-01-03" },
      { id: 4, username: "dave", name: "Dave", note: "token-owner", enabled: 1, banned: 0, created_at: "2026-01-04" },
    ];
    const createMutation = deferred();
    const enableMutation = deferred();
    const banMutation = deferred();
    const rotateMutation = deferred();
    const deleteMutation = deferred();
    const createReload = deferred();
    const enableReload = deferred();
    const banReload = deferred();
    const rotateReload = deferred();
    const deleteReload = deferred();
    const reloads = [
      { users: initial },
      createReload.promise,
      enableReload.promise,
      banReload.promise,
      rotateReload.promise,
      deleteReload.promise,
    ];
    const api = {
      getUsers: vi.fn(() => Promise.resolve(reloads.shift()).then((value) => value)),
      createUser: vi.fn(() => createMutation.promise),
      updateUser: vi.fn((id, body) => id === 1 && body.enabled !== undefined ? enableMutation.promise : banMutation.promise),
      rotateUserToken: vi.fn(() => rotateMutation.promise),
      deleteUser: vi.fn(() => deleteMutation.promise),
    };
    const dom = createDom();
    const page = createUsersPage(context(dom.window.document, api));
    dom.window.document.body.append(page.element);
    await page.activate({}, new dom.window.AbortController().signal);

    const createForm = page.element.querySelector('[data-form="create-user"]');
    createForm.querySelector('[name="username"]').value = "erin";
    createForm.querySelector('[name="name"]').value = "Erin";
    createForm.querySelector('[name="password"]').value = "secret7";
    createForm.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
    findRow(page.element, ".user-row", "alice").querySelector('[data-action="toggle-user-enabled"]').click();
    findRow(page.element, ".user-row", "bob").querySelector('[data-action="toggle-ban"]').click();
    findRow(page.element, ".user-row", "dave").querySelector('[data-action="rotate-token"]').click();
    findRow(page.element, ".user-row", "carol").querySelector('[data-action="delete-user"]').click();
    page.element.querySelector('[data-filter="user-status"]').value = "banned";
    page.element.querySelector('[data-filter="user-status"]').dispatchEvent(new dom.window.Event("change", { bubbles: true }));

    createMutation.resolve({ ok: true, id: 5, username: "erin", name: "Erin", token: "created-token" });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(2));
    enableMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(3));
    banMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(4));
    rotateMutation.resolve({ ok: true, token: "rotated-token" });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(5));
    deleteMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(6));

    const erin = { id: 5, username: "erin", name: "Erin", note: "created", enabled: 1, banned: 0, created_at: "2026-01-05" };
    const finalUsers = [{ ...initial[0], enabled: 0 }, { ...initial[1], banned: 1 }, initial[3], erin];
    deleteReload.resolve({ users: finalUsers });
    await vi.waitFor(() => expect(page.element.textContent).toContain("已封禁"));
    rotateReload.resolve({ users: [...finalUsers, initial[2]] });
    banReload.resolve({ users: [{ ...initial[0], enabled: 0 }, { ...initial[1], banned: 1 }, initial[2], initial[3], erin] });
    enableReload.resolve({ users: [{ ...initial[0], enabled: 0 }, initial[1], initial[2], initial[3], erin] });
    createReload.resolve({ users: [...initial, erin] });
    await Promise.all([createReload.promise, enableReload.promise, banReload.promise, rotateReload.promise, deleteReload.promise]);
    await new Promise((resolve) => dom.window.setTimeout(resolve, 0));

    expect(page.element.textContent).toContain("bob");
    expect(page.element.textContent).toContain("已封禁");
    expect(page.element.textContent).not.toContain("carol");
    expect(page.element.textContent).toContain("rotated-token");
    expect(page.element.querySelector('[data-filter="user-status"]').value).toBe("banned");
  });

  it("keeps the last version snapshot stale after concurrent mutation reload failure and retries authority", async () => {
    const initial = [
      { id: 1, version: "1.0.0", min_version: "1.0.0", download_url: "https://old/1", note: "old-one", enabled: 1 },
      { id: 2, version: "2.0.0", min_version: "1.0.0", download_url: "https://old/2", note: "old-two", enabled: 1 },
      { id: 3, version: "3.0.0", min_version: "1.0.0", download_url: "https://old/3", note: "delete-me", enabled: 1 },
    ];
    const editMutation = deferred();
    const toggleMutation = deferred();
    const deleteMutation = deferred();
    const editReload = deferred();
    const toggleReload = deferred();
    const failedReload = deferred();
    const retryReload = deferred();
    const reloads = [{ versions: initial }, editReload.promise, toggleReload.promise, failedReload.promise, retryReload.promise];
    const api = {
      getAppVersions: vi.fn(() => Promise.resolve(reloads.shift()).then((value) => value)),
      getVersionSummary: vi.fn().mockResolvedValue({
        current_version: "3.0.0", minimum_version: "1.0.0", today_426: 0, supported_versions: [],
      }),
      updateAppVersion: vi.fn((id, body) => id === 1 && body.note !== undefined ? editMutation.promise : toggleMutation.promise),
      deleteAppVersion: vi.fn(() => deleteMutation.promise),
    };
    const dom = createDom();
    const page = createVersionsPage(context(dom.window.document, api));
    dom.window.document.body.append(page.element);
    await page.activate({}, new dom.window.AbortController().signal);

    const first = findRow(page.element, ".version-row", "1.0.0");
    first.querySelector('[data-action="edit-version"]').click();
    const editForm = first.querySelector('[data-form="edit-version"]');
    editForm.querySelector('[name="note"]').value = "edited-one";
    editForm.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
    findRow(page.element, ".version-row", "2.0.0").querySelector('[data-action="toggle-version"]').click();
    findRow(page.element, ".version-row", "3.0.0").querySelector('[data-action="delete-version"]').click();

    editMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(2));
    toggleMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(3));
    deleteMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(4));
    failedReload.reject(new Error("version reload unavailable"));
    editReload.resolve({ versions: [{ ...initial[0], note: "edited-one" }, initial[1], initial[2]] });
    toggleReload.resolve({ versions: [{ ...initial[0], note: "edited-one" }, { ...initial[1], enabled: 0 }, initial[2]] });

    await vi.waitFor(() => expect(page.element.querySelector('[data-action="retry-versions"]')).not.toBeNull());
    expect(page.element.querySelector('[role="alert"]')?.textContent).toContain("version reload unavailable");
    expect(page.element.textContent).toContain("old-one");
    expect(findRow(page.element, ".version-row", "2.0.0")?.textContent).toContain("已启用");
    expect(page.element.textContent).toContain("delete-me");

    page.element.querySelector('[data-action="retry-versions"]').click();
    await vi.waitFor(() => expect(api.getAppVersions).toHaveBeenCalledTimes(5));
    retryReload.resolve({ versions: [{ ...initial[0], note: "edited-one" }, { ...initial[1], enabled: 0 }] });
    await vi.waitFor(() => expect(page.element.textContent).toContain("edited-one"));
    expect(findRow(page.element, ".version-row", "2.0.0")?.textContent).toContain("已停用");
    expect(page.element.textContent).not.toContain("delete-me");
    expect(page.element.querySelector('[data-action="retry-versions"]')).toBeNull();
    expect(page.element.getAttribute("data-page-state")).toBe("ready");
  });

  it("keeps users and the current one-time token stale until retry confirms every concurrent mutation", async () => {
    const initial = [
      { id: 1, username: "alice", name: "Alice", note: "initial-one", enabled: 1, banned: 0, created_at: "2026-01-01" },
      { id: 2, username: "bob", name: "Bob", note: "initial-two", enabled: 1, banned: 0, created_at: "2026-01-02" },
      { id: 3, username: "carol", name: "Carol", note: "initial-three", enabled: 1, banned: 0, created_at: "2026-01-03" },
      { id: 4, username: "dave", name: "Dave", note: "token-owner", enabled: 1, banned: 0, created_at: "2026-01-04" },
    ];
    const createMutation = deferred();
    const enableMutation = deferred();
    const banMutation = deferred();
    const rotateMutation = deferred();
    const deleteMutation = deferred();
    const createReload = deferred();
    const enableReload = deferred();
    const banReload = deferred();
    const rotateReload = deferred();
    const failedReload = deferred();
    const retryReload = deferred();
    const reloads = [
      { users: initial }, createReload.promise, enableReload.promise, banReload.promise,
      rotateReload.promise, failedReload.promise, retryReload.promise,
    ];
    const api = {
      getUsers: vi.fn(() => Promise.resolve(reloads.shift()).then((value) => value)),
      createUser: vi.fn(() => createMutation.promise),
      updateUser: vi.fn((id, body) => id === 1 && body.enabled !== undefined ? enableMutation.promise : banMutation.promise),
      rotateUserToken: vi.fn(() => rotateMutation.promise),
      deleteUser: vi.fn(() => deleteMutation.promise),
    };
    const dom = createDom();
    const page = createUsersPage(context(dom.window.document, api));
    dom.window.document.body.append(page.element);
    await page.activate({}, new dom.window.AbortController().signal);

    const createForm = page.element.querySelector('[data-form="create-user"]');
    createForm.querySelector('[name="username"]').value = "erin";
    createForm.querySelector('[name="name"]').value = "Erin";
    createForm.querySelector('[name="password"]').value = "secret7";
    createForm.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
    findRow(page.element, ".user-row", "alice").querySelector('[data-action="toggle-user-enabled"]').click();
    findRow(page.element, ".user-row", "bob").querySelector('[data-action="toggle-ban"]').click();
    findRow(page.element, ".user-row", "dave").querySelector('[data-action="rotate-token"]').click();
    findRow(page.element, ".user-row", "carol").querySelector('[data-action="delete-user"]').click();

    createMutation.resolve({ ok: true, id: 5, username: "erin", name: "Erin", token: "created-token" });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(2));
    enableMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(3));
    banMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(4));
    rotateMutation.resolve({ ok: true, token: "rotated-token" });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(5));
    deleteMutation.resolve({ ok: true });
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(6));
    failedReload.reject(new Error("user reload unavailable"));
    createReload.resolve({ users: [...initial, { id: 5, username: "erin", name: "Erin", note: "created", enabled: 1, banned: 0 }] });
    enableReload.resolve({ users: [{ ...initial[0], enabled: 0 }, ...initial.slice(1)] });
    banReload.resolve({ users: [initial[0], { ...initial[1], banned: 1 }, ...initial.slice(2)] });
    rotateReload.resolve({ users: initial });

    await vi.waitFor(() => expect(page.element.querySelector('[data-action="retry-users"]')).not.toBeNull());
    expect(page.element.querySelector('[role="alert"]')?.textContent).toContain("user reload unavailable");
    expect(page.element.textContent).toContain("initial-one");
    expect(page.element.textContent).toContain("carol");
    expect(page.element.textContent).toContain("rotated-token");

    page.element.querySelector('[data-action="retry-users"]').click();
    await vi.waitFor(() => expect(api.getUsers).toHaveBeenCalledTimes(7));
    const erin = { id: 5, username: "erin", name: "Erin", note: "created", enabled: 1, banned: 0, created_at: "2026-01-05" };
    retryReload.resolve({ users: [{ ...initial[0], enabled: 0 }, { ...initial[1], banned: 1 }, initial[3], erin] });
    await vi.waitFor(() => expect(page.element.textContent).toContain("erin"));
    expect(page.element.textContent).not.toContain("carol");
    expect(page.element.textContent).toContain("rotated-token");
    expect(page.element.querySelector('[data-action="retry-users"]')).toBeNull();
    expect(page.element.getAttribute("data-page-state")).toBe("ready");
  });
});
