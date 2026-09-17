import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

import adminWorker, { type Env as AdminEnv } from "../web/src/index";
import adminIndexHtml from "../web/src/admin/index.html";
import adminStylesCss from "../web/src/admin/styles.css";
import adminApiJs from "../web/src/admin/api.js";
import adminAppJs from "../web/src/admin/app.js";
import adminComponentsJs from "../web/src/admin/components.js";
import adminFormatTimeJs from "../web/src/admin/format-time.js";
import adminRouterJs from "../web/src/admin/router.js";
import adminAuditJs from "../web/src/admin/pages/audit.js";
import adminOverviewJs from "../web/src/admin/pages/overview.js";
import adminRomJs from "../web/src/admin/pages/rom.js";
import adminSessionsJs from "../web/src/admin/pages/sessions.js";
import adminUsersJs from "../web/src/admin/pages/users.js";
import adminVersionsJs from "../web/src/admin/pages/versions.js";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends AdminEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const ORIGIN = "https://web.nwflash.cc.cd";
const CRUD_SESSION = "crud-authority-session";
const STRICT_CSP = "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; "
  + "object-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
  + "font-src 'self'; connect-src 'self'";

const STATIC_MANIFEST = Object.freeze([
  ["/", "text/html; charset=utf-8", adminIndexHtml],
  ["/admin/styles.css", "text/css; charset=utf-8", adminStylesCss],
  ["/admin/app.js", "text/javascript; charset=utf-8", adminAppJs],
  ["/admin/api.js", "text/javascript; charset=utf-8", adminApiJs],
  ["/admin/router.js", "text/javascript; charset=utf-8", adminRouterJs],
  ["/admin/components.js", "text/javascript; charset=utf-8", adminComponentsJs],
  ["/admin/format-time.js", "text/javascript; charset=utf-8", adminFormatTimeJs],
  ["/admin/pages/audit.js", "text/javascript; charset=utf-8", adminAuditJs],
  ["/admin/pages/overview.js", "text/javascript; charset=utf-8", adminOverviewJs],
  ["/admin/pages/versions.js", "text/javascript; charset=utf-8", adminVersionsJs],
  ["/admin/pages/users.js", "text/javascript; charset=utf-8", adminUsersJs],
  ["/admin/pages/sessions.js", "text/javascript; charset=utf-8", adminSessionsJs],
  ["/admin/pages/rom.js", "text/javascript; charset=utf-8", adminRomJs],
] as const);

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

