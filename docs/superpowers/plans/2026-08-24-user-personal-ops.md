# Nwflash User Personal Ops Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox ('- [ ]') syntax for tracking.

**Goal:** Replace 'user.nwflash.cc.cd' with a secure Personal Ops portal whose server-scoped activity, session, and password flows enforce the approved privacy and token-revocation rules.

**Architecture:** The user Worker remains the single same-origin boundary for HTML, static assets, authentication, and D1 queries. The browser receives a host-only HttpOnly cookie, never a bearer token. A framework-free HTML/CSS/JavaScript client renders four Personal Ops views and consumes only sanitized Worker responses.

**Tech Stack:** Cloudflare Workers, D1, TypeScript, vanilla ES modules, Vitest 4.1.11, Cloudflare Vitest Plugin, Happy DOM 20.11.6, Wrangler 4.

## Global Constraints

- Own only 'cloudflare/user/**' plus this specification and plan.
- Do not modify 'cloudflare/web/**', 'cloudflare/web/schema.sql', 'cloudflare/src/**', Rust/Tauri, VMP, or deployment state.
- Do not deploy.
- Primary navigation is exactly: 概览, 我的活动, 设备与会话, 安全设置.
- ROM query is an activity type/filter, never primary navigation.
- Core text is 13–14 px and interactive targets are at least 44 px.
- Support keyboard use, native labelled dialogs, reduced motion, and 320 px width.
- User activity is only activity → operation → step.
- Never return or render real partition names, raw commands, stdout/stderr, complete serials, paths, tokens, signing material, raw operation titles, ROM URLs, or signed URLs.
- If real sanitized step telemetry is absent, return and render exactly '无更详细数据'; never fabricate steps.
- Password change replaces the old token with an unreturned 'revoked:<64-hex>' marker and removes all leases/sessions in one D1 batch.
- A fresh token is generated only after successful reauthentication sees the revoked marker.
- Ownership misses for foreign and nonexistent resources both return the same 404.
- Every behavior change follows RED → verify failure → GREEN → verify pass.

---

## File map

### Portal presentation

- Create 'cloudflare/user/src/portal/index.html': semantic login and four-view application shell.
- Create 'cloudflare/user/src/portal/styles.css': Personal Ops tokens, responsive layout, focus, dialogs, loading/error/empty states.
- Create 'cloudflare/user/src/portal/app.client.js': same-origin API client, state machine, rendering, history, polling, dialogs.
- Create 'cloudflare/user/src/portal/app.client.test.js': real Happy DOM behavior tests.
- Create 'cloudflare/user/vitest.ui.config.ts': Happy DOM Vitest configuration.
- Modify 'cloudflare/user/package.json' and create 'cloudflare/user/package-lock.json': pinned test dependencies and final scripts.

### Worker and contract

- Modify 'cloudflare/user/src/index.ts': static assets, strict headers, cookie auth, revocation, overview, activities, masked sessions.
- Create 'cloudflare/user/src/assets.d.ts': text-module declarations for portal assets.
- Modify 'cloudflare/user/wrangler.toml': text-module rules for HTML, CSS, and '*.client.js'.
- Create 'cloudflare/user/vitest.workerd.config.ts': real Workerd+D1 test configuration using the existing shared schema read-only.
- Create 'cloudflare/user/test/user.workerd.test.ts': route, D1, concurrency, privacy, and ownership tests.

### Documentation and handoff

- Delete 'cloudflare/user/src/user.html': remove the old monolithic glass UI after the Worker imports the new portal.
- Rewrite 'cloudflare/user/README.md': current architecture, API, security, test, and no-deploy instructions.
- Create 'cloudflare/user/docs/client-api-handoff.md': frozen user API and the shared API Worker revoked-marker requirement for later client integration.

---

### Task 1: Personal Ops portal and UI behavior

**Files:**
- Create: 'cloudflare/user/src/portal/index.html'
- Create: 'cloudflare/user/src/portal/styles.css'
- Create: 'cloudflare/user/src/portal/app.client.js'
- Create: 'cloudflare/user/src/portal/app.client.test.js'
- Create: 'cloudflare/user/vitest.ui.config.ts'
- Modify: 'cloudflare/user/package.json'
- Create: 'cloudflare/user/package-lock.json'

**Interfaces:**
- Consumes: JSON routes specified in 'docs/superpowers/specs/2026-08-24-user-personal-ops-design.md'.
- Produces: 'createPortal(options)' in 'app.client.js'; static files imported and served by Task 2.

