import { describe, expect, it, vi } from "vitest";

import {
  DEFAULT_ROUTE,
  VIEWS,
  createRouter,
  parseRoute,
  serializeRoute,
} from "../router.js";

const AUDIT_ROUTE = {
  view: "audit",
  userId: "42",
  runId: "v2:019d0000-0000-7000-8000-000000000001",
  eventId: "019d0000-0000-7000-8000-000000000002",
  level: "command",
  stream: "stderr",
  from: "2026-08-01T00:00:00Z",
  to: "2026-08-27T23:59:59Z",
  status: "failed",
  kind: "Flashing",
  partition: "super",
  errorCode: "FLASH_PARTITION_NOT_FOUND",
  q: "device stopped",
  cursor: "next:019d0000-0000-7000-8000-000000000003",
};
const EMPTY_AUDIT_ROUTE = Object.fromEntries(
  Object.keys(AUDIT_ROUTE).map(key => [key, key === "view" ? "audit" : null]),
);

describe("admin query route", () => {
  it("supports exactly the six documented views", () => {
    expect(VIEWS).toEqual([
      "overview",
      "versions",
      "users",
      "sessions",
      "rom",
      "audit",
    ]);

    for (const view of VIEWS) {
      expect(parseRoute(`?view=${view}`)).toEqual(view === "audit" ? EMPTY_AUDIT_ROUTE : { view });
      expect(serializeRoute({ view })).toBe(`?view=${view}`);
    }
  });

  it("round-trips a bounded audit deep link in canonical field order", () => {
    const encoded = serializeRoute(AUDIT_ROUTE);

    expect(parseRoute(encoded)).toEqual(AUDIT_ROUTE);
    expect(encoded).toBe(
      "?view=audit&userId=42&runId=v2%3A019d0000-0000-7000-8000-000000000001" +
        "&eventId=019d0000-0000-7000-8000-000000000002&level=command&stream=stderr" +
        "&from=2026-08-01T00%3A00%3A00Z&to=2026-08-27T23%3A59%3A59Z&status=failed" +
        "&kind=Flashing&partition=super&errorCode=FLASH_PARTITION_NOT_FOUND" +
        "&q=device+stopped&cursor=next%3A019d0000-0000-7000-8000-000000000003",
    );
  });

  it("round-trips the documented null audit fields without putting them in the URL", () => {
    const route = { ...AUDIT_ROUTE, from: null, to: null, q: null, cursor: null };

    expect(parseRoute(serializeRoute(route))).toEqual(route);
    expect(serializeRoute(route)).not.toMatch(/(?:from|to|q|cursor)=/);
  });

  it("never serializes secret or command-output fields", () => {
    const encoded = serializeRoute({
      ...AUDIT_ROUTE,
      token: "real-token",
      password: "real-password",
      cookie: "session=real-cookie",
      stdout: "private stdout",
      stderr: "private stderr",
      command: "flash super secret.img",
      argv: ["--token", "real-token"],
    });

    expect(encoded).not.toContain("real-token");
    expect(encoded).not.toContain("real-password");
    expect(encoded).not.toContain("real-cookie");
    expect(encoded).not.toContain("private+");
    expect(encoded).not.toContain("secret.img");
    expect(parseRoute(encoded)).toEqual(AUDIT_ROUTE);
  });

  it("drops query text that resembles credential or command content", () => {
    for (const q of [
      "token=top-secret",
      "password: top-secret",
      "passwd=top-secret",
      "api-key=top-secret",
      "api_key: top-secret",
      "secret=top-secret",
      "credential: top-secret",
      "--token top-secret",
      "Bearer top-secret",
      "client_secret=top-secret",
      "access_token=top-secret",
      "clientSecret=top-secret",
      "apiToken=top-secret",
      '{"token":"top-secret"}',
      "Authorization: Basic dXNlcjpwYXNz",
      "Proxy-Authorization: Digest opaque-credential",
      "Cookie: sid=top-secret",
      "Authorization: Bearer top-secret",
      "stdout=top-secret",
      "command: fastboot flash super secret.img",
    ]) {
      const encoded = serializeRoute({ view: "audit", q });
      expect(encoded).toBe("?view=audit");
      expect(parseRoute(`?view=audit&q=${encodeURIComponent(q)}`)).toEqual(EMPTY_AUDIT_ROUTE);
    }
  });

  it("strips audit-only state from other workspaces", () => {
    expect(parseRoute("?view=users&userId=42&status=failed")).toEqual({ view: "users" });
    expect(serializeRoute({ view: "rom", runId: "run-secret" })).toBe("?view=rom");
  });

  it.each([
    ["unknown view", "?view=settings"],
    ["repeated view", "?view=audit&view=users"],
    ["repeated filter", "?view=audit&status=failed&status=success"],
    ["too long", `?view=audit&q=${"x".repeat(2049)}`],
    ["malformed percent", "?view=audit&q=%E0%A4%A"],
    ["invalid UTF-8", "?view=audit&q=%C0%AF"],
  ])("safely downgrades %s", (_case, search) => {
    expect(parseRoute(search)).toEqual(DEFAULT_ROUTE);
  });

  it("drops unknown parameters and canonicalizes invalid bounded filters", () => {
    expect(parseRoute("?view=audit&unknown=ignored&level=not-a-level&stream=combined")).toEqual(
      EMPTY_AUDIT_ROUTE,
    );
    expect(serializeRoute({ view: "audit", userId: "x".repeat(129) })).toBe("?view=audit");
  });
});

