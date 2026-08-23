# Personal Ops user portal

`cloudflare/user` is the Personal Ops self-service portal for an authorized user's account. It has four primary views: overview, activity, devices and sessions, and security.

## Authentication and privacy

Login creates the host-only `__Host-nwflash_user` cookie. It is `Secure`, `HttpOnly`, `SameSite=Strict`, and scoped to `/`; an optional remembered login lasts 30 days. The browser never receives, persists, or sends a bearer token through JavaScript. Every non-GET API call, including `POST /api/login` and `POST /api/logout`, requires `X-Requested-With: XMLHttpRequest`. Login is credential-authenticated rather than cookie-authenticated. Logout does not require an authenticated cookie and always expires only the portal cookie.

Each allowed login attempt performs exactly one PBKDF2 operation using the account credential or a dummy credential. Rate records contain only domain-separated SHA-256 keys. The per-credential limit is supplemented by a coarse IP-wide ceiling so rotating usernames cannot create unbounded credential work; raw IPs and usernames are not stored in `login_attempts`.

Overview activity metrics cover the most recent seven days. Activities are restricted to the authenticated owner and return only safe summaries. Operation detail currently returns `steps_state: "unavailable"`, an empty `steps` array, and `steps_message: "无更详细数据"`; the portal must not invent steps. Sessions expose only the allowlisted fields `id`, `clientVersion`, `ip_masked`, `connectedAt`, `lastSeenAt`, `duration`, and `pendingExit`. Raw IP and `force_exit_reason` are never serialized.

Changing a password requires a different value of 8–128 characters. A winning compare-and-swap replaces the authoritative `api_users.token` with this request's unique `revoked:` marker and conditionally deletes that user's leases and online sessions in the same D1 batch. A stale CAS loser cannot delete sessions. The response expires the cookie and requires reauthentication. Any later request authenticated by a revoked or missing token returns 401 and expires the cookie.

The portal distinguishes logged-out state from a recoverable `/api/me` bootstrap failure. Activity list and detail requests, session polls, and authentication actions ignore stale responses. Activity detail has idle, loading, error/retry, and success states. Authoritative `pendingExit` sessions resume polling after reload; rows reconcile by session key, preserve focused controls, and move focus to the session heading when a focused row disappears. Loading, error, pending, timeout, and confirmed-disappearance changes use busy states, alerts, and live announcements. Kick errors remain recoverable inside the open native dialog.

## Current API

All responses are JSON. `/api/me` endpoints require the HttpOnly cookie. The login and logout exceptions are described above.

| Endpoint | Method | Purpose |
| --- | --- | --- |
| `/api/login` | POST | Authenticate and set the cookie |
| `/api/logout` | POST | Expire the cookie |
| `/api/me` | GET | Current identity and active-session count |
| `/api/me/overview` | GET | Most-recent-seven-day activity totals and current sessions |
| `/api/me/activities?type&status&limit&offset` | GET | Owned, sanitized activity list |
| `/api/me/activities/{activityId}` | GET | Owned activity detail; unknown or unowned is 404 |
| `/api/me/sessions` | GET | Owned active sessions with masked IPs |
| `/api/me/sessions/kick` | POST | Request exit for an owned session |
| `/api/me/password` | POST | Change password and force reauthentication |

The frozen request and response shapes are in [docs/client-api-handoff.md](docs/client-api-handoff.md). The subsystem design and security boundaries are in [docs/architecture.md](docs/architecture.md).

## Local verification

Run from `cloudflare/user`:

```powershell
npm test
npm run typecheck
```

These commands run local tests and a dry-run Worker build only. Verification does **not** authorize or perform a deployment. Client integration remains on hold until the coordinating task implements and contract-tests the shared API Worker revoked-marker behavior.