describe("administrator static module manifest", () => {
  it("serves exactly thirteen explicit GET assets before touching seed D1", async () => {
    const poison = poisonAdminEnv();
    expect(STATIC_MANIFEST).toHaveLength(13);

    for (const [path, contentType, expectedBody] of STATIC_MANIFEST) {
      const response = await adminWorker.fetch(new Request(`${ORIGIN}${path}`), poison.env);
      const body = await response.text();

      expect(response.status, path).toBe(200);
      expect(response.headers.get("content-type"), path).toBe(contentType);
      expect(response.headers.get("cache-control"), path).toBe("no-store");
      expect(body, path).toBe(expectedBody);
      expectStrictSecurityHeaders(response, path);
    }
    expect(poison.accesses()).toBe(0);
  });

  it("serves the modular root without legacy inline script or unsafe CSP", async () => {
    const poison = poisonAdminEnv();
    const response = await adminWorker.fetch(new Request(`${ORIGIN}/`), poison.env);
    const body = await response.text();

    expect(response.status).toBe(200);
    expect(body).toContain('<script type="module" src="/admin/app.js"></script>');
    expect(body).not.toMatch(/<script(?![^>]*\bsrc=)[^>]*>/i);
    expect(body).not.toContain("async function api(");
    expect(body.length).toBeLessThan(8_192);
    expect(response.headers.get("content-security-policy")).toBe(STRICT_CSP);
    expect(response.headers.get("content-security-policy")).not.toContain("unsafe-inline");
    expect(poison.accesses()).toBe(0);
  });

  it.each([
    ["unknown asset", "GET", "/admin/missing.js", 404],
    ["directory alias", "GET", "/admin/", 404],
    ["source filename alias", "GET", "/admin/index.html", 404],
    ["legacy monolith", "GET", "/admin.html", 404],
    ["test source", "GET", "/admin/tests/api.test.js", 404],
    ["case variant", "GET", "/admin/App.js", 404],
    ["trailing slash", "GET", "/admin/app.js/", 404],
    ["encoded namespace", "GET", "/%61dmin/app.js", 404],
    ["encoded filename", "GET", "/admin/%61pp.js", 404],
    ["encoded page filename", "GET", "/admin/pages/%61udit.js", 404],
    ["encoded dot", "GET", "/admin/app%2Ejs", 404],
    ["encoded slash", "GET", "/admin/pages%2Faudit.js", 404],
    ["encoded forward slash segment", "GET", "/admin/pages/%2faudit.js", 404],
    ["encoded backslash segment", "GET", "/admin/pages/%5caudit.js", 404],
    ["double slash", "GET", "/admin//app.js", 404],
    ["POST root", "POST", "/", 405],
    ["PUT asset", "PUT", "/admin/app.js", 405],
    ["DELETE asset", "DELETE", "/admin/styles.css", 405],
    ["HEAD asset", "HEAD", "/admin/app.js", 405],
    ["OPTIONS asset", "OPTIONS", "/admin/styles.css", 405],
  ])("rejects %s without dynamic path lookup or D1", async (_label, method, path, status) => {
    const poison = poisonAdminEnv();
    const response = await adminWorker.fetch(new Request(`${ORIGIN}${path}`, { method }), poison.env);

    expect(response.status).toBe(status);
    expect(response.headers.get("content-type")).toBe("application/json; charset=utf-8");
    expectStrictSecurityHeaders(response, `${method} ${path}`);
    expect(poison.accesses()).toBe(0);
  });

  it("serves a query variant from the same closed asset without changing body or cache policy", async () => {
    const poison = poisonAdminEnv();
    const response = await adminWorker.fetch(new Request(`${ORIGIN}/admin/app.js?v=task11`), poison.env);

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("text/javascript; charset=utf-8");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.text()).toBe(adminAppJs);
    expect(poison.accesses()).toBe(0);
  });
});

