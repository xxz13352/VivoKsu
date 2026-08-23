# Personal Ops client/API handoff

This is the frozen `cloudflare/user` contract. JSON is used for every request and response. Cookie-authenticated calls use `credentials: "same-origin"`; the browser does not access a bearer token. Every non-GET call includes `X-Requested-With: XMLHttpRequest`.

## Authentication

### `POST /api/login`

Request:

```json
{"username":"alice","password":"correct horse","remember":false}
```

Success (200; sets the HttpOnly cookie):

```json
{"ok":true,"username":"alice","name":"Alice"}
```

Missing credentials are 400; incorrect or disabled credentials are 401; excessive attempts are 429. A revoked marker is exchanged server-side before the cookie is created, never exposed in the response.

### `GET /api/me`

Success (200):

```json
{"loggedIn":true,"username":"alice","name":"Alice","online":1}
```

No valid cookie returns 401:

```json
{"loggedIn":false,"message":"请先登录。"}
```

An expired or revoked cookie also returns 401, with `{"loggedIn":false,"message":"登录已失效。"}` and an expired cookie.

## Personal Ops data

### `GET /api/me/overview`

Success (200):

```json
{"total":8,"operations":5,"rom":3,"successes":6,"failures":2,"activeSessions":1}
```

### `GET /api/me/activities?type=all&status=all&limit=50&offset=0`

`type` is `all`, `operation`, or `rom`; `status` is `all`, `success`, `failed`, or `canceled`. `limit` is clamped to 1–100 and defaults to 100; `offset` defaults to 0.

Success (200):

```json
{"activities":[{"id":"operation:7","type":"operation","status":"success","summary":"刷写操作","timestamp":1720000000,"ended_at":1720000030,"duration_ms":30000},{"id":"rom:9","type":"rom","status":"success","summary":"ROM 查询","timestamp":1720000040,"pd":"model","version":"v1","http_status":200}],"count":2,"limit":50,"offset":0}
```

### `GET /api/me/activities/operation%3A7`

Operation success (200):

```json
{"id":"operation:7","type":"operation","status":"success","summary":"刷写操作","timestamp":1720000000,"ended_at":1720000030,"duration_ms":30000,"steps_state":"unavailable","steps":[],"steps_message":"无更详细数据"}
```

ROM detail success (200):

```json
{"id":"rom:9","type":"rom","status":"success","summary":"ROM 查询","timestamp":1720000040,"pd":"model","version":"v1","http_status":200}
```

Malformed IDs are 400. Missing or another user's activity is deliberately indistinguishable and returns 404:

```json
{"message":"Not Found"}
```

## Session control

### `GET /api/me/sessions`

Success (200):

```json
{"count":1,"sessions":[{"id":"session-1","clientVersion":"1.0.0","ip_masked":"203.0.113.••","connectedAt":"2024-07-03T00:00:00.000Z","lastSeenAt":"2024-07-03T00:01:00.000Z","duration":"1 分钟","pendingExit":false,"pendingExitReason":null}]}
```

`ip_masked` is the only IP field. When a kick is pending, the session remains listed with `pendingExit: true`; the portal polls every two seconds, up to six attempts, and confirms success only when the session disappears.

### `POST /api/me/sessions/kick`

Request:

```json
{"id":"session-1"}
```

Success (200):

```json
{"ok":true}
```

Missing `id` is 400. An absent or unowned session returns the same 404 body as an absent activity.

## Password and logout

### `POST /api/me/password`

Request:

```json
{"current":"correct horse","newPassword":"new correct horse"}
```

Success (200; expires the cookie):

```json
{"ok":true,"reauthenticate":true}
```

The new password must be at least eight characters and differ from the current one (400 otherwise). An incorrect current password is 401. A concurrent credential change is 409 and expires the cookie. The client must immediately return to login on a successful response or any 401.

### `POST /api/logout`

Success (200; expires the cookie):

```json
{"ok":true}
```

## Future shared API Worker handoff

No client integration starts until the coordinating task implements and contract-tests this server behavior:

1. After password verification, if `api_users.token` starts with `revoked:`, compare-and-swap it to a fresh 32-byte hex token before creating a signed lease.
2. Never return a revoked marker.
3. Old-token heartbeat/auth returns 401 and the desktop exits.
4. No client integration starts until this server behavior is implemented and contract-tested by the coordinating task.

For future sanitized steps, a handoff migration may add an ownership-indexed table containing only generic phase, sequence, status, duration, retry count, exit code, safe error category, and remediation. It must not store user-visible partition, command, raw output, serial, path, token, or signed URL fields. This task does not edit the shared schema.
