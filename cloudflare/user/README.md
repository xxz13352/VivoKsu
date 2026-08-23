# Personal Ops user portal

`cloudflare/user` is the Personal Ops self-service portal for an authorized user's account. It has four primary views: overview, activity, devices and sessions, and security.

## Authentication and privacy

Login creates the `__Host-nwflash_user` cookie. It is `Secure`, `HttpOnly`, `SameSite=Strict`, and scoped to `/`; an optional remembered login lasts 30 days. The browser never receives, persists, or sends a bearer token through JavaScript. `POST /api/login` is unauthenticated and does not require `X-Requested-With`. `POST /api/logout` does not require an authenticated cookie, but requires `X-Requested-With: XMLHttpRequest` and always expires the portal cookie. Other `/api/me` write endpoints require both a valid cookie and that header.

Activities are restricted to the authenticated owner and return only safe summaries. Operation detail currently returns `steps_state: "unavailable"`, an empty `steps` array, and `steps_message: "无更详细数据"`; the portal must not invent steps. Sessions expose only `ip_masked`, never the raw IP.

Changing a password replaces the authoritative `api_users.token` with a `revoked:` marker and deletes that user's leases and online sessions. The response expires the cookie and requires reauthentication. Any later request authenticated by a revoked or missing token returns 401 and expires the cookie.

## Current API

All responses are JSON. `/api/me` endpoints require the HttpOnly cookie. The login and logout exceptions are described above.

| Endpoint | Method | Purpose |
| --- | --- | --- |
| `/api/login` | POST | Authenticate and set the cookie |
| `/api/logout` | POST | Expire the cookie |
| `/api/me` | GET | Current identity and active-session count |
| `/api/me/overview` | GET | Activity and session totals |
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
