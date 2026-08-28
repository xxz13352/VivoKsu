import { afterEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";

import {
  ADMIN_MENU_ITEMS,
  PAGE_STATES,
  announceStatus,
  createAdminMenu,
  createConfirmationDialog,
  createCursorControls,
  createHistoryFocusReturn,
  createSafeElement,
  isCurrentPageActivation,
  renderPageState,
  showPersistentAlert,
} from "../components.js";

const doms = [];

function createDom(url = "https://admin.example.test/admin/") {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", { url });
  doms.push(dom);
  return dom;
}

afterEach(() => {
  vi.useRealTimers();
  while (doms.length > 0) doms.pop().window.close();
});

describe("DOM-safe primitives", () => {
  it("renders external values as text and rejects executable attributes", () => {
    const { window } = createDom();
    const payload = '<img src=x onerror="globalThis.pwned=true">';
    const element = createSafeElement(window.document, "p", { className: "copy", "aria-label": payload }, payload);

    expect(element.textContent).toBe(payload);
    expect(element.querySelector("img")).toBeNull();
    expect(element.getAttribute("aria-label")).toBe(payload);
    expect(element.innerHTML).toContain("&lt;img src=x");
    expect(() => createSafeElement(window.document, "p", { onclick: "alert(1)" }, "bad")).toThrow(
      /attribute/i,
    );
    expect(() => createSafeElement(window.document, "a", { href: "javascript:alert(1)" }, "bad")).toThrow(
      /URL/i,
    );
  });
});

describe("page states and announcements", () => {
  it.each(PAGE_STATES)("renders the %s page state without interpolating HTML", (state) => {
    const { window } = createDom();
    const container = window.document.createElement("section");
    const retry = vi.fn();
    const payload = `<svg onload=alert('${state}')>`;

    renderPageState(container, { state, title: payload, message: payload, onRetry: retry });

    expect(container.dataset.pageState).toBe(state);
    expect(container.textContent).toContain(payload);
    expect(container.querySelector("svg")).toBeNull();
    if (["partial", "stale", "unauthorized", "error", "retry"].includes(state)) {
      expect(container.querySelector('[role="alert"]')).not.toBeNull();
    } else {
      expect(container.querySelector('[role="status"]')).not.toBeNull();
    }
    if (state === "retry") {
      container.querySelector("button").click();
      expect(retry).toHaveBeenCalledOnce();
    }
  });

  it("clears short status announcements and keeps persistent alerts until explicitly cleared", () => {
    vi.useFakeTimers();
    const { window } = createDom();
    const status = window.document.createElement("div");
    const alerts = window.document.createElement("div");

    const cancel = announceStatus(status, "Saved", { durationMs: 500 });
    const dismiss = showPersistentAlert(alerts, { message: "Connection lost", kind: "error" });
    expect(status.getAttribute("role")).toBe("status");
    expect(status.textContent).toBe("Saved");
    expect(alerts.querySelector('[role="alert"]').textContent).toContain("Connection lost");

    vi.advanceTimersByTime(500);
    expect(status.textContent).toBe("");
    expect(alerts.textContent).toContain("Connection lost");

    cancel();
    dismiss();
    expect(alerts.textContent).toBe("");
  });
});

describe("cursor controls", () => {
  it("exposes bounded cursor actions and removes listeners on destroy", () => {
    const { window } = createDom();
    const previous = vi.fn();
    const next = vi.fn();
    const controls = createCursorControls({ document: window.document, onPrevious: previous, onNext: next });
    window.document.body.append(controls.element);
    const [previousButton, nextButton] = controls.element.querySelectorAll("button");

    controls.update({ hasPrevious: false, hasNext: true, pageLabel: "第 2 页" });
    expect(previousButton.disabled).toBe(true);
    expect(nextButton.disabled).toBe(false);
    expect(controls.element.textContent).toContain("第 2 页");
    nextButton.click();
    expect(next).toHaveBeenCalledOnce();

    controls.destroy();
    nextButton.click();
    expect(next).toHaveBeenCalledOnce();
  });
});

describe("six-item menu", () => {
  it("implements one current item and roving Arrow/Home/End keyboard focus", () => {
    const { window } = createDom();
    const selected = vi.fn();
    const menu = createAdminMenu({ document: window.document, activeId: "overview", onSelect: selected });
    window.document.body.append(menu.element);
    const items = [...menu.element.querySelectorAll("[data-menu-id]")];

    expect(ADMIN_MENU_ITEMS).toHaveLength(6);
    expect(items.map((item) => item.textContent)).toEqual([
      "概览",
      "版本策略",
      "用户管理",
      "在线会话",
      "操作审计",
      "ROM 查询",
    ]);
    expect(items.filter((item) => item.getAttribute("aria-current") === "page")).toHaveLength(1);
    expect(items.map((item) => item.tabIndex)).toEqual([0, -1, -1, -1, -1, -1]);

    items[0].focus();
    items[0].dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(window.document.activeElement).toBe(items[1]);
    expect(items[1].getAttribute("aria-current")).toBe("page");

    items[1].dispatchEvent(new window.KeyboardEvent("keydown", { key: "End", bubbles: true }));
    expect(window.document.activeElement).toBe(items[5]);
    items[5].dispatchEvent(new window.KeyboardEvent("keydown", { key: "Home", bubbles: true }));
    expect(window.document.activeElement).toBe(items[0]);
    expect(selected.mock.calls.map(([id]) => id)).toEqual(["versions", "rom", "overview"]);

    menu.destroy();
    items[0].dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(selected).toHaveBeenCalledTimes(3);
  });

  it("rejects a menu contract that is not exactly six unique entries", () => {
    const { window } = createDom();
    expect(() => createAdminMenu({ document: window.document, items: ADMIN_MENU_ITEMS.slice(0, 5) })).toThrow(
      /six/i,
    );
    expect(() =>
      createAdminMenu({
        document: window.document,
        items: ADMIN_MENU_ITEMS.map((item, index) => (index === 5 ? { ...item, id: "overview" } : item)),
      }),
    ).toThrow(/unique/i);
  });
});

describe("confirmation dialog", () => {
  it("focuses Cancel, traps Tab, closes on Escape, and restores the trigger", () => {
    const { window } = createDom();
    const trigger = window.document.createElement("button");
    trigger.textContent = "Delete";
    window.document.body.append(trigger);
    trigger.focus();
    const cancelled = vi.fn();
    const dialog = createConfirmationDialog({
      document: window.document,
      title: "Delete user",
      message: '<img src=x onerror="steal()">',
      confirmLabel: "Delete",
      onCancel: cancelled,
    });
    window.document.body.append(dialog.element);

    dialog.open(trigger);
    const [cancel, confirm] = dialog.element.querySelectorAll("button");
    expect(window.document.activeElement).toBe(cancel);
    expect(dialog.element.querySelector("img")).toBeNull();

    cancel.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true }));
    expect(window.document.activeElement).toBe(confirm);
    confirm.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(window.document.activeElement).toBe(cancel);

    dialog.element.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(dialog.isOpen()).toBe(false);
    expect(cancelled).toHaveBeenCalledOnce();
    expect(window.document.activeElement).toBe(trigger);
  });

  it("prevents duplicate confirmation and remains open when the action fails", async () => {
    const { window } = createDom();
    let reject;
    const pending = new Promise((_, rejectPromise) => {
      reject = rejectPromise;
    });
    const confirmAction = vi.fn(() => pending);
    const dialog = createConfirmationDialog({
      document: window.document,
      title: "Ban user",
      message: "This action is dangerous",
      onConfirm: confirmAction,
    });
    window.document.body.append(dialog.element);
    dialog.open();
    const confirm = dialog.element.querySelector('[data-dialog-action="confirm"]');

    confirm.click();
    confirm.click();
    expect(confirmAction).toHaveBeenCalledOnce();
    expect(confirm.disabled).toBe(true);
    reject(new Error("Server unavailable"));
    await vi.waitFor(() => expect(dialog.element.querySelector('[role="alert"]')?.textContent).toContain("Server unavailable"));
    expect(dialog.isOpen()).toBe(true);
    expect(confirm.disabled).toBe(false);
  });
});