- [ ] **Step 1: Install the test runtime**

Run from 'cloudflare/user':

~~~powershell
npm install --save-dev vitest@4.1.11 happy-dom@20.11.6 @cloudflare/vitest-plugin@1.0.0 typescript@5.9.3
~~~

Add scripts with these exact final names:

~~~json
{
  "test": "npm run test:ui && npm run test:worker",
  "test:ui": "vitest run --config vitest.ui.config.ts",
  "test:worker": "vitest run --config vitest.workerd.config.ts",
  "typecheck": "tsc --noEmit --strict --target ES2022 --module ESNext --moduleResolution Bundler --lib ES2022,WebWorker,DOM --types @cloudflare/workers-types --skipLibCheck src/index.ts && wrangler deploy --dry-run --outdir .wrangler/personal-ops-build"
}
~~~

Task 1 runs only 'npm run test:ui'; 'test:worker' becomes runnable in Task 2.

- [ ] **Step 2: Write failing UI tests**

Create 'vitest.ui.config.ts' with 'environment: "happy-dom"' and include only
'src/portal/**/*.test.js'.

Write behavior tests that import the real 'createPortal' and build the actual
'index.html' in Happy DOM. Cover at minimum:

~~~js
it('renders exactly four primary navigation destinations', () => {
  const labels = [...document.querySelectorAll('[data-nav]')]
    .map((node) => node.textContent.trim());
  expect(labels).toEqual(['概览', '我的活动', '设备与会话', '安全设置']);
});

it('renders unavailable telemetry without inventing step rows', async () => {
  fetchQueue.respond('/api/me/activities/operation:7', {
    id: 'operation:7',
    steps_state: 'unavailable',
    steps: [],
    steps_message: '无更详细数据'
  });
  await clickActivity('operation:7');
  expect(document.querySelector('[data-step-state]').textContent).toBe('无更详细数据');
  expect(document.querySelectorAll('[data-step-row]')).toHaveLength(0);
});

it('renders API strings as text instead of markup', async () => {
  fetchQueue.respond('/api/me', {
    loggedIn: true,
    username: 'alice',
    name: '<img src=x onerror=alert(1)>',
    online: 0
  });
  await startPortal();
  expect(document.querySelector('img')).toBeNull();
  expect(document.querySelector('[data-user-name]').textContent)
    .toBe('<img src=x onerror=alert(1)>');
});

it('keeps a kicked session pending until the server stops returning it', async () => {
  await openKickDialog('session-owned');
  await confirmKick();
  expect(sessionStatus()).toBe('请求已发送');
  fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
  await pollOnce();
  expect(sessionStatus()).toBe('请求已发送');
  fetchQueue.respond('/api/me/sessions', { count: 0, sessions: [] });
  await pollOnce();
  expect(document.querySelector('[data-session="session-owned"]')).toBeNull();
});

it('returns to login after authoritative password revocation', async () => {
  fetchQueue.respond('/api/me/password', { ok: true, reauthenticate: true });
  await submitPasswordChange();
  expect(document.querySelector('[data-view="login"]').hidden).toBe(false);
  expect(window.localStorage.length).toBe(0);
  expect(window.sessionStorage.length).toBe(0);
});
~~~

Also test loading, empty, retryable error, ROM filtering, URL deep-link
restoration, popstate back/forward, dialog cancel/focus restoration, and a
failed session poll that leaves a retry action.

- [ ] **Step 3: Run the UI tests and verify RED**

Run:

~~~powershell
npm run test:ui
~~~

Expected: FAIL because 'index.html' and 'createPortal' do not exist. Record the
command and expected failure in the task report.

- [ ] **Step 4: Implement the semantic HTML shell**

Create one login surface and one authenticated application surface. The app
contains exactly four navigation buttons with 'data-nav' values 'overview',
'activity', 'sessions', and 'security'. Include:

- one 'aria-live="polite"' status region;
- native '<dialog>' elements for session kick and password change;
- explicit loading, empty, error, and retry containers per view;
- an activity list and detail region;
- no inline script or style;
- '<link rel="stylesheet" href="/portal/styles.css">';
- '<script type="module" src="/portal/app.js"></script>'.

- [ ] **Step 5: Implement the UI controller**

Export:

~~~js
export function createPortal({
  document,
  window,
  fetchImpl = window.fetch.bind(window),
  setTimeoutImpl = window.setTimeout.bind(window),
  clearTimeoutImpl = window.clearTimeout.bind(window)
}) {
  return {
    start,
    destroy,
    retryCurrentView,
    pollSessionsOnce
  };
}
~~~

