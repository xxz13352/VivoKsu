const API_ROOT = "https://admin.invalid";
const MUTATION_METHODS = new Set(["POST", "PUT", "PATCH", "DELETE"]);
const SENSITIVE_QUERY_KEY = /(?:password|passwd|token|api[-_]?key|secret|cookie|credential|authorization)/i;
// Keep this grammar synchronized with router.js. It rejects secrets before URL construction.
const SENSITIVE_QUERY_PATTERNS = Object.freeze([
  /(?:^|[\s,;])(?:proxy-)?authorization\s*:\s*[A-Za-z][A-Za-z0-9._-]*\s+\S+/i,
  /(?:^|[\s,;])(?:bearer|basic)\s+\S+/i,
  /(?:^|\s)--?[A-Za-z0-9_-]*(?:password|passwd|token|api[-_]?key|secret|cookie|credential)(?:\s+|=)\S+/i,
  /(?:^|[^A-Za-z0-9])["']?[A-Za-z0-9_-]*(?:password|passwd|token|api[-_]?key|secret|cookie|credential|authorization|stdout|stderr|command|argv)["']?\s*[:=]\s*["']?\S+/i,
]);
const SENSITIVE_HEADER = /^(?:authorization|cookie|proxy-authorization|x-api-key|x-auth-token)$/i;
const CONTROL_CHARACTERS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g;
const SECRET_ASSIGNMENT = /\b(bearer|password|passwd|token|api[-_ ]?key|secret|cookie|credential|authorization)(\s*(?:[:=]|\bis\b)?\s*)([^\s,;]+)/gi;
const OPAQUE_SECRET = /\b(?:[A-Fa-f0-9]{24,}|[A-Za-z0-9_-]{32,}|[A-Za-z0-9+/]{28,}={0,2})\b/g;

const STATUS_DEFAULTS = Object.freeze({
  400: ["http", "ADMIN_BAD_REQUEST", "请求参数无效。"],
  401: ["unauthorized", "ADMIN_UNAUTHORIZED", "未登录或会话已过期。"],
  403: ["forbidden", "ADMIN_FORBIDDEN", "无权执行该操作。"],
  404: ["http", "ADMIN_NOT_FOUND", "请求的资源不存在。"],
  409: ["http", "ADMIN_CONFLICT", "请求与当前状态冲突。"],
  426: ["update_required", "UPDATE_REQUIRED", "当前客户端需要更新。"],
});

export class AdminApiError extends Error {
  constructor(message, options = {}) {
    super(message);
    this.name = "AdminApiError";
    this.kind = options.kind ?? "http";
    this.status = options.status ?? 0;
    this.code = options.code ?? "ADMIN_HTTP_ERROR";
    this.requestId = options.requestId ?? null;
    this.details = options.details ?? null;
    this.update = options.update ?? null;
  }
}

