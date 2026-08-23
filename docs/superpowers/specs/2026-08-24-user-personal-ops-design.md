# Nwflash User Personal Ops Design

## Status

Approved for implementation on 2026-08-24.

This specification owns only 'cloudflare/user/**'. It must not modify
'cloudflare/web/**', the shared 'cloudflare/web/schema.sql', Rust/Tauri code,
VMP code, or deployed Cloudflare resources.

## Product direction

'user.nwflash.cc.cd' is a Personal Ops workspace for one licensed user. It is
not a reduced administrator console.

The primary navigation is:

1. 概览
2. 我的活动
3. 设备与会话
4. 安全设置

ROM queries are an activity type and secondary filter inside “我的活动”; they
are not a primary navigation item.

The visual system uses a calm personal workspace with a dark navigation rail
and light task surfaces. Core content is 13–14 px. Every interactive target is
at least 44 px high. The page supports 320 px width, keyboard navigation,
visible focus, reduced motion, and native accessible dialogs.

## Information and privacy boundary

The user portal exposes only data owned by the authenticated API user.
Ownership checks happen in the Worker query, not in the browser.

The user activity drill-down has exactly three levels:

1. 'activity': time, type, status, safe summary, duration, client version when
   present.
2. 'operation': opaque activity ID, start/end time, safe operation category,
   status, duration, and whether step telemetry exists.
3. 'step': sequence, generic phase label, status, duration, retry count, exit
   code, a structured safe error category, and a remediation message.

The user portal never returns or renders:

- real partition names or target slots;
- raw commands or arguments;
- complete stdout or stderr;
- complete device serial numbers;
- local or remote filesystem paths;
- bearer tokens, signing material, or private keys;
- full session identifiers as visible text;
- ROM result URLs or signed download URLs;
- raw client-provided operation titles;
- any other user’s data or administrator-only audit fields.

The current shared schema has only operation summaries in 'usage_logs'. Until a
separate sanitized step contract is available, operation detail responses must
return:

~~~json
{
  "steps_state": "unavailable",
  "steps": [],
  "steps_message": "无更详细数据"
}
~~~

The Worker and UI must not infer, synthesize, or fabricate steps. Visual sample
records are labelled 'MOCK · 待脱敏遥测支持'.

## Authentication contract

The portal uses a host-only HttpOnly cookie instead of exposing the API bearer
token to browser JavaScript:

~~~text
__Host-nwflash_user=<token>; Path=/; HttpOnly; Secure; SameSite=Strict
~~~

The cookie is a session cookie unless the login request explicitly sets
'remember: true', in which case it receives a bounded Max-Age of 30 days.

'POST /api/login' accepts:

~~~json
{
  "username": "alice",
  "password": "correct horse",
  "remember": false
}
~~~

It returns the user identity but never returns the token:

~~~json
{
  "ok": true,
  "username": "alice",
  "name": "Alice"
}
~~~

All authenticated user-portal APIs consume the HttpOnly cookie. All non-GET
APIs require 'X-Requested-With: XMLHttpRequest'. Login attempts are keyed by a
SHA-256 digest of IP plus normalized username; raw IP is not persisted by this
Worker.

'POST /api/logout' clears only the portal cookie. It does not revoke desktop
sessions or the account token.

## Password change and authoritative token revocation

The user-approved rule is:

> Password change deletes the old server token. A new token is issued only
> after the user authenticates again.

'POST /api/me/password' accepts 'current' and 'newPassword'. The new password
must be 8–128 characters, differ from the current password, and pass current
password verification.

On success, one D1 batch performs the authoritative state transition:

1. replace the current 'api_users.token' with a unique, unreturned
   'revoked:<64-hex>' marker while updating password hash and salt;
2. delete every 'session_leases' row for the user;
3. delete every 'online_sessions' row for the user.

The update is bound to both the authenticated user ID and old token. This makes
the old token invalid immediately on the server. The response clears the
portal cookie and returns:

~~~json
{
  "ok": true,
  "reauthenticate": true
}
~~~

The browser then returns to the login view. Clearing browser state is only a
consequence; it is not the revocation mechanism.

After password authentication, if the stored token has the 'revoked:' prefix,
the login handler atomically replaces that marker with a fresh 32-byte random
hex token before setting the new HttpOnly cookie. Concurrent reauthentication
uses a compare-and-swap update so only one fresh token becomes authoritative;
another valid concurrent login reads and uses the winning token.