The controller must:

- call same-origin fetch with 'credentials: "same-origin"';
- add 'X-Requested-With: XMLHttpRequest' to non-GET requests;
- never create an Authorization header;
- never read or write a token in Web Storage;
- render API data using 'textContent' and element creation;
- parse and serialize 'view', 'type', 'status', and 'activity' URL parameters;
- fetch a selected activity detail without synthesizing steps;
- poll sessions after kick and keep pending until absence is confirmed;
- return to login on any 401 and after password change success;
- use the Worker’s safe messages for recoverable states.

- [ ] **Step 6: Implement the Personal Ops stylesheet**

Use the approved dark rail and light task surfaces. Enforce:

- core body and control text at 13–14 px;
- all buttons, inputs, tabs, and navigation targets at least 44 px;
- visible ':focus-visible';
- no glass blur;
- responsive single-column layouts at 820 px and 320 px;
- no horizontal clipping at 320 px;
- '@media (prefers-reduced-motion: reduce)'.

- [ ] **Step 7: Run UI tests and verify GREEN**

Run:

~~~powershell
npm run test:ui
~~~

Expected: all UI tests pass with no warnings.

- [ ] **Step 8: Self-review and commit**

Confirm no 'innerHTML' assignment uses API data, no storage contains a token,
and no primary ROM navigation exists. Commit only Task 1 files:

~~~powershell
git add -- cloudflare/user/package.json cloudflare/user/package-lock.json cloudflare/user/vitest.ui.config.ts cloudflare/user/src/portal
git commit -m "feat(user): build personal ops portal"
~~~

---

### Task 2: Cookie authentication, sanitized APIs, and authoritative revocation

**Files:**
- Modify: 'cloudflare/user/src/index.ts'
- Create: 'cloudflare/user/src/assets.d.ts'
- Modify: 'cloudflare/user/wrangler.toml'
- Create: 'cloudflare/user/vitest.workerd.config.ts'
- Create: 'cloudflare/user/test/user.workerd.test.ts'

**Interfaces:**
- Consumes: portal assets from Task 1 and the current shared schema without mutation.
- Produces: the exact User API and cookie contract consumed by Task 1.

- [ ] **Step 1: Write the Workerd configuration**

Mirror 'cloudflare/vitest.workerd.config.ts', but use
'configPath: "./wrangler.toml"', copy '../web/schema.sql' into a temporary
migration directory, bind the migrations as 'TEST_MIGRATIONS', include
'test/user.workerd.test.ts', and treat HTML, CSS, and '*.client.js' as Text
modules.

- [ ] **Step 2: Write failing real-D1 route tests**

Use 'reset()' and 'applyD1Migrations()' before each test and invoke the actual
user Worker. Write independent tests for:

~~~ts
it("sets an HttpOnly cookie without returning the token on login", async () => {
  const response = await postLogin("alice", PASSWORD, false);
  const body = await response.json() as Record<string, unknown>;
  expect(response.status).toBe(200);
  expect(response.headers.get("set-cookie")).toContain("__Host-nwflash_user=");
  expect(response.headers.get("set-cookie")).toContain("HttpOnly");
  expect(response.headers.get("set-cookie")).toContain("SameSite=Strict");
  expect(body).not.toHaveProperty("token");
});

it("revokes the old token and removes every session in one password change", async () => {
  const oldToken = await seedUserAndSessions();
  const response = await changePassword(cookie(oldToken), PASSWORD, NEW_PASSWORD);
  expect(response.status).toBe(200);
  expect(await scalar("SELECT COUNT(*) FROM api_users WHERE token = ?", oldToken)).toBe(0);
  expect(await scalar("SELECT COUNT(*) FROM session_leases WHERE user_id = 7")).toBe(0);
  expect(await scalar("SELECT COUNT(*) FROM online_sessions WHERE user_id = 7")).toBe(0);
  expect(await getMe(cookie(oldToken))).toHaveProperty("status", 401);
});

it("issues a fresh token only after reauthentication of a revoked account", async () => {
  const oldToken = await seedRevokedUser();
  const rejected = await postLogin("alice", "wrong password", false);
  expect(rejected.status).toBe(401);
  expect(await storedToken()).toBe(oldToken);
  const accepted = await postLogin("alice", NEW_PASSWORD, false);
  expect(accepted.status).toBe(200);
  expect(await storedToken()).toMatch(/^[0-9a-f]{64}$/);
  expect(await storedToken()).not.toBe(oldToken);
});

