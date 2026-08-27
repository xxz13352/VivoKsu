export const VIEWS = Object.freeze([
  "overview",
  "versions",
  "users",
  "sessions",
  "rom",
  "audit",
]);

export const DEFAULT_ROUTE = Object.freeze({ view: "overview" });

const MAX_SEARCH_LENGTH = 2048;
const MAX_SCROLL_Y = 10_000_000;
const ROUTER_STATE_VERSION = 1;
const AUDIT_FIELDS = Object.freeze([
  "userId",
  "runId",
  "eventId",
  "level",
  "stream",
  "from",
  "to",
  "status",
  "kind",
  "partition",
  "errorCode",
  "q",
  "cursor",
]);
const LEVELS = new Set(["overview", "user", "run", "event", "command", "output"]);
const STREAMS = new Set(["stdout", "stderr"]);
const TOKEN_VALUE = /^[\p{L}\p{N}_.:@+/-]+$/u;
const ID_VALUE = /^[A-Za-z0-9_.:-]+$/;
const DATE_VALUE = /^[0-9T:.+Z-]+$/;
const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/;
// Keep this grammar synchronized with api.js so history and HTTP share one boundary.
const SENSITIVE_QUERY_PATTERNS = Object.freeze([
  /(?:^|[\s,;])(?:proxy-)?authorization\s*:\s*[A-Za-z][A-Za-z0-9._-]*\s+\S+/i,
  /(?:^|[\s,;])(?:bearer|basic)\s+\S+/i,
  /(?:^|\s)--?[A-Za-z0-9_-]*(?:password|passwd|token|api[-_]?key|secret|cookie|credential)(?:\s+|=)\S+/i,
  /(?:^|[^A-Za-z0-9])["']?[A-Za-z0-9_-]*(?:password|passwd|token|api[-_]?key|secret|cookie|credential|authorization|stdout|stderr|command|argv)["']?\s*[:=]\s*["']?\S+/i,
]);

const FIELD_RULES = Object.freeze({
  userId: value => boundedPattern(value, 128, ID_VALUE),
  runId: value => boundedPattern(value, 128, ID_VALUE),
  eventId: value => boundedPattern(value, 128, ID_VALUE),
  level: value => (LEVELS.has(value) ? value : null),
  stream: value => (STREAMS.has(value) ? value : null),
  from: value => boundedPattern(value, 40, DATE_VALUE),
  to: value => boundedPattern(value, 40, DATE_VALUE),
  status: value => boundedPattern(value, 32, TOKEN_VALUE),
  kind: value => boundedPattern(value, 64, TOKEN_VALUE),
  partition: value => boundedPattern(value, 64, TOKEN_VALUE),
  errorCode: value => boundedPattern(value, 128, TOKEN_VALUE),
  q: value => boundedText(value, 256, { rejectSensitive: true }),
  cursor: value => boundedText(value, 512),
});

/**
 * Parse a location.search value into a small, non-sensitive route object.
 * Any ambiguous query representation falls back to the overview workspace.
 */
export function parseRoute(search) {
  if (typeof search !== "string" || search.length > MAX_SEARCH_LENGTH || !hasValidPercentEncoding(search)) {
    return { ...DEFAULT_ROUTE };
  }

  const source = search.startsWith("?") ? search.slice(1) : search;
  const params = new URLSearchParams(source);
  const seen = new Set();
  for (const [key] of params) {
    if (seen.has(key)) return { ...DEFAULT_ROUTE };
    seen.add(key);
  }

  const requestedView = params.get("view") ?? DEFAULT_ROUTE.view;
  if (!VIEWS.includes(requestedView)) return { ...DEFAULT_ROUTE };

  const route = { view: requestedView };
  if (requestedView !== "audit") return route;

  for (const field of AUDIT_FIELDS) route[field] = null;

  for (const field of AUDIT_FIELDS) {
    const rawValue = params.get(field);
    if (rawValue === null) continue;
    const value = FIELD_RULES[field](rawValue);
    if (value !== null) route[field] = value;
  }
  return route;
}

/** Serialize a route in one stable order using only the public query contract. */
export function serializeRoute(route) {
  const view = VIEWS.includes(route?.view) ? route.view : DEFAULT_ROUTE.view;
  const params = new URLSearchParams([["view", view]]);
  if (view === "audit") {
    for (const field of AUDIT_FIELDS) {
      const rawValue = route?.[field];
      if (typeof rawValue !== "string") continue;
      const value = FIELD_RULES[field](rawValue);
      if (value !== null) params.set(field, value);
    }
  }
  return `?${params.toString()}`;
}

/**
 * Owns query routing and browser history only. `onRoute` performs page rendering.
 * Methods return promises so callers can wait for async page activation and focus.
 */
