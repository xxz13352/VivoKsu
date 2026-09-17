# `cloudflare/user` architecture

This document covers the Personal Ops user portal subsystem only.

## Module tree

```
cloudflare/user/
├─ src/index.ts                    Worker: routes, authentication, data access, serialization, headers
├─ src/assets.d.ts                 Type declarations for text asset imports
├─ src/portal/index.html           Semantic portal structure, views, dialogs, and accessible labels
├─ src/portal/styles.css           Responsive portal presentation and 320 px layout rules
├─ src/portal/app.client.js        URL state, rendering, cookie API client, dialogs, session polling
├─ src/portal/app.client.test.js   Browser-style UI behavior tests
├─ test/user.workerd.test.ts       Workerd API, authorization, cookie, and serialization tests
├─ vitest.ui.config.ts             UI test configuration
└─ vitest.workerd.config.ts        Worker test configuration
```

`src/index.ts` imports and serves the portal HTML, CSS, and client script. There is no separate legacy page.

## Request and identity flow

`POST /api/login` requires `X-Requested-With: XMLHttpRequest` before rate or credential work. Every allowed attempt performs one PBKDF2 operation against a real or dummy salt/hash. The existing `login_attempts` table stores distinct domain-separated SHA-256 keys for IP-plus-normalized-username and IP-wide windows, providing both the credential limit and a coarse ceiling without persisting raw IPs. On success the Worker sets the host-only Secure, HttpOnly, SameSite=Strict `__Host-nwflash_user` cookie; browser JavaScript sees neither the cookie value nor a bearer token. `authenticateUser` reads that cookie, rejects revoked markers, verifies the current enabled/unbanned account, and returns a cookie-expiring 401 for invalid credentials.

Password change enforces a different 8–128-character value and validates the current password. One D1 batch compare-and-swaps this request's unique `revoked:` token marker and makes both session-table deletes conditional on that marker. A winner removes the user's session leases and online sessions; a stale CAS loser changes zero rows and cannot delete sessions. The response expires the cookie. The portal returns to login when the response requests reauthentication or when an authenticated API call returns 401. Login may atomically exchange a revoked marker for a fresh local token before setting a cookie.

The shared API Worker now performs its own generation-bound compare-and-swap after password verification and before signed-lease creation, never emits a revoked marker, and returns 401 for old-token heartbeat/auth after the version gate. Deterministic shared-API tests force two login reads to observe the same marker before releasing the real D1 CAS; the user Workerd suite also runs the complete password-change → marker → shared login → signed heartbeat flow against the same real D1 database. The frozen contract and executable references are in [client-api-handoff.md](client-api-handoff.md).

## Personal data flows

Overview activity subqueries are constrained by `api_user_id` and a seven-day cutoff; active-session count remains current. Activity list queries are always constrained by `api_user_id`; activity detail also constrains the selected ID by owner. Unknown and cross-account activity both return 404. Operation labels are allow-listed and ROM records expose only their defined summary fields. Operation steps are explicitly unavailable today, so the response is an empty list with an unavailable state rather than fabricated progress.

Session list queries the authenticated owner only, derives `ip_masked`, and serializes exactly the allowlisted session shape. It never selects or returns `force_exit_reason`. A kick updates only a matching owned session. After a successful request, the client marks it pending; after reload it also derives pending work from authoritative `pendingExit` rows. It polls every two seconds and treats disappearance as confirmation. If a row remains after six polls, the client shows an announced unconfirmed state and retry affordance. Session rows reconcile by ID so focused controls survive updates; removal of a focused row moves focus to the session heading.

The portal persists navigation state in query parameters: `view`, `type`, `status`, and `activity`. It uses `pushState` for navigation/filter/detail selection and reads state on `popstate`, so browser back and forward restore the corresponding view and data request. Navigation exposes `aria-current`, selected activity rows expose `aria-pressed`, and request-generation guards prevent older list, detail, session, or authentication responses from replacing newer state.

Bootstrap has distinct loading, logged-out, and recoverable failure/retry states. Activity detail has idle, loading, failure/retry, and success states and renders only allowlisted ID, time, type, status, summary, duration, ROM metadata, and unavailable-step fields. Busy attributes, alert regions, a centralized polite live region, and status nodes announce loading, failure, retry, pending, timeout, and confirmed changes. Native kick and password dialogs restore opener focus; kick failures remain visible and retryable inside the dialog.

## Security boundaries

The Worker applies CSP, HSTS, `nosniff`, frame denial, no-referrer, permission restrictions, and `no-store` to all portal and JSON responses. CSP permits only same-origin script, style, image, and connection sources. Client rendering uses `textContent` and creates DOM nodes rather than interpolating activity or session data as HTML, limiting XSS exposure.

Cookie authentication plus SameSite=Strict is the primary CSRF control; every non-GET route, including unauthenticated login and cookie-clearing logout, additionally demands `X-Requested-With: XMLHttpRequest`. Session IPs are masked before the response, and pending reasons are not serialized. Ownership predicates protect records and writes; the intentional 404 for absent and unowned activities/sessions prevents existence disclosure.

## Verification and release boundary

UI tests cover rendering, URL/back-forward behavior, dialogs, retry states, and forced reauthentication. Workerd tests cover Worker routing, cookies, login/revocation, ownership 404s, sanitization, sessions, security headers, and the real-D1 handoff to the shared API Worker.

Run from `cloudflare/user`:

```powershell
npm test
npm run typecheck
```

These checks include a dry-run Worker build. They do not deploy. The shared API handoff behavior is implemented and tested without a schema change; production deployment and desktop-client rollout remain separate coordinating-task release actions.