describe("administrator response security headers", () => {
  it("uses one strict policy for ordinary API, V2, static 404, and both 500 families", async () => {
    const poisonOrdinary = poisonAdminEnv({ seed: false });
    const poisonV2 = poisonAdminEnv({ seed: false });
    const responses = [
      ["ordinary success", await adminWorker.fetch(new Request(`${ORIGIN}/api/me`), env), 200],
      ["ordinary 404", await adminWorker.fetch(new Request(`${ORIGIN}/not-found`), env), 404],
      ["V2 unauthorized", await adminWorker.fetch(new Request(`${ORIGIN}/api/usage-logs/v2/runs`), env), 401],
      ["ordinary 500", await adminWorker.fetch(poisonApiRequest("/api/users"), poisonOrdinary.env), 500],
      ["V2 500", await adminWorker.fetch(poisonApiRequest("/api/usage-logs/v2/runs"), poisonV2.env), 500],
    ] as const;

    for (const [label, response, status] of responses) {
      expect(response.status, label).toBe(status);
      expect(response.headers.get("content-type"), label).toBe("application/json; charset=utf-8");
      expectStrictSecurityHeaders(response, label);
    }
  });

  it("keeps strict headers on authenticated V2 JSON, streamed NDJSON, logout cookies, and HTTPS redirect", async () => {
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO admins (id, username, salt, password_hash) VALUES (31, 'static-reviewer', 'unused', 'unused')",
      ),
      env.DB.prepare(
        "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (31, 'static-session', '2999-01-01T00:00:00.000Z')",
      ),
    ]);
    const authenticated = { Cookie: "nwflash_session=static-session" };
    const v2 = await adminWorker.fetch(new Request(`${ORIGIN}/api/usage-logs/v2/runs`, {
      headers: authenticated,
    }), env);
    const ndjson = await adminWorker.fetch(new Request(`${ORIGIN}/api/usage-logs/v2/export`, {
      headers: authenticated,
    }), env);
    const logout = await adminWorker.fetch(new Request(`${ORIGIN}/api/logout`, {
      method: "POST",
      headers: { "X-Requested-With": "XMLHttpRequest" },
    }), env);
    const poison = poisonAdminEnv();
    const redirect = await adminWorker.fetch(new Request(`http://web.nwflash.cc.cd/admin/app.js`, {
      headers: { "x-forwarded-proto": "http" },
    }), poison.env);

    expect(v2.status).toBe(200);
    expect(v2.headers.get("content-type")).toBe("application/json; charset=utf-8");
    expectStrictSecurityHeaders(v2, "V2 success");
    expect(ndjson.status).toBe(200);
    expect(ndjson.headers.get("content-type")).toBe("application/x-ndjson; charset=utf-8");
    expect(ndjson.headers.get("content-disposition"))
      .toMatch(/^attachment; filename="nwflash-traces-[0-9]+\.ndjson"$/);
    expectStrictSecurityHeaders(ndjson, "NDJSON success");
    expect(new TextDecoder().decode(await ndjson.arrayBuffer())).toBe("");
    expect(logout.headers.get("set-cookie")).toContain("nwflash_session=;");
    expectStrictSecurityHeaders(logout, "logout");
    expect(redirect.status).toBe(301);
    expect(redirect.headers.get("location")).toBe("https://web.nwflash.cc.cd/admin/app.js");
    expectStrictSecurityHeaders(redirect, "HTTPS redirect");
    expect(poison.accesses()).toBe(0);
  });

  it("requires the mutation CSRF header before deleting an administrator session", async () => {
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO admins (id, username, salt, password_hash) VALUES (32, 'csrf-reviewer', 'unused', 'unused')",
      ),
      env.DB.prepare(
        "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (32, 'csrf-session', '2999-01-01T00:00:00.000Z')",
      ),
    ]);
    const cookie = { Cookie: "nwflash_session=csrf-session" };

    const rejected = await adminWorker.fetch(new Request(`${ORIGIN}/api/logout`, {
      method: "POST",
      headers: cookie,
    }), env);
    expect(rejected.status).toBe(403);
    expect(Number((await env.DB.prepare(
      "SELECT COUNT(*) AS value FROM admin_sessions WHERE token = 'csrf-session'",
    ).first<{ value: number }>())?.value ?? 0)).toBe(1);

    const accepted = await adminWorker.fetch(new Request(`${ORIGIN}/api/logout`, {
      method: "POST",
      headers: { ...cookie, "X-Requested-With": "XMLHttpRequest" },
    }), env);
    expect(accepted.status).toBe(200);
    expect(accepted.headers.get("set-cookie")).toContain("nwflash_session=;");
    expect(Number((await env.DB.prepare(
      "SELECT COUNT(*) AS value FROM admin_sessions WHERE token = 'csrf-session'",
    ).first<{ value: number }>())?.value ?? 0)).toBe(0);
  });
});

