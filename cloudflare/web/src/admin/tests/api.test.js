import { describe, expect, it, vi } from "vitest";

import { AdminApiError, createApiClient } from "../api.js";

function jsonResponse(body, init = {}) {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    headers: { "Content-Type": "application/json; charset=utf-8", ...init.headers },
  });
}

describe("admin API client", () => {
  it("uses same-origin cookies and adds the CSRF header only to mutations", async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse({ loggedIn: true, username: "admin" }))
      .mockResolvedValueOnce(jsonResponse({ ok: true }));
    const api = createApiClient({ fetchImpl });

    await api.getMe();
    await api.changePassword("new-password");

    expect(fetchImpl).toHaveBeenNthCalledWith(1, "/api/me", expect.objectContaining({
      method: "GET",
      credentials: "same-origin",
    }));
    expect(fetchImpl.mock.calls[0][1].headers.has("X-Requested-With")).toBe(false);

    const [mutationUrl, mutationInit] = fetchImpl.mock.calls[1];
    expect(mutationUrl).toBe("/api/change-password");
    expect(mutationUrl).not.toContain("new-password");
    expect(mutationInit).toMatchObject({
      method: "POST",
      credentials: "same-origin",
      body: JSON.stringify({ newPassword: "new-password" }),
    });
    expect(mutationInit.headers.get("X-Requested-With")).toBe("XMLHttpRequest");
    expect(mutationInit.headers.get("Content-Type")).toBe("application/json");
  });

  it("normalizes a legacy 401 and invokes the centralized unauthorized callback", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({ error: "会话已过期" }, { status: 401 })),
      onUnauthorized,
    });

    const error = await api.getTraceOverview().catch((value) => value);

    expect(error).toBeInstanceOf(AdminApiError);
    expect(error).toMatchObject({
      kind: "unauthorized",
      status: 401,
      code: "ADMIN_UNAUTHORIZED",
      message: "会话已过期",
      requestId: null,
    });
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
    expect(onUnauthorized).toHaveBeenCalledWith(error);
  });

  it("does not let protected request options suppress centralized 401 cleanup", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({ error: "会话已过期" }, { status: 401 })),
      onUnauthorized,
    });

    await expect(api.getTraceRuns({}, { notifyUnauthorized: false })).rejects.toMatchObject({ status: 401 });
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
  });

  it("only suppresses centralized cleanup for POST /api/login", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockImplementation(() => Promise.resolve(
        jsonResponse({ error: "未登录" }, { status: 401 }),
      )),
      onUnauthorized,
    });

    await expect(api.request("/api/login", { method: "GET" })).rejects.toMatchObject({ status: 401 });
    await expect(api.request("/api/login", { method: "PUT" })).rejects.toMatchObject({ status: 401 });
    expect(onUnauthorized).toHaveBeenCalledTimes(2);
  });

  it("keeps a login 401 local to the form instead of invalidating an existing shell", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({ error: "用户名或密码错误。" }, { status: 401 })),
      onUnauthorized,
    });

    await expect(api.login("operator", "wrong-password")).rejects.toMatchObject({
      kind: "unauthorized",
      status: 401,
      message: "用户名或密码错误。",
    });
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it("normalizes the frozen V2 envelope without treating 403 as logout", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({
        ok: false,
        error: {
          code: "TRACE_FORBIDDEN",
          message: "禁止访问",
          request_id: "ray-403",
          details: [{ entity: "run", id: "run-1", code: "invalid", message: "bad" }],
        },
      }, { status: 403 })),
      onUnauthorized,
    });

    const error = await api.getTraceRuns().catch((value) => value);

    expect(error).toMatchObject({
      kind: "forbidden",
      status: 403,
      code: "TRACE_FORBIDDEN",
      requestId: "ray-403",
      details: [{ entity: "run", id: "run-1", code: "invalid", message: "bad" }],
    });
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it("classifies 426 separately and nulls a non-HTTP download URL", async () => {
    const onUnauthorized = vi.fn();
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({
        error: "需要更新",
        code: "UPDATE_REQUIRED",
        latest: "3.1.0",
        min: "3.0.0",
        download_url: "javascript:alert(1)",
      }, { status: 426 })),
      onUnauthorized,
    });

    const error = await api.request("/api/app-versions/summary").catch((value) => value);

    expect(error).toMatchObject({
      kind: "update_required",
      status: 426,
      code: "UPDATE_REQUIRED",
      update: { latest: "3.1.0", min: "3.0.0", download_url: null },
    });
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it("sanitizes download_url fields in successful JSON responses", async () => {
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({
        versions: [
          { version: "3.1.0", download_url: "https://download.example/app.zip" },
          { version: "3.0.0", download_url: "data:text/html,payload" },
          { version: "2.9.0", download_url: "/relative/app.zip" },
        ],
      })),
    });

    await expect(api.getAppVersions()).resolves.toEqual({
      versions: [
        { version: "3.1.0", download_url: "https://download.example/app.zip" },
        { version: "3.0.0", download_url: null },
        { version: "2.9.0", download_url: null },
      ],
    });
  });

  it("parses explicit text responses without assuming JSON", async () => {
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(new Response("run_id,event_id\n1,2", {
        headers: { "Content-Type": "text/csv; charset=utf-8" },
      })),
    });

    await expect(api.request("/api/usage-logs/v2/export", { responseType: "text" }))
      .resolves.toBe("run_id,event_id\n1,2");
  });

  it("still normalizes a frozen JSON error from a text export request", async () => {
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(jsonResponse({
        ok: false,
        error: { code: "TRACE_FORBIDDEN", message: "禁止导出", request_id: "export-403" },
      }, { status: 403 })),
    });

    await expect(api.exportTrace()).rejects.toMatchObject({
      kind: "forbidden",
      status: 403,
      code: "TRACE_FORBIDDEN",
      requestId: "export-403",
      message: "禁止导出",
    });
  });

  it("classifies malformed declared JSON as an invalid response", async () => {
    const api = createApiClient({
      fetchImpl: vi.fn().mockResolvedValue(new Response("not-json", {
        headers: { "Content-Type": "application/json" },
      })),
    });

    await expect(api.getMe()).rejects.toMatchObject({
      kind: "invalid_response",
      status: 200,
      code: "ADMIN_INVALID_RESPONSE",
    });
  });

  it("classifies a text 5xx while redacting submitted secrets from the error message", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(new Response(
      "upstream failed for password super-secret and token abcdef123456",
      { status: 503, headers: { "Content-Type": "text/plain" } },
    ));
    const api = createApiClient({ fetchImpl });

    const error = await api.login("admin", "super-secret").catch((value) => value);

    expect(error).toMatchObject({ kind: "server", status: 503, code: "ADMIN_SERVER_ERROR" });
    expect(error.message).not.toContain("super-secret");
    expect(error.message).not.toContain("abcdef123456");
    expect(error.message).toContain("[REDACTED]");
    expect(fetchImpl.mock.calls[0][0]).toBe("/api/login");
  });

  it("classifies network failures without copying the transport error message", async () => {
    const api = createApiClient({
      fetchImpl: vi.fn().mockRejectedValue(new Error("connect failed with token private-value")),
    });

    const error = await api.getMe().catch((value) => value);

    expect(error).toMatchObject({
      kind: "network",
      status: 0,
      code: "ADMIN_NETWORK_ERROR",
      message: "网络连接失败。",
    });
    expect(error.message).not.toContain("private-value");
  });

  it("classifies an AbortError and forwards the AbortSignal", async () => {
    const fetchImpl = vi.fn((_url, init) => {
      expect(init.signal).toBe(signal);
      return Promise.reject(new DOMException("cancelled", "AbortError"));
    });
    const signal = new AbortController().signal;
    const api = createApiClient({ fetchImpl });

    await expect(api.getTraceOverview({}, { signal })).rejects.toMatchObject({
      kind: "aborted",
      status: 0,
      code: "ADMIN_ABORTED",
      message: "请求已取消。",
    });
  });

  it("provides the authentication lifecycle helpers with fixed paths and bodies", async () => {
    const fetchImpl = vi.fn().mockImplementation(() => Promise.resolve(jsonResponse({ ok: true })));
    const api = createApiClient({ fetchImpl });

    await api.restoreSession();
    await api.login("operator", "secret-value");
    await api.logout();
    await api.changePassword("replacement-value");

    expect(fetchImpl.mock.calls.map(([url]) => url)).toEqual([
      "/api/me",
      "/api/login",
      "/api/logout",
      "/api/change-password",
    ]);
    expect(JSON.parse(fetchImpl.mock.calls[1][1].body)).toEqual({
      username: "operator",
      password: "secret-value",
    });
    expect(JSON.parse(fetchImpl.mock.calls[3][1].body)).toEqual({
      newPassword: "replacement-value",
    });
  });

  it("rejects cross-origin paths and sensitive query keys before fetch", async () => {
    const fetchImpl = vi.fn();
    const api = createApiClient({ fetchImpl });

    await expect(api.request("https://evil.example/api/me")).rejects.toMatchObject({
      kind: "invalid_request",
      code: "ADMIN_INVALID_REQUEST",
    });
    await expect(api.request("/api/users", { query: { token: "secret-value" } }))
      .rejects.toMatchObject({ kind: "invalid_request", code: "ADMIN_INVALID_REQUEST" });
    for (const q of [
      "Authorization: Bearer top-secret",
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
      "stdout=private-output",
      "command: fastboot flash super secret.img",
    ]) {
      await expect(api.request("/api/usage-logs/v2/runs", { query: { q } }))
        .rejects.toMatchObject({ kind: "invalid_request", code: "ADMIN_INVALID_REQUEST" });
    }
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("does not allow callers to add the mutation-only CSRF header to GET", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ loggedIn: true }));
    const api = createApiClient({ fetchImpl });

    await api.request("/api/me", { headers: { "X-Requested-With": "XMLHttpRequest" } });

    expect(fetchImpl.mock.calls[0][1].headers.has("X-Requested-With")).toBe(false);
  });

  it("classifies an unserializable body before invoking fetch", async () => {
    const fetchImpl = vi.fn();
    const api = createApiClient({ fetchImpl });
    const body = {};
    body.self = body;

    await expect(api.request("/api/users", { method: "POST", body })).rejects.toMatchObject({
      kind: "invalid_request",
      code: "ADMIN_INVALID_REQUEST",
    });
    expect(fetchImpl).not.toHaveBeenCalled();
  });
});