export function createApiClient({ fetchImpl = globalThis.fetch, onUnauthorized = () => {} } = {}) {
  if (typeof fetchImpl !== "function") {
    throw new TypeError("fetchImpl must be a function");
  }
  if (typeof onUnauthorized !== "function") {
    throw new TypeError("onUnauthorized must be a function");
  }

  async function request(path, options = {}) {
    let url;
    let headers;
    try {
      url = buildApiUrl(path, options.query);
      headers = buildHeaders(options.headers);
    } catch {
      throw invalidRequest();
    }

    const method = String(options.method ?? "GET").toUpperCase();
    const hasBody = options.body !== undefined;
    if (MUTATION_METHODS.has(method)) {
      headers.set("X-Requested-With", "XMLHttpRequest");
    } else {
      headers.delete("X-Requested-With");
    }
    if (hasBody) {
      headers.set("Content-Type", "application/json");
    }

    let serializedBody;
    if (hasBody) {
      try {
        serializedBody = JSON.stringify(options.body);
        if (serializedBody === undefined) throw new TypeError("body is not JSON serializable");
      } catch {
        throw invalidRequest();
      }
    }

    let response;
    try {
      response = await fetchImpl(url, {
        method,
        credentials: "same-origin",
        headers,
        body: serializedBody,
        signal: options.signal,
      });
    } catch (error) {
      if (options.signal?.aborted || error?.name === "AbortError") {
        throw new AdminApiError("请求已取消。", {
          kind: "aborted",
          code: "ADMIN_ABORTED",
        });
      }
      throw new AdminApiError("网络连接失败。", {
        kind: "network",
        code: "ADMIN_NETWORK_ERROR",
      });
    }

    const parsed = await parseResponse(response, options.responseType ?? "auto");
    if (!response.ok) {
      const error = normalizeHttpError(response.status, parsed, collectSecrets(options.body));
      if (response.status === 401 && !(url === "/api/login" && method === "POST")) {
        try {
          onUnauthorized(error);
        } catch {
          // Session cleanup is best-effort and must not hide the authoritative API error.
        }
      }
      throw error;
    }

    if (parsed.invalidJson) {
      throw new AdminApiError("服务器返回了无效数据。", {
        kind: "invalid_response",
        status: response.status,
        code: "ADMIN_INVALID_RESPONSE",
      });
    }
    return normalizeDownloadUrls(parsed.value);
  }

  const jsonMutation = (path, method, body, options = {}) => request(path, { ...options, method, body });
  const get = (path, query = {}, options = {}) => request(path, { ...options, query });
  const id = (value) => encodeURIComponent(String(value));

  return Object.freeze({
    request,
    restoreSession: (options) => request("/api/me", options),
    getMe: (options) => request("/api/me", options),
    login: (username, password, options) => jsonMutation("/api/login", "POST", { username, password }, options),
    logout: (options) => request("/api/logout", { ...options, method: "POST" }),
    changePassword: (newPassword, options) => jsonMutation(
      "/api/change-password",
      "POST",
      { newPassword },
      options,
    ),

    getAppVersions: (options) => request("/api/app-versions", options),
    getVersionSummary: (options) => request("/api/app-versions/summary", options),
    createAppVersion: (body, options) => jsonMutation("/api/app-versions", "POST", body, options),
    updateAppVersion: (versionId, body, options) => jsonMutation(
      `/api/app-versions/${id(versionId)}`,
      "PUT",
      body,
      options,
    ),
    deleteAppVersion: (versionId, options) => request(`/api/app-versions/${id(versionId)}`, {
      ...options,
      method: "DELETE",
    }),

    getUsers: (options) => request("/api/users", options),
    createUser: (body, options) => jsonMutation("/api/users", "POST", body, options),
    updateUser: (userId, body, options) => jsonMutation(`/api/users/${id(userId)}`, "PUT", body, options),
    deleteUser: (userId, options) => request(`/api/users/${id(userId)}`, { ...options, method: "DELETE" }),
    rotateUserToken: (userId, options) => request(`/api/users/${id(userId)}/rotate-token`, {
      ...options,
      method: "POST",
    }),

    getOnlineSessions: (options) => request("/api/online", options),
    kickSession: (body, options) => jsonMutation("/api/online/kick", "POST", body, options),

    getTraceOverview: (query = {}, options = {}) => get("/api/usage-logs/v2/overview", query, options),
    getTraceUsers: (query = {}, options = {}) => get("/api/usage-logs/v2/users", query, options),
    getTraceRuns: (query = {}, options = {}) => get("/api/usage-logs/v2/runs", query, options),
    getTraceRun: (runId, options) => request(`/api/usage-logs/v2/runs/${id(runId)}`, options),
    getTraceEvent: (runId, eventId, options) => request(
      `/api/usage-logs/v2/runs/${id(runId)}/events/${id(eventId)}`,
      options,
    ),
    getTraceOutput: (runId, eventId, query = {}, options = {}) => get(
      `/api/usage-logs/v2/runs/${id(runId)}/events/${id(eventId)}/output`,
      query,
      options,
    ),
    exportTrace: (query = {}, options = {}) => request("/api/usage-logs/v2/export", {
      ...options,
      query,
      responseType: "text",
    }),
    getRomLogs: (query = {}, options = {}) => get("/api/rom-logs/v2", query, options),
  });
}

function buildApiUrl(path, query) {
  if (typeof path !== "string" || !path.startsWith("/api") || path.startsWith("//")) {
    throw new TypeError("invalid API path");
  }
  const url = new URL(path, API_ROOT);
  if (url.origin !== API_ROOT || !(url.pathname === "/api" || url.pathname.startsWith("/api/")) || url.hash) {
    throw new TypeError("invalid API path");
  }
  assertSafeQuery(url.searchParams);

  if (query !== undefined && query !== null) {
    const values = query instanceof URLSearchParams ? query : Object.entries(query);
    for (const [key, rawValue] of values) {
      if (SENSITIVE_QUERY_KEY.test(key)) throw new TypeError("sensitive query key");
      if (rawValue === undefined || rawValue === null || rawValue === "") continue;
      if (Array.isArray(rawValue)) {
        for (const value of rawValue) url.searchParams.append(key, String(value));
      } else {
        url.searchParams.set(key, String(rawValue));
      }
    }
  }
  assertSafeQuery(url.searchParams);
  return `${url.pathname}${url.search}`;
}

function assertSafeQuery(searchParams) {
  for (const [key, value] of searchParams) {
    if (SENSITIVE_QUERY_KEY.test(key)) throw new TypeError("sensitive query key");
    if (containsSensitiveQueryText(value)) throw new TypeError("sensitive query value");
  }
}

function containsSensitiveQueryText(value) {
  if (typeof value !== "string") return false;
  const candidates = [value];
  try {
    const decoded = decodeURIComponent(value.replace(/\+/g, "%20"));
    if (decoded !== value) candidates.push(decoded);
  } catch {
    // Malformed percent text is handled as opaque input; the raw candidate is still checked.
  }
  return candidates.some((candidate) => SENSITIVE_QUERY_PATTERNS.some((pattern) => pattern.test(candidate)));
}

function buildHeaders(input) {
  const headers = new Headers(input);
  for (const name of headers.keys()) {
    if (SENSITIVE_HEADER.test(name)) throw new TypeError("sensitive header");
  }
  return headers;
}

