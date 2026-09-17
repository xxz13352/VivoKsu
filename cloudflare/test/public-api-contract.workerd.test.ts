import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import apiWorker, { type Env as WorkerEnv } from "../src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const API_TOKEN = "public-contract-token";
const VERSION = "1.4.0";
const PASSWORD_HASH = "0".repeat(64);
const SALT = "00112233445566778899aabbccddeeff";

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await env.DB.prepare(
    `INSERT INTO api_users (id, username, name, token, password, salt, enabled, banned)
     VALUES (7, 'alice', 'Alice', ?, ?, ?, 1, 0)`,
  ).bind(API_TOKEN, PASSWORD_HASH, SALT).run();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("public API method and cache contract", () => {
  it.each([
    ["/health", "POST", "GET, OPTIONS"],
    ["/api/app/version", "POST", "GET, OPTIONS"],
    ["/api/security/pins", "POST", "GET, OPTIONS"],
    ["/api/integrity/report", "GET", "POST, OPTIONS"],
    ["/api/diagnostics/crash", "GET", "POST, OPTIONS"],
    ["/api/login", "GET", "POST, OPTIONS"],
    ["/api/me", "POST", "GET, OPTIONS"],
    ["/api/heartbeat", "GET", "POST, OPTIONS"],
    ["/api/online", "POST", "GET, OPTIONS"],
    ["/api/operation/authorize", "GET", "POST, OPTIONS"],
    ["/api/usage/logs", "GET", "POST, OPTIONS"],
    ["/api/usage/traces/v2", "GET", "POST, OPTIONS"],
    ["/api/rom?pd=PD&version=1.0.0", "POST", "GET, OPTIONS"],
  ] as const)("rejects %s %s with a method-closed 405", async (path, method, allow) => {
    const response = await request(path, { method });
    expect(response.status).toBe(405);
    expect(response.headers.get("allow")).toBe(allow);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: "方法不允许。" });
  });

  it("keeps OPTIONS as an un-gated CORS preflight for known and unknown paths", async () => {
    for (const path of ["/health", "/api/me", "/api/rom", "/not-a-route"]) {
      const response = await request(path, { method: "OPTIONS" });
      expect(response.status, path).toBe(204);
      expect(response.headers.get("access-control-allow-origin"), path).toBe("*");
      expect(response.headers.get("access-control-allow-methods"), path).toBe("GET,POST,OPTIONS");
    }
  });

  it("marks personalized and policy JSON responses no-store", async () => {
    const responses = [
      await request("/health"),
      await request("/api/app/version"),
      await request("/api/me", { headers: { "X-Nwflash-Version": VERSION } }),
      await request("/api/online", { headers: { Authorization: `Bearer ${API_TOKEN}` } }),
    ];
    for (const response of responses) expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

describe("public ROM API authority and upstream contract", () => {
  it("authenticates before exposing missing parameters or server configuration", async () => {
    const missingParameters = await request("/api/rom", {
      headers: { "X-Nwflash-Version": VERSION },
    });
    expect(missingParameters.status).toBe(401);
    expect(await missingParameters.json()).toEqual({ error: "请先登录。" });

    const noVotaEnv: WorkerEnv = { DB: env.DB, VOTA_API_TOKEN: "" };
    const missingConfiguration = await apiWorker.fetch(
      new Request("https://api.nwflash.cc.cd/api/rom?pd=PD&version=1.0.0", {
        headers: { "X-Nwflash-Version": VERSION },
      }),
      noVotaEnv,
    );
    expect(missingConfiguration.status).toBe(401);
    expect(await missingConfiguration.json()).toEqual({ error: "请先登录。" });

    expect(await scalar("SELECT COUNT(*) AS n FROM access_logs")).toBe(0);
  });

  it("returns 400 for missing parameters after successful authentication", async () => {
    const response = await request("/api/rom", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: "缺少 pd 或 version 查询参数。" });
    expect(await scalar("SELECT COUNT(*) AS n FROM access_logs")).toBe(0);
  });

  it.each([
    "/api/rom?pd=%20&version=1.0.0",
    "/api/rom?pd=PD&version=%09",
  ])("rejects blank ROM query parameters after authentication (%s)", async (path) => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    const response = await request(path, {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: "缺少 pd 或 version 查询参数。" });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it.each([
    undefined,
    "",
    "relative/rom.zip",
    "file:///C:/rom.zip",
  ] as const)("requires a usable HTTP(S) URL for an upstream success (%s)", async (url) => {
    const fetchSpy = vi.fn(async () => new Response(JSON.stringify({ ok: true, url }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }));
    vi.stubGlobal("fetch", fetchSpy);

    const response = await request("/api/rom?pd=PD&version=1.0.0", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({ error: "上游返回异常。" });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(await scalar("SELECT status AS n FROM access_logs")).toBe(502);
  });

  it("maps the upstream UPDATE_REQUIRED code to HTTP 426", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ok: false,
      code: "UPDATE_REQUIRED",
      error: "客户端版本过旧。",
    }), { status: 200, headers: { "Content-Type": "application/json" } })));

    const response = await request("/api/rom?pd=PD&version=1.0.0", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(426);
    expect(await response.json()).toEqual({ error: "客户端版本过旧。" });
    expect(await scalar("SELECT status AS n FROM access_logs")).toBe(426);
  });

  it("maps the upstream NOT_FOUND code to HTTP 404 with an actionable message", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ok: false,
      code: "NOT_FOUND",
      error: "ROM not found",
    }), { status: 200, headers: { "Content-Type": "application/json" } })));

    const response = await request("/api/rom?pd=PD&version=1.0.0", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({
      error: "未找到该固件包（该机型/版本可能未上架或不受支持）。",
    });
    expect(await scalar("SELECT status AS n FROM access_logs")).toBe(404);
  });

  it("maps an unrecognized upstream failure code to HTTP 502 as a server fault", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ok: false,
      code: "UPSTREAM_BUSY",
      error: "upstream overloaded",
    }), { status: 200, headers: { "Content-Type": "application/json" } })));

    const response = await request("/api/rom?pd=PD&version=1.0.0", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({
      error: "上游 ROM 服务返回错误：upstream overloaded",
    });
    expect(await scalar("SELECT status AS n FROM access_logs")).toBe(502);
  });

  it("accepts a valid upstream success, forwards only server credentials, and logs the URL", async () => {
    const fetchSpy = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      expect(String(input)).toContain("action=resolve_url");
      expect(init?.method).toBe("POST");
      expect(new Headers(init?.headers).get("Authorization")).toBe(`Bearer ${env.VOTA_API_TOKEN}`);
      expect(JSON.parse(String(init?.body))).toEqual({ ver: "0.1.0", pd: "PD", version: "1.0.0" });
      return new Response(JSON.stringify({
        ok: true,
        url: "https://download.example.test/rom.zip?sig=opaque",
        name: "rom.zip",
        sizeBytes: 42,
        sha256: "ignored-by-contract",
      }), { status: 200, headers: { "Content-Type": "application/json" } });
    });
    vi.stubGlobal("fetch", fetchSpy);

    const response = await request("/api/rom?pd=PD&version=1.0.0", {
      headers: {
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      pd: "PD",
      version: "1.0.0",
      url: "https://download.example.test/rom.zip?sig=opaque",
      name: "rom.zip",
      sizeBytes: 42,
      sha256: "ignored-by-contract",
    });
    const log = await env.DB.prepare(
      "SELECT api_user_id, api_user_name, pd, version, url, status FROM access_logs",
    ).first<Record<string, unknown>>();
    expect(log).toEqual({
      api_user_id: 7,
      api_user_name: "Alice",
      pd: "PD",
      version: "1.0.0",
      url: "https://download.example.test/rom.zip?sig=opaque",
      status: 200,
    });
  });
});