describe("admin browser router", () => {
  it("canonicalizes with replaceState and navigates with pushState", async () => {
    const source = element("source-row");
    const heading = element("page-heading");
    const browser = fakeWindow("/?view=audit&userId=42&unknown=drop#not-a-route");
    browser.document.activeElement = source;
    browser.document.querySelector = selector =>
      selector === "[data-route-heading]" ? heading : null;
    browser.scrollY = 321;
    const onRoute = vi.fn();
    const router = createRouter({ window: browser, onRoute });

    await router.start();
    expect(browser.history.replaceState).toHaveBeenCalledWith(
      expect.objectContaining({ focusId: null, scrollY: 0 }),
      "",
      "/?view=audit&userId=42",
    );
    expect(browser.history.pushState).not.toHaveBeenCalled();
    expect(onRoute).toHaveBeenLastCalledWith(
      { ...EMPTY_AUDIT_ROUTE, userId: "42" },
      expect.objectContaining({ navigationType: "start" }),
    );

    browser.document.activeElement = source;
    await router.navigate({ view: "users" });
    expect(browser.history.replaceState).toHaveBeenLastCalledWith(
      expect.objectContaining({ focusId: "source-row", scrollY: 321 }),
      "",
      "/?view=audit&userId=42",
    );
    expect(browser.history.pushState).toHaveBeenCalledOnce();
    expect(browser.history.pushState).toHaveBeenCalledWith(
      expect.objectContaining({ focusId: null, scrollY: 0 }),
      "",
      "/?view=users",
    );
    expect(onRoute).toHaveBeenLastCalledWith(
      { view: "users" },
      expect.objectContaining({ navigationType: "push" }),
    );
    expect(heading.focus).toHaveBeenCalledWith({ preventScroll: true });
  });

  it("restores the source focus and scroll position on Back without pushing", async () => {
    const source = element("source-row");
    const browser = fakeWindow("/?view=users");
    browser.document.getElementById = id => (id === "source-row" ? source : null);
    const onRoute = vi.fn();
    const router = createRouter({ window: browser, onRoute });
    await router.start();
    browser.history.pushState.mockClear();

    browser.setUrl("/?view=audit&runId=run-1");
    browser.dispatch("popstate", {
      state: { adminRouter: 1, focusId: "source-row", scrollY: 444 },
    });
    await flushNavigation();

    expect(onRoute).toHaveBeenLastCalledWith(
      { ...EMPTY_AUDIT_ROUTE, runId: "run-1" },
      expect.objectContaining({ navigationType: "pop" }),
    );
    expect(browser.history.pushState).not.toHaveBeenCalled();
    expect(source.focus).toHaveBeenCalledWith({ preventScroll: true });
    expect(browser.scrollTo).toHaveBeenCalledWith(0, 444);
  });

  it("captures and restores a safe data focus id when the source has no DOM id", async () => {
    const source = element("");
    source.dataset.routerFocusId = "run-row:42";
    const heading = element("page-heading");
    const browser = fakeWindow("/?view=audit&level=run");
    browser.document.activeElement = source;
    browser.document.querySelector = selector => selector === "[data-route-heading]" ? heading : null;
    browser.document.querySelectorAll = selector => selector === "[data-router-focus-id]" ? [source] : [];
    browser.scrollY = 286;
    const router = createRouter({ window: browser, onRoute: vi.fn() });
    await router.start();

    browser.document.activeElement = source;
    await router.navigate({ view: "rom" });
    expect(browser.history.replaceState).toHaveBeenLastCalledWith(
      expect.objectContaining({ focusId: "run-row:42", scrollY: 286 }),
      "",
      "/?view=audit&level=run",
    );

    browser.setUrl("/?view=audit&level=run");
    browser.dispatch("popstate", {
      state: { adminRouter: 1, focusId: "run-row:42", scrollY: 286 },
    });
    await flushNavigation();
    expect(source.focus).toHaveBeenLastCalledWith({ preventScroll: true });
    expect(browser.scrollTo).toHaveBeenLastCalledWith(0, 286);
  });

  it("uses a stable fallback when the source focus target no longer exists", async () => {
    const fallback = element("users-tab");
    const browser = fakeWindow("/?view=users");
    browser.document.querySelector = selector =>
      selector === "[data-router-focus-fallback]" ? fallback : null;
    const router = createRouter({ window: browser, onRoute: vi.fn() });
    await router.start();

    expect(router.restoreReturnPoint({ focusId: "removed-row", scrollY: 98 })).toBe(true);
    expect(fallback.focus).toHaveBeenCalledWith({ preventScroll: true });
    expect(browser.scrollTo).toHaveBeenLastCalledWith(0, 98);
  });

  it("canonicalizes a malformed route with replaceState", async () => {
    const browser = fakeWindow("/?view=audit&q=%E0%A4%A#ignored");
    const onRoute = vi.fn();
    const router = createRouter({ window: browser, onRoute });

    await router.start();

    expect(onRoute).toHaveBeenCalledWith(
      DEFAULT_ROUTE,
      expect.objectContaining({ navigationType: "start", canonicalized: true }),
    );
    expect(browser.history.replaceState).toHaveBeenCalledWith(
      expect.any(Object),
      "",
      "/?view=overview",
    );
  });

  it("removes its popstate listener on destroy", async () => {
    const browser = fakeWindow("/?view=overview");
    const onRoute = vi.fn();
    const router = createRouter({ window: browser, onRoute });
    await router.start();
    onRoute.mockClear();

    router.destroy();
    browser.setUrl("/?view=rom");
    browser.dispatch("popstate", { state: null });
    await flushNavigation();

    expect(browser.removeEventListener).toHaveBeenCalledWith("popstate", expect.any(Function));
    expect(onRoute).not.toHaveBeenCalled();
  });
});