async function parseResponse(response, responseType) {
  const text = await response.text();
  if (!text) return { value: null, invalidJson: false };

  const contentType = response.headers.get("Content-Type")?.toLowerCase() ?? "";
  const declaredJson = contentType.includes("application/json") || contentType.includes("+json");
  if (responseType === "text" && response.ok) return { value: text, invalidJson: false };
  if (declaredJson || responseType === "json") {
    try {
      return { value: JSON.parse(text), invalidJson: false };
    } catch {
      return { value: null, invalidJson: true };
    }
  }
  return { value: text, invalidJson: false };
}

function normalizeHttpError(status, parsed, secrets) {
  const defaults = status >= 500
    ? ["server", "ADMIN_SERVER_ERROR", "服务器暂时无法处理请求。"]
    : STATUS_DEFAULTS[status] ?? ["http", "ADMIN_HTTP_ERROR", "请求失败。"];
  const payload = parsed.invalidJson ? null : parsed.value;
  const v2 = isObject(payload?.error) ? payload.error : null;
  const legacyMessage = typeof payload?.error === "string" ? payload.error : null;
  const textMessage = typeof payload === "string" ? payload : null;
  const message = safeMessage(v2?.message ?? legacyMessage ?? textMessage, secrets, defaults[2]);
  const details = Array.isArray(v2?.details)
    ? Object.freeze(v2.details.map((detail) => normalizeDetail(detail, secrets)))
    : null;
  const update = status === 426 && isObject(payload)
    ? Object.freeze({
      latest: safeScalar(payload.latest),
      min: safeScalar(payload.min),
      download_url: safeDownloadUrl(payload.download_url),
    })
    : null;

  return new AdminApiError(message, {
    kind: defaults[0],
    status,
    code: safeCode(v2?.code ?? payload?.code) ?? defaults[1],
    requestId: safeRequestId(v2?.request_id),
    details,
    update,
  });
}

function normalizeDetail(detail, secrets) {
  if (!isObject(detail)) {
    return Object.freeze({ entity: null, id: null, code: null, message: "请求项无效。" });
  }
  return Object.freeze({
    entity: safeScalar(detail.entity),
    id: safeScalar(detail.id),
    code: safeCode(detail.code),
    message: safeMessage(detail.message, secrets, "请求项无效。"),
  });
}

function invalidRequest() {
  return new AdminApiError("请求地址或选项无效。", {
    kind: "invalid_request",
    code: "ADMIN_INVALID_REQUEST",
  });
}

function safeMessage(value, secrets, fallback) {
  if (typeof value !== "string") return fallback;
  let message = value.replace(CONTROL_CHARACTERS, " ").replace(/\s+/g, " ").trim();
  if (!message || /<\s*(?:!doctype|html|script|style)\b/i.test(message)) return fallback;
  for (const secret of secrets) {
    if (!secret) continue;
    message = message.split(secret).join("[REDACTED]");
  }
  message = message.replace(SECRET_ASSIGNMENT, (_match, label, separator) => `${label}${separator}[REDACTED]`);
  message = message.replace(OPAQUE_SECRET, "[REDACTED]");
  return message.slice(0, 240) || fallback;
}

function collectSecrets(body) {
  const secrets = [];
  collectSensitiveValues(body, null, secrets);
  return secrets.sort((left, right) => right.length - left.length);
}

function collectSensitiveValues(value, key, target) {
  if (SENSITIVE_QUERY_KEY.test(String(key ?? "")) && (typeof value === "string" || typeof value === "number")) {
    target.push(String(value));
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) collectSensitiveValues(item, key, target);
    return;
  }
  if (isObject(value)) {
    for (const [childKey, childValue] of Object.entries(value)) {
      collectSensitiveValues(childValue, childKey, target);
    }
  }
}

function normalizeDownloadUrls(value) {
  if (Array.isArray(value)) return value.map(normalizeDownloadUrls);
  if (!isObject(value)) return value;
  const normalized = {};
  for (const [key, child] of Object.entries(value)) {
    if (key === "__proto__" || key === "constructor" || key === "prototype") continue;
    normalized[key] = key === "download_url" ? safeDownloadUrl(child) : normalizeDownloadUrls(child);
  }
  return normalized;
}

function safeDownloadUrl(value) {
  if (typeof value !== "string" || !value.trim()) return null;
  try {
    const url = new URL(value.trim());
    if ((url.protocol !== "http:" && url.protocol !== "https:") || url.username || url.password) return null;
    return url.href;
  } catch {
    return null;
  }
}

function safeCode(value) {
  return typeof value === "string" && /^[A-Za-z][A-Za-z0-9_]{1,63}$/.test(value) ? value : null;
}

function safeRequestId(value) {
  if (typeof value !== "string") return null;
  const requestId = value.trim();
  return requestId && requestId.length <= 128 && /^[A-Za-z0-9._:-]+$/.test(requestId) ? requestId : null;
}

function safeScalar(value) {
  return typeof value === "string" || typeof value === "number" ? value : null;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
