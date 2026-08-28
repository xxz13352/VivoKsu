import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

import adminWorker, { type Env as AdminEnv } from "../web/src/index";
import adminIndexHtml from "../web/src/admin/index.html";
import adminStylesCss from "../web/src/admin/styles.css";
import adminApiJs from "../web/src/admin/api.js";
import adminAppJs from "../web/src/admin/app.js";
import adminComponentsJs from "../web/src/admin/components.js";
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
  it("serves exactly twelve explicit GET assets before touching seed D1", async () => {
    const poison = poisonAdminEnv();
    expect(STATIC_MANIFEST).toHaveLength(12);

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