The shared desktop/API login Worker must adopt the same revoked-marker
reauthentication rule before client integration begins. This repository task
records that exact handoff under 'cloudflare/user/docs/', but does not modify
the shared API Worker.

## User API

All ownership misses deliberately return the same '404' response as nonexistent
records.

### Page and assets

- 'GET /' returns the portal HTML.
- 'GET /portal/styles.css' returns the portal stylesheet.
- 'GET /portal/app.js' returns the portal module.

The HTML uses only same-origin external CSS and JavaScript. CSP removes
"'unsafe-inline'" from 'script-src' and 'style-src'.

### Account and overview

- 'GET /api/me' returns 'username', display 'name', and active session count.
- 'GET /api/me/overview' returns seven-day counts for total activity, tool
  operations, ROM queries, successes, failures, and active sessions.

### Activities

- 'GET /api/me/activities?type=all|operation|rom&status=all|success|failed|canceled&limit&offset'
  returns a merged, ownership-scoped activity list.
- 'GET /api/me/activities/:activityId' returns one ownership-scoped sanitized
  detail.

Activity IDs are 'operation:<positive integer>' or 'rom:<positive integer>'.
The Worker parses and validates the prefix and integer before querying.

The list query uses a server-side union of 'usage_logs' and 'access_logs' and
never selects 'usage_logs.title' or 'access_logs.url'. Safe operation labels are
derived only from a fixed mapping of 'operation_kind'. ROM status is derived
from the HTTP status and exposes only PD, version, status, and timestamp.

### Sessions

- 'GET /api/me/sessions' returns only the user’s active sessions with masked IP,
  client version, connected time, last-seen time, duration, and pending-exit
  state.
- 'POST /api/me/sessions/kick' accepts a session ID, scopes the update by
  'user_id', and returns '404' for both foreign and missing sessions.

The Worker masks IPv4 and IPv6 before serialization. The UI never receives the
raw IP. After a kick succeeds, the UI shows “请求已发送”, polls the session list,
and removes or marks the session offline only after the server no longer
returns it. Timeout or polling failure leaves a retryable pending/error state.

## UI states and navigation

Every data surface implements:

- initial loading;
- empty;
- success;
- recoverable failure with a visible retry action;
- stale/pending where applicable.

The “我的活动” list uses URL state:

~~~text
?view=activity&type=all&status=all&activity=operation%3A123
~~~

Selecting a row pushes history. Back/forward restores filters, selected
activity, and detail state. An inaccessible or deleted deep link shows the same
safe not-found state and keeps the user in the activity list.

Activity API values are rendered with DOM text properties, never HTML
interpolation.

Session kick and password change use native '<dialog>' elements with labelled
titles, initial focus, Escape/cancel support, and focus restoration.

## Testing and acceptance

### Worker/API acceptance

- Login sets an HttpOnly host-only cookie and does not return a token.
- A revoked marker is replaced only after successful reauthentication.
- Password change makes the old cookie and bearer value fail immediately.
- Password change removes session leases and online sessions in the same D1
  batch.
- Foreign activity detail and foreign kick both return indistinguishable 404s.
- Activity responses contain no raw title, URL, partition, command, path, or
  token fields.
- Session responses contain only masked IP values.
- Static assets have correct types and strict CSP.

### UI acceptance

- Four primary navigation items only.
- ROM appears only as an activity type/filter.
- Loading, empty, failure, retry, and success states are test-covered.
- Missing step telemetry renders “无更详细数据”.
- Kick transitions from confirmation to pending to confirmed disappearance.
- Password change confirmation states that the old token and all sessions are
  invalidated; success returns to login.
- Deep links and browser back/forward restore activity state.
- No API string is injected with 'innerHTML'.
- Keyboard focus, dialogs, and 320 px layout are verified.

### Verification

The implementation must run user UI tests, real Workerd+D1 contract tests,
strict TypeScript checking, Wrangler dry-run build, 'git diff --check', and an
independent security/accessibility review. It must not deploy.

## Handoff boundary

After verification, the user API contract is frozen in a handoff document for
the coordinating task. Client integration starts only after that handoff. Any
future sanitized step table or index is specified as SQL requirements in the
handoff; this task does not change the shared schema.