describe("administrator CRUD authority", () => {
  beforeEach(async () => {
    await env.DB.batch([
      env.DB.prepare(
        "INSERT INTO admins (id, username, salt, password_hash) VALUES (41, 'crud-reviewer', 'unused', 'unused')",
      ),
      env.DB.prepare(
        "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (41, ?, '2999-01-01T00:00:00.000Z')",
      ).bind(CRUD_SESSION),
      env.DB.prepare(
        "INSERT INTO app_versions (id, version, min_version, download_url, note, enabled) VALUES (9, '2.0.0', '1.0.0', '', 'stable', 1)",
      ),
      env.DB.prepare(
        "INSERT INTO api_users (id, username, name, token, password, salt, note) VALUES (7, 'alice', 'Alice', 'old-token', 'old-password', 'old-salt', 'stable')",
      ),
    ]);
  });

  it("rejects invalid ids and extra path segments without touching a target", async () => {
    const invalidIds = ["0", "00", "01", "0007", "-1", "1.5", "1e2", "9007199254740992", "not-id"];
    for (const id of invalidIds) {
      const version = await adminWrite(`/api/app-versions/${id}`, "PUT", { enabled: false });
      const versionDelete = await adminWrite(`/api/app-versions/${id}`, "DELETE");
      const user = await adminWrite(`/api/users/${id}`, "PUT", { enabled: false });
      const userDelete = await adminWrite(`/api/users/${id}`, "DELETE");
      const rotate = await adminWrite(`/api/users/${id}/rotate-token`, "POST");
      expect(version.status, `version ${id}`).toBe(400);
      expect(versionDelete.status, `version delete ${id}`).toBe(400);
      expect(user.status, `user ${id}`).toBe(400);
      expect(userDelete.status, `user delete ${id}`).toBe(400);
      expect(rotate.status, `rotate ${id}`).toBe(400);
    }

    for (const [path, method] of [
      ["/api/app-versions/9/extra", "PUT"],
      ["/api/app-versions/9/extra", "DELETE"],
      ["/api/users/7/extra", "PUT"],
      ["/api/users/7/extra", "DELETE"],
      ["/api/users/7/rotate-token/extra", "POST"],
      ["/api/users/7/extra/rotate-token", "POST"],
    ] as const) {
      const response = await adminWrite(path, method, { enabled: false });
      expect(response.status, `${method} ${path}`).toBe(404);
    }

    expect(await scalar("SELECT enabled AS value FROM app_versions WHERE id = 9")).toBe(1);
    expect(await scalar("SELECT enabled AS value FROM api_users WHERE id = 7")).toBe(1);
  });

  it("rejects empty or invalid version updates atomically", async () => {
    for (const body of [{}, { unknown: true }, { enabled: "false" }, { min_version: "" }]) {
      const response = await adminWrite("/api/app-versions/9", "PUT", body);
      expect(response.status).toBe(400);
    }

    const mixed = await adminWrite("/api/app-versions/9", "PUT", {
      note: "must-not-stick",
      min_version: "   ",
    });
    expect(mixed.status).toBe(400);
    expect(await textScalar("SELECT note AS value FROM app_versions WHERE id = 9")).toBe("stable");
    expect(await textScalar("SELECT min_version AS value FROM app_versions WHERE id = 9")).toBe("1.0.0");
  });

  it("rejects empty, mistyped, or short-password user updates atomically", async () => {
    for (const body of [{}, { unknown: true }, { banned: 1 }, { newPassword: "short" }]) {
      const response = await adminWrite("/api/users/7", "PUT", body);
      expect(response.status).toBe(400);
    }

    const mixed = await adminWrite("/api/users/7", "PUT", {
      note: "must-not-stick",
      enabled: false,
      newPassword: "short",
    });
    expect(mixed.status).toBe(400);
    const user = await env.DB.prepare(
      "SELECT note, enabled, password, salt FROM api_users WHERE id = 7",
    ).first<{ note: string; enabled: number; password: string; salt: string }>();
    expect(user).toEqual(expect.objectContaining({
      note: "stable",
      enabled: 1,
      password: "old-password",
      salt: "old-salt",
    }));
  });

  it("returns 404 for every unknown update, delete, and rotation target", async () => {
    for (const [path, method, body] of [
      ["/api/app-versions/999", "PUT", { enabled: false }],
      ["/api/app-versions/999", "DELETE", undefined],
      ["/api/users/999", "PUT", { enabled: false }],
      ["/api/users/999", "DELETE", undefined],
      ["/api/users/999/rotate-token", "POST", undefined],
    ] as const) {
      const response = await adminWrite(path, method, body);
      expect(response.status, `${method} ${path}`).toBe(404);
      expect(await response.json()).toMatchObject({ error: expect.any(String) });
    }
  });

  it("requires both the administrator session and CSRF header for every CRUD mutation", async () => {
    const mutations = [
      ["/api/app-versions", "POST", { version: "3.0.0" }],
      ["/api/app-versions/9", "PUT", { enabled: false }],
      ["/api/app-versions/9", "DELETE", undefined],
      ["/api/users", "POST", { username: "bob", password: "password" }],
      ["/api/users/7", "PUT", { enabled: false }],
      ["/api/users/7", "DELETE", undefined],
      ["/api/users/7/rotate-token", "POST", undefined],
    ] as const;

    for (const [path, method, body] of mutations) {
      const missingCsrf = await adminMutation(path, method, body, { csrf: false });
      const missingSession = await adminMutation(path, method, body, { session: false });
      expect(missingCsrf.status, `CSRF ${method} ${path}`).toBe(403);
      expect(missingSession.status, `session ${method} ${path}`).toBe(401);
    }

    expect(await scalar("SELECT COUNT(*) AS value FROM app_versions")).toBe(1);
    expect(await scalar("SELECT COUNT(*) AS value FROM api_users")).toBe(1);
    expect(await textScalar("SELECT token AS value FROM api_users WHERE id = 7")).toBe("old-token");
    expect(await textScalar("SELECT note AS value FROM app_versions WHERE id = 9")).toBe("stable");
    expect(await textScalar("SELECT note AS value FROM api_users WHERE id = 7")).toBe("stable");
  });

  it("validates create payloads, preserves duplicate 409, and creates valid rows", async () => {
    expect((await adminWrite("/api/app-versions", "POST", { version: 123 })).status).toBe(400);
    expect((await adminWrite("/api/app-versions", "POST", { version: "2.0.0" })).status).toBe(409);
    expect((await adminWrite("/api/app-versions", "POST", {
      version: "3.0.0", min_version: "", download_url: "", note: "next",
    })).status).toBe(201);
    expect(await textScalar("SELECT min_version AS value FROM app_versions WHERE version = '3.0.0'")).toBe("0.0.0");

    expect((await adminWrite("/api/users", "POST", { username: 123, password: "password" })).status).toBe(400);
    expect((await adminWrite("/api/users", "POST", { username: "bob", password: "short" })).status).toBe(400);
    expect((await adminWrite("/api/users", "POST", { username: "alice", password: "password" })).status).toBe(409);
    const created = await adminWrite("/api/users", "POST", {
      username: "bob", name: "Bob", password: "password", note: "new",
    });
    const createdBody = await created.json() as Record<string, unknown>;
    expect(created.status).toBe(201);
    expect(createdBody).toMatchObject({ ok: true, id: expect.any(Number), token: expect.stringMatching(/^[0-9a-f]{64}$/) });
  });

  it("linearizes concurrent duplicate creates to one 201 and one 409", async () => {
    const versionResponses = await Promise.all([
      adminWrite("/api/app-versions", "POST", { version: "4.0.0", min_version: "1.0.0" }),
      adminWrite("/api/app-versions", "POST", { version: "4.0.0", min_version: "1.0.0" }),
    ]);
    expect(versionResponses.map((response) => response.status).sort()).toEqual([201, 409]);
    expect(await scalar("SELECT COUNT(*) AS value FROM app_versions WHERE version = '4.0.0'")).toBe(1);

    const userResponses = await Promise.all([
      adminWrite("/api/users", "POST", { username: "carol", password: "password" }),
      adminWrite("/api/users", "POST", { username: "carol", password: "password" }),
    ]);
    expect(userResponses.map((response) => response.status).sort()).toEqual([201, 409]);
    expect(await scalar("SELECT COUNT(*) AS value FROM api_users WHERE username = 'carol'")).toBe(1);
  });

  it("applies valid multi-field updates atomically and authoritatively deletes or rotates", async () => {
    expect((await adminWrite("/api/app-versions/9", "PUT", { enabled: true })).status).toBe(200);
    expect((await adminWrite("/api/users/7", "PUT", { enabled: true })).status).toBe(200);

    const version = await adminWrite("/api/app-versions/9", "PUT", {
      enabled: false,
      min_version: "1.5.0",
      download_url: "https://example.test/app.zip",
      note: "updated",
    });
    expect(version.status).toBe(200);
    expect(await env.DB.prepare(
      "SELECT enabled, min_version, download_url, note FROM app_versions WHERE id = 9",
    ).first()).toEqual(expect.objectContaining({
      enabled: 0,
      min_version: "1.5.0",
      download_url: "https://example.test/app.zip",
      note: "updated",
    }));

    const user = await adminWrite("/api/users/7", "PUT", {
      enabled: false,
      banned: true,
      note: "updated",
      newPassword: "replacement",
    });
    expect(user.status).toBe(200);
    expect(await env.DB.prepare(
      "SELECT enabled, banned, note, password, salt FROM api_users WHERE id = 7",
    ).first()).toEqual(expect.objectContaining({ enabled: 0, banned: 1, note: "updated" }));
    expect(await textScalar("SELECT password AS value FROM api_users WHERE id = 7")).not.toBe("old-password");
    expect(await textScalar("SELECT salt AS value FROM api_users WHERE id = 7")).not.toBe("old-salt");

    const rotated = await adminWrite("/api/users/7/rotate-token", "POST");
    const rotatedBody = await rotated.json() as Record<string, unknown>;
    expect(rotated.status).toBe(200);
    expect(rotatedBody.token).toEqual(expect.stringMatching(/^[0-9a-f]{64}$/));
    expect(await textScalar("SELECT token AS value FROM api_users WHERE id = 7")).toBe(rotatedBody.token);

    expect((await adminWrite("/api/app-versions/9", "DELETE")).status).toBe(200);
    expect((await adminWrite("/api/app-versions/9", "DELETE")).status).toBe(404);
    expect((await adminWrite("/api/users/7", "DELETE")).status).toBe(200);
    expect((await adminWrite("/api/users/7", "DELETE")).status).toBe(404);
  });
});