export function createRouter({ window, onRoute }) {
  if (!window?.history || !window?.location || !window?.document) {
    throw new TypeError("createRouter requires a browser-like window");
  }
  if (typeof onRoute !== "function") {
    throw new TypeError("createRouter requires onRoute");
  }

  let started = false;
  let destroyed = false;
  let transition = 0;

  function canonicalize(state = window.history.state) {
    const route = parseRoute(window.location.search);
    const search = serializeRoute(route);
    const url = `${window.location.pathname}${search}`;
    const normalizedState = normalizeHistoryState(state);
    const canonicalized =
      window.location.search !== search ||
      Boolean(window.location.hash) ||
      !isNormalizedHistoryState(state, normalizedState);
    if (canonicalized) window.history.replaceState(normalizedState, "", url);
    return { route, canonicalized, state: normalizedState, url };
  }

  async function render(route, navigationType, state, canonicalized = false) {
    const currentTransition = ++transition;
    await onRoute(route, { navigationType, canonicalized });
    if (destroyed || currentTransition !== transition) return route;

    if (navigationType === "pop") restoreReturnPoint(state);
    else focusRouteHeading();
    return route;
  }

  function start() {
    if (destroyed) throw new Error("router is destroyed");
    if (!started) {
      window.addEventListener("popstate", handlePopState);
      started = true;
    }
    const current = canonicalize();
    return render(current.route, "start", current.state, current.canonicalized);
  }

  function navigate(route, options = {}) {
    if (destroyed) throw new Error("router is destroyed");
    const target = parseRoute(serializeRoute(route));
    const sourceState = captureReturnPoint(options.focusId);
    const currentUrl = `${window.location.pathname}${window.location.search}`;
    window.history.replaceState(sourceState, "", currentUrl);

    const targetState = emptyHistoryState();
    window.history.pushState(
      targetState,
      "",
      `${window.location.pathname}${serializeRoute(target)}`,
    );
    return render(target, "push", targetState);
  }

  function handlePopState(event) {
    if (destroyed) return;
    const current = canonicalize(event?.state);
    void render(current.route, "pop", current.state, current.canonicalized);
  }

  function captureReturnPoint(explicitFocusId) {
    const activeElement = window.document.activeElement;
    const focusId = normalizeFocusId(explicitFocusId)
      ?? normalizeFocusId(activeElement?.id)
      ?? normalizeFocusId(activeElement?.dataset?.routerFocusId);
    return {
      adminRouter: ROUTER_STATE_VERSION,
      focusId,
      scrollY: normalizeScrollY(window.scrollY),
    };
  }

  function restoreReturnPoint(state = window.history.state) {
    const normalized = normalizeHistoryState(state);
    const target =
      (normalized.focusId && window.document.getElementById(normalized.focusId)) ||
      findDataFocusTarget(window.document, normalized.focusId) ||
      firstMatch(window.document, [
        "[data-router-focus-fallback]",
        '[aria-current="page"]',
        "[data-route-heading]",
        "[data-page-title]",
        "h1",
      ]);
    focusElement(target);
    window.scrollTo(0, normalized.scrollY);
    return Boolean(target);
  }

  function focusRouteHeading() {
    const heading = firstMatch(window.document, [
      "[data-route-heading]",
      "[data-page-title]",
      "h1",
    ]);
    return focusElement(heading);
  }

  function destroy() {
    if (!started || destroyed) return;
    destroyed = true;
    transition += 1;
    window.removeEventListener("popstate", handlePopState);
  }

  return { start, navigate, canonicalize, restoreReturnPoint, destroy };
}

function findDataFocusTarget(document, focusId) {
  if (!focusId || typeof document.querySelectorAll !== "function") return null;
  return [...document.querySelectorAll("[data-router-focus-id]")].find(
    (element) => element?.dataset?.routerFocusId === focusId,
  ) ?? null;
}

function boundedPattern(value, maxLength, pattern) {
  if (typeof value !== "string" || value.length === 0 || value.length > maxLength) return null;
  return pattern.test(value) ? value : null;
}

function boundedText(value, maxLength, { rejectSensitive = false } = {}) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > maxLength ||
    CONTROL_CHARACTER.test(value) ||
    (rejectSensitive && containsSensitiveQueryText(value))
  ) {
    return null;
  }
  return value;
}

function containsSensitiveQueryText(value) {
  const candidates = [value];
  try {
    const decoded = decodeURIComponent(value.replace(/\+/g, "%20"));
    if (decoded !== value) candidates.push(decoded);
  } catch {
    // Malformed percent text is already rejected at the route boundary.
  }
  return candidates.some((candidate) => SENSITIVE_QUERY_PATTERNS.some((pattern) => pattern.test(candidate)));
}

function hasValidPercentEncoding(value) {
  if (/%(?![0-9A-Fa-f]{2})/.test(value)) return false;
  try {
    decodeURIComponent(value.replace(/\+/g, "%20"));
    return true;
  } catch {
    return false;
  }
}

function emptyHistoryState() {
  return { adminRouter: ROUTER_STATE_VERSION, focusId: null, scrollY: 0 };
}

function normalizeHistoryState(state) {
  return {
    adminRouter: ROUTER_STATE_VERSION,
    focusId: normalizeFocusId(state?.focusId),
    scrollY: normalizeScrollY(state?.scrollY),
  };
}

function isNormalizedHistoryState(candidate, normalized) {
  return (
    candidate?.adminRouter === normalized.adminRouter &&
    candidate?.focusId === normalized.focusId &&
    candidate?.scrollY === normalized.scrollY &&
    Object.keys(candidate).every(key => ["adminRouter", "focusId", "scrollY"].includes(key))
  );
}

function normalizeFocusId(value) {
  if (typeof value !== "string" || value.length === 0 || value.length > 128) return null;
  return CONTROL_CHARACTER.test(value) ? null : value;
}

function normalizeScrollY(value) {
  if (!Number.isFinite(value)) return 0;
  return Math.min(MAX_SCROLL_Y, Math.max(0, Math.round(value)));
}

function firstMatch(document, selectors) {
  for (const selector of selectors) {
    const match = document.querySelector(selector);
    if (match) return match;
  }
  return null;
}

function focusElement(element) {
  if (!element || typeof element.focus !== "function") return false;
  if (typeof element.hasAttribute === "function" && !element.hasAttribute("tabindex")) {
    element.setAttribute("tabindex", "-1");
  }
  element.focus({ preventScroll: true });
  return true;
}