function element(id) {
  return {
    id,
    dataset: {},
    focus: vi.fn(),
    hasAttribute: vi.fn(() => true),
    setAttribute: vi.fn(),
  };
}

function fakeWindow(initialUrl) {
  let current = new URL(initialUrl, "https://admin.example.test");
  const listeners = new Map();
  const browser = {
    document: {
      activeElement: null,
      getElementById: () => null,
      querySelector: () => null,
      querySelectorAll: () => [],
    },
    scrollY: 0,
    scrollTo: vi.fn(),
    addEventListener: vi.fn((type, listener) => listeners.set(type, listener)),
    removeEventListener: vi.fn((type, listener) => {
      if (listeners.get(type) === listener) listeners.delete(type);
    }),
    history: {
      state: null,
      replaceState: vi.fn((state, _title, url) => {
        browser.history.state = state;
        current = new URL(url, current);
      }),
      pushState: vi.fn((state, _title, url) => {
        browser.history.state = state;
        current = new URL(url, current);
      }),
    },
    setUrl(url) {
      current = new URL(url, current);
    },
    dispatch(type, event) {
      listeners.get(type)?.(event);
    },
  };
  Object.defineProperty(browser, "location", { get: () => current });
  return browser;
}

async function flushNavigation() {
  await Promise.resolve();
  await Promise.resolve();
}
