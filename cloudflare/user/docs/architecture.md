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

`POST /api/login` checks rate limits and PBKDF2 credentials. On success it sets the Secure, HttpOnly, SameSite=Strict `__Host-nwflash_user` cookie; browser JavaScript sees neither the cookie value nor a bearer token. `authenticateUser` reads that cookie, rejects revoked markers, verifies the current enabled/unbanned account, and returns a cookie-expiring 401 for invalid credentials.

Password change validates the current password, atomically installs a `revoked:` token marker, deletes the user's session leases and online sessions, and expires the cookie. The portal returns to login when the response requests reauthentication or when any API call returns 401. Login may atomically exchange a revoked marker for a fresh local token before setting a cookie.

The later shared API Worker must perform its own compare-and-swap exchange after password verification and before signed-lease creation, never emit a revoked marker, and make old-token heartbeat/auth return 401 so the desktop exits. Client integration waits until the coordinating task implements and contract-tests that behavior.

## Personal data flows

Activity list queries are always constrained by `api_user_id`; activity detail also constrains the selected ID by owner. Unknown and cross-account activity both return 404. Operation labels are allow-listed and ROM records expose only their defined summary fields. Operation steps are explicitly unavailable today, so the response is an empty list with an unavailable state rather than fabricated progress.

Session list queries the authenticated owner only and derives `ip_masked` before serialization. A kick updates only a matching owned session. After a successful request, the client marks it pending, polls the list every two seconds, and treats disappearance as confirmation. If it remains after six polls, the client shows an unconfirmed state and retry affordance.

The portal persists navigation state in query parameters: `view`, `type`, `status`, and `activity`. It uses `pushState` for navigation/filter/detail selection and reads state on `popstate`, so browser back and forward restore the corresponding view and data request.

## Security boundaries

The Worker applies CSP, HSTS, `nosniff`, frame denial, no-referrer, permission restrictions, and `no-store` to all portal and JSON responses. CSP permits only same-origin script, style, image, and connection sources. Client rendering uses `textContent` and creates DOM nodes rather than interpolating activity or session data as HTML, limiting XSS exposure.

Cookie authentication plus SameSite=Strict is the primary CSRF control; non-GET routes additionally demand `X-Requested-With: XMLHttpRequest`. Session IPs are masked before the response. Ownership predicates protect records and writes; the intentional 404 for absent and unowned activities/sessions prevents existence disclosure.

## Verification and release boundary

UI tests cover rendering, URL/back-forward behavior, dialogs, retry states, and forced reauthentication. Workerd tests cover Worker routing, cookies, login/revocation, ownership 404s, sanitization, sessions, and security headers.

Run from `cloudflare/user`:

```powershell
npm test
npm run typecheck
```

These checks include a dry-run Worker build. They do not deploy. Deployment, shared-schema changes, the shared API Worker change, and desktop-client integration are outside this subsystem handoff and remain with the coordinating task.