it("returns the same 404 for foreign and missing activity details", async () => {
  const foreign = await getActivity(cookie(ALICE_TOKEN), "operation:88");
  const missing = await getActivity(cookie(ALICE_TOKEN), "operation:999999");
  expect(foreign.status).toBe(404);
  expect(await foreign.json()).toEqual(await missing.json());
});

it("never serializes raw activity titles or ROM URLs", async () => {
  await seedUnsafeActivity({
    title: "flash vendor_boot_a C:/secret/image.img",
    url: "https://example.test/file.zip?sign=secret"
  });
  const response = await getActivities(cookie(ALICE_TOKEN));
  const bodyText = await response.text();
  expect(bodyText).not.toContain("vendor_boot_a");
  expect(bodyText).not.toContain("C:/secret");
  expect(bodyText).not.toContain("sign=secret");
});
~~~

Also cover hashed login-attempt keys, remembered-cookie Max-Age, cookie-only
authentication, logout, concurrent revoked-token login CAS, password-update
CAS conflict, overview ownership, activity filters/pagination, malformed IDs,
ROM detail without URL, IPv4/IPv6 masking, foreign kick 404, pending kick, CSP,
and static asset content types.

- [ ] **Step 3: Run Workerd tests and verify RED**

Run:

~~~powershell
npm run test:worker
~~~

Expected: FAIL because the routes and cookie contract are not implemented.
Record the expected failures.

- [ ] **Step 4: Implement strict static responses and cookie auth**

Import the three portal Text modules. Serve '/', '/portal/styles.css', and
'/portal/app.js' with correct content types. Use CSP:

~~~text
default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:;
connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'
~~~

Use cookie name '__Host-nwflash_user'. Login responses never include the token.
Authenticated routes accept only that cookie. A token with the 'revoked:'
prefix is never accepted as authentication even if presented directly.
Invalid or revoked cookies return 401 and expire the cookie.

- [ ] **Step 5: Implement server-authoritative password revocation**

Use constants:

~~~ts
const USER_COOKIE = "__Host-nwflash_user";
const REVOKED_TOKEN_PREFIX = "revoked:";
const REMEMBER_MAX_AGE_SECONDS = 30 * 24 * 60 * 60;
~~~

Password change generates 'revoked:' plus 'randomHex(32)' and performs one
'env.DB.batch' containing:

~~~sql
UPDATE api_users
SET salt = ?, password = ?, token = ?
WHERE id = ? AND token = ? AND enabled = 1 AND banned = 0;

DELETE FROM session_leases WHERE user_id = ?;
DELETE FROM online_sessions WHERE user_id = ?;
~~~

Require the first statement’s change count to equal one. Return
'{ok:true,reauthenticate:true}' and an expired cookie. On a revoked-marker
login, compare-and-swap the marker to one fresh 'randomHex(32)' token. A
concurrent loser re-reads the winning active token after successful password
authentication; it never returns or stores another candidate.

- [ ] **Step 6: Implement overview and sanitized activity queries**

Use a validated 'type' enum ('all', 'operation', 'rom') and 'status' enum
('all', 'success', 'failed', 'canceled'). Clamp limit to 1–100 and offset to a
nonnegative integer.

Build the list and count from a SQL union that selects no raw title or URL:

~~~sql
SELECT *
FROM (
  SELECT 'operation' AS activity_type, id, operation_kind,
         CASE WHEN status IN ('success','failed','canceled') THEN status ELSE 'failed' END AS status,
         started_at AS occurred_at, ended_at, duration_ms,
         NULL AS pd, NULL AS version, NULL AS http_status
  FROM usage_logs WHERE api_user_id = ?
  UNION ALL
  SELECT 'rom' AS activity_type, id, 'RomQuery' AS operation_kind,
         CASE WHEN status BETWEEN 200 AND 299 THEN 'success' ELSE 'failed' END AS status,
         CAST(strftime('%s', created_at) AS INTEGER) AS occurred_at,
         NULL AS ended_at, NULL AS duration_ms,
         pd, version, status AS http_status
  FROM access_logs WHERE api_user_id = ?
) AS activities
WHERE (? = 'all' OR activity_type = ?)
  AND (? = 'all' OR status = ?)
ORDER BY occurred_at DESC, id DESC
LIMIT ? OFFSET ?;
~~~

Map 'operation_kind' through a fixed safe-label map; never fall back to the raw
title. Operation detail returns 'steps_state: "unavailable"', an empty steps
array, and 'steps_message: "无更详细数据"'. Detail queries include both ID and
authenticated user ID.