describe("history focus return", () => {
  it("stores a non-sensitive return id, restores focus on popstate, and can destroy its listener", async () => {
    const { window } = createDom();
    window.scrollTo = vi.fn();
    const trigger = window.document.createElement("button");
    trigger.textContent = "Open detail";
    window.document.body.append(trigger);
    const helper = createHistoryFocusReturn({ window });

    const id = helper.remember(trigger);
    expect(id).toMatch(/^focus-return-/);
    expect(window.history.state.focusId).toBe(id);
    expect(JSON.stringify(window.history.state)).not.toContain("Open detail");

    window.document.body.focus();
    window.dispatchEvent(new window.PopStateEvent("popstate", { state: { focusId: id, scrollY: 0 } }));
    await Promise.resolve();
    expect(window.document.activeElement).toBe(trigger);

    helper.destroy();
    const other = window.document.createElement("button");
    window.document.body.append(other);
    other.focus();
    window.dispatchEvent(new window.PopStateEvent("popstate", { state: { focusId: id, scrollY: 0 } }));
    await Promise.resolve();
    expect(window.document.activeElement).toBe(other);
  });
});

describe("page activation guard", () => {
  it("accepts only the current non-aborted page/controller pair", () => {
    const page = {};
    const otherPage = {};
    const controller = new AbortController();
    const otherController = new AbortController();

    expect(isCurrentPageActivation(page, controller, page, controller)).toBe(true);
    expect(isCurrentPageActivation(page, controller, otherPage, controller)).toBe(false);
    expect(isCurrentPageActivation(page, controller, page, otherController)).toBe(false);
    controller.abort();
    expect(isCurrentPageActivation(page, controller, page, controller)).toBe(false);
  });
});