describe("public login and legacy usage payload errors", () => {
  it("distinguishes an absent bearer from a malformed bearer on /api/me", async () => {
    const anonymous = await request("/api/me", {
      headers: { "X-Nwflash-Version": VERSION },
    });
    expect(anonymous.status).toBe(200);
    expect(await anonymous.json()).toEqual({ loggedIn: false });

    for (const authorization of ["Basic not-a-bearer", "Bearer", "Bearer "]) {
      const response = await request("/api/me", {
        headers: {
          "X-Nwflash-Version": VERSION,
          Authorization: authorization,
        },
      });
      expect(response.status, authorization).toBe(401);
      expect(await response.json(), authorization).toEqual({ error: "API token 无效或已停用。" });
    }
  });

  it.each([
    ["username", { username: 7 }],
    ["password", { password: { value: "not-a-string" } }],
  ] as const)("returns 400 for non-string %s instead of an internal error", async (_field, override) => {
    const body = {
      username: "alice",
      password: "correct horse",
      client_version: VERSION,
      build_id: "build",
      process_nonce: "nonce",
      session_id: "session",
      ...override,
    };
    const response = await request("/api/login", {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Nwflash-Version": VERSION },
      body: JSON.stringify(body),
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: "缺少用户名或密码。" });
  });

  it("returns 400 for malformed usage JSON instead of acknowledging a dropped batch", async () => {
    const response = await request("/api/usage/logs", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Nwflash-Version": VERSION,
        Authorization: `Bearer ${API_TOKEN}`,
      },
      body: "{not-json",
    });
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: "日志请求体不合法。" });
    expect(await scalar("SELECT COUNT(*) AS n FROM usage_logs")).toBe(0);
  });
});

async function request(path: string, init: RequestInit = {}): Promise<Response> {
  return exports.default.fetch(new Request(`https://api.nwflash.cc.cd${path}`, init), env);
}

async function scalar(sql: string): Promise<number> {
  const row = await env.DB.prepare(sql).first<{ n: number }>();
  return Number(row?.n ?? 0);
}