- [ ] **Step 7: Implement masked sessions and ownership-safe kick**

Serialize 'ip_masked', never 'ip'. IPv4 keeps the first three octets and
replaces the fourth with '••'. IPv6 keeps at most the first two nonempty
segments and appends '…'. Invalid or absent values become '已隐藏'.

Kick updates:

~~~sql
UPDATE online_sessions
SET force_exit_at = ?, force_exit_reason = ?
WHERE session_id = ? AND user_id = ?
RETURNING session_id;
~~~

No returned row produces the same 404 as a missing session.

- [ ] **Step 8: Run Workerd, UI, and type checks**

Run:

~~~powershell
npm run test:worker
npm run test:ui
npm run typecheck
~~~

Expected: Workerd and UI suites pass, then strict TypeScript and Wrangler
dry-run exit 0.

- [ ] **Step 9: Self-review and commit**

Confirm the Worker never selects 'usage_logs.title' or 'access_logs.url' for
user routes, bearer auth is absent, and no shared file changed. Commit:

~~~powershell
git add -- cloudflare/user/src/index.ts cloudflare/user/src/assets.d.ts cloudflare/user/wrangler.toml cloudflare/user/vitest.workerd.config.ts cloudflare/user/test/user.workerd.test.ts
git commit -m "feat(user): enforce personal ops contracts"
~~~

---

### Task 3: Remove the old portal and freeze the handoff

**Files:**
- Delete: 'cloudflare/user/src/user.html'
- Modify: 'cloudflare/user/README.md'
- Create: 'cloudflare/user/docs/client-api-handoff.md'

**Interfaces:**
- Consumes: the verified UI and Worker contracts from Tasks 1–2.
- Produces: the frozen handoff used before client integration begins.

- [ ] **Step 1: Remove the old UI**

Delete only 'cloudflare/user/src/user.html'. Verify 'src/index.ts' imports the
new 'src/portal/index.html' before deletion.

- [ ] **Step 2: Rewrite the user README**

Document:

- Personal Ops positioning and four primary views;
- HttpOnly cookie auth and no browser-visible bearer token;
- activity privacy boundary and unavailable step behavior;
- password revoked-marker and forced reauthentication flow;
- current API table;
- exact local test/typecheck commands;
- explicit “do not deploy from verification” note.

- [ ] **Step 3: Write the client/API handoff**

Freeze request/response examples for login, me, overview, activities, activity
detail, sessions, kick, password, and logout.

State the later shared API Worker requirement exactly:

1. after password verification, if 'api_users.token' starts with 'revoked:',
   compare-and-swap it to a fresh 32-byte hex token before creating a signed
   lease;
2. never return a revoked marker;
3. old-token heartbeat/auth returns 401 and the desktop exits;
4. no client integration starts until this server behavior is implemented and
   contract-tested by the coordinating task.

For future sanitized steps, specify that a handoff migration may add an
ownership-indexed table containing only generic phase, sequence, status,
duration, retry count, exit code, safe error category, and remediation. It must
not store user-visible partition, command, raw output, serial, path, token, or
signed URL fields. Do not edit the shared schema in this task.

- [ ] **Step 4: Run full verification**

Run from 'cloudflare/user':

~~~powershell
npm test
npm run typecheck
~~~

Run from the repository root:

~~~powershell
git diff --check
git status --short
~~~

Expected: all user tests and dry-run build pass, diff check is clean, and no
'cloudflare/user/.wrangler/' entry remains in final status.

- [ ] **Step 5: Commit documentation and deletion**

~~~powershell
git add -- cloudflare/user/README.md cloudflare/user/docs/client-api-handoff.md
git rm -- cloudflare/user/src/user.html
git commit -m "docs(user): freeze personal ops handoff"
~~~

---

## Independent final review

After the three tasks pass task-scoped reviews:

1. dispatch one read-only security/accessibility reviewer;
2. verify cookie flags, token revocation, CAS behavior, ownership 404s, XSS
   boundaries, CSP, dialog labels/focus, pending session state, and 320 px CSS;
3. run a whole-branch code review against this plan;
4. send one fix wave for any Critical/Important findings, then one scoped
   re-review;
5. remove only the verified 'cloudflare/user/.wrangler/' directory after
   resolving its absolute path and confirming it remains below
   'cloudflare/user';
6. freeze the final API contract and notify the coordinating task;
7. do not deploy.