function expectStrictSecurityHeaders(response: Response, label: string): void {
  expect(response.headers.get("content-security-policy"), label).toBe(STRICT_CSP);
  expect(response.headers.get("cache-control"), label).toBe("no-store");
  expect(response.headers.get("strict-transport-security"), label)
    .toBe("max-age=31536000; includeSubDomains");
  expect(response.headers.get("x-content-type-options"), label).toBe("nosniff");
  expect(response.headers.get("x-frame-options"), label).toBe("DENY");
  expect(response.headers.get("referrer-policy"), label).toBe("no-referrer");
  expect(response.headers.get("permissions-policy"), label)
    .toBe("camera=(), microphone=(), geolocation=()");
  expect(response.headers.get("cross-origin-opener-policy"), label).toBe("same-origin");
  expect(response.headers.get("cross-origin-resource-policy"), label).toBe("same-origin");
}

function poisonAdminEnv({ seed = true } = {}): {
  env: AdminEnv;
  accesses: () => number;
} {
  let accesses = 0;
  const DB = new Proxy({}, {
    get() {
      accesses += 1;
      throw new Error("poison D1 access");
    },
  }) as D1Database;
  return {
    env: {
      DB,
      ADMIN_SEED_PASSWORD: seed ? "must-not-reach-seed" : undefined,
      ADMIN_SEED_USERNAME: "poison",
      ONLINE_TIMEOUT_MS: "120000",
    },
    accesses: () => accesses,
  };
}

function poisonApiRequest(path: string): Request {
  return new Request(`${ORIGIN}${path}`, {
    headers: { Cookie: "nwflash_session=poison-session" },
  });
}

async function adminWrite(path: string, method: string, body?: unknown): Promise<Response> {
  return adminMutation(path, method, body);
}

async function adminMutation(
  path: string,
  method: string,
  body?: unknown,
  options: { session?: boolean; csrf?: boolean } = {},
): Promise<Response> {
  const headers = new Headers();
  if (options.session !== false) headers.set("Cookie", `nwflash_session=${CRUD_SESSION}`);
  if (options.csrf !== false) headers.set("X-Requested-With", "XMLHttpRequest");
  const init: RequestInit = { method, headers };
  if (body !== undefined) {
    headers.set("Content-Type", "application/json");
    init.body = JSON.stringify(body);
  }
  return adminWorker.fetch(new Request(`${ORIGIN}${path}`, init), env);
}

async function scalar(query: string): Promise<number> {
  const row = await env.DB.prepare(query).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function textScalar(query: string): Promise<string | null> {
  const row = await env.DB.prepare(query).first<{ value: string | null }>();
  return row?.value ?? null;
}
