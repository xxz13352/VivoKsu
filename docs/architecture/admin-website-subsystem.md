# Administrator Website Subsystem

## 1. Purpose and ownership boundary

This subsystem owns the administrator website under `cloudflare/web`, the shared structured-trace V2 upload/query contract used by that website, and the D1 migrations needed to preserve the contract. It does not own the desktop producer, the user portal, release protection, signing, packaging, or deployment automation. Those systems may consume the frozen interfaces described here, but they must not silently change them.

## 2. Browser module tree

The browser entry point is `web/src/admin/index.html`. It loads only same-origin modules and styles:

- `app.js` owns authentication recovery, the shell, page lifecycle, account actions, status/alert surfaces, and route-to-page activation.
- `api.js` owns same-origin cookie requests, mutation CSRF headers, error-envelope normalization, sensitive query rejection, and the safe native export URL.
- `router.js` owns the bounded `?view=` URL contract, history state, scroll position, and focus return.
- `components.js` owns DOM-safe construction, page states, announcements, dialogs, cursor controls, menu behavior, and reusable focus helpers.
- `pages/overview.js`, `versions.js`, `users.js`, `sessions.js`, and `rom.js` each own one operational workspace.
- `pages/audit.js` owns the five-level persisted trace explorer and native NDJSON export trigger.
- `styles.css` owns the responsive dark visual system, accessibility tokens, local evidence overflow, reduced-motion behavior, and focus presentation.

Every page exposes `element`, `activate(route, signal)`, `deactivate()`, and `destroy()`. A route change aborts and destroys the previous page and its dialogs before the next page becomes authoritative.

## 3. Worker route split and static delivery

The Worker uses a closed build-time Text-module manifest. It serves exactly the root document, one stylesheet, four shared JavaScript modules, and six page modules. It never resolves a request path through the filesystem, dynamically imports a requested path, or falls back to an SPA asset. Static requests are answered before administrator seeding and therefore perform no D1 access.

Static HTML, CSS, and JavaScript use exact UTF-8 MIME types. Static, API, error, redirect, and NDJSON responses share `Cache-Control: no-store`, HSTS, `nosniff`, `DENY`, `no-referrer`, a restrictive permissions policy, COOP/CORP `same-origin`, and a CSP that permits only same-origin scripts, styles, fonts, connections, and images plus `data:` images. The CSP contains no `unsafe-inline` or `unsafe-eval`. Unknown, encoded, case-variant, backup, source-map, directory, or disallowed-method paths never expose a manifest body.

## 4. D1 V1/V2 data and retention

V2 persists operation runs, ordered events, and output chunks. Natural keys and triggers enforce ownership, parent existence, event quota, event sequence, declared chunk totals, chunk bounds, open-to-terminal completion, and immutable idempotent retries. Terminal V2 runs create one compatibility `usage_logs` projection keyed by the run ID. Internal `source_schema` and owner-bound `trace_run_id` provenance distinguish that projection from historical V1 rows, so a V1 event key may safely equal a V2 run ID. Dual-read queries suppress only tagged V2 projections, and retention deletes only the projection whose trace ID and owner match the expiring run.

Retention drains deterministic batches of at most 100 rows. At 30 days it deletes output and clears operational detail, permanently sealing any still-open run. At 90 days it removes event metadata. At 180 days it deletes the matching compatibility projection before deleting the V2 run. Partial indexes contain only rows awaiting detail cleanup, so already-clean history is not rescanned. Base, P0, and retention-stage migrations must all succeed before upload traffic is admitted.

## 5. Authentication boundaries

Desktop trace upload uses the shared API Worker, bearer authentication, and application-version gating. The signed short-lived lease returned by login independently gates sensitive local desktop operations; it is not the trace-upload credential. Administrator browsing and queries use the website Worker and an HttpOnly, Secure, SameSite=Lax administrator session cookie. Browser code never reads or stores that cookie. State-changing administrator requests add `X-Requested-With: XMLHttpRequest`; login is the sole endpoint whose credential failure remains local to the login form. A protected 401 clears the active shell, while 403 and 426 remain distinct errors and do not impersonate logout.

## 6. V2 data flow

The frozen flow is:

1. The client validates the V2 contract and streaming-redacts each complete logical stream before spool, HTTP serialization, chunking, or hashing.
2. The API applies bounded structural validation and request-local credential defense in depth.
3. One D1 batch rechecks ownership, parents, quotas, idempotency, bounds, completion, and retention seals before acknowledging item IDs.
4. Administrator endpoints query users, runs, run details, events, separately paginated stdout/stderr, overview data, ROM activity, and audited NDJSON export through keyset cursors.
5. The browser renders only persisted fields. V1 records stop at their available summary; incomplete V2 traces remain explicitly partial; exit code or browser heuristics never fabricate success or missing evidence.

## 7. Router, lifecycle, and UI states

The URL contains a whitelisted view and bounded, non-sensitive filters only. Unknown, duplicate, malformed, oversized, credential-shaped, command, or output values are discarded before they can remain in history or enter an API URL. Audit deep links carry opaque `trace_ref` and event identifiers; ROM links preserve only the documented filters and opaque cursor.

Page activation owns an AbortController and generation. Retry, polling, reload, and mutation continuations verify that ownership before updating the DOM. Navigation aborts the previous generation, clears dialogs, revokes one-time token presentation, stops polling, and restores a stable focus target and scroll position on Back. Loading, empty, partial, stale, unauthorized, error, retry, pending, success, and authoritative-reload failure states are explicit. Dangerous actions use a single-flight confirmation dialog whose initial focus is Cancel.

## 8. Verification layers and nondeployment gate

Node/Vitest tests cover contract parsing, redaction, API-client safety, router normalization, DOM primitives, page lifecycle, mutation state machines, trace hierarchy, output continuity, and migration semantics. Workerd tests execute the real Workers and D1 schema for upload, query, retention, migration, authentication, static assets, security headers, and failure paths. Playwright runs the production browser modules for login/session behavior, six workspaces, five audit levels, native download, keyboard/focus, axe, malicious fixtures, mutation failures, and 320/360/768/1024/1440 layouts.

The release gate is nondeploying: Node tests, all Workerd tests, strict TypeScript, the full administrator suite, API and website Wrangler dry-runs, dependency audit, syntax checks, and `git diff --check` must exit zero. Generated Wrangler and Playwright output is scoped to approved task paths and removed or quarantined after verification.

## 9. Deployment boundary and handoff

Production rollout is ordered: apply and verify the base, P0, and retention-stage migrations; deploy the API/schema side; verify upload/query compatibility; then deploy the static administrator UI. This task performs dry-runs only and does not deploy.

The desktop producer, seven-day per-user spool, replay/uploader lifecycle, and the additional VMP redaction/sentinel leaf are deferred to Plan C. Plan C must consume the frozen handoff, delete only explicitly accepted IDs, retain rejected or unacknowledged items, record durable loss when seven-day spool data expires, and request a versioned protocol change rather than silently widening V2.

Rollback is coordinated by the release operator:

| Failure point | Required action | Stop condition |
| --- | --- | --- |
| Any D1 migration fails | Keep V2 traffic disabled; preserve the failed database and logs; restore the verified pre-migration D1 recovery point or apply a reviewed forward repair. | Do not run later migrations or deploy either Worker. |
| Migrations succeed but API verification fails | Keep the administrator UI unchanged; roll the API Worker back only to a build proven compatible with the additive migrated schema, or restore D1 and the API build together while traffic is quiesced. | Never run unreviewed reverse SQL. |
| API succeeds but new UI fails | Keep the migrated schema and API; restore the prior website Worker/static manifest, which does not mutate V2 data. | Stop if the prior UI is not compatible with the current API responses. |
| V2 traffic has produced data | Preserve current D1 and prefer a forward repair or a schema-compatible Worker rollback. After the first accepted acknowledgement, an older D1 recovery point is not an ordinary rollback because clients may already have deleted acknowledged spool items. | Never restore behind an issued acknowledgement while claiming audit completeness. |

If disaster recovery unavoidably restores D1 behind issued acknowledgements, it requires explicit incident authorization and a recorded affected acknowledgement window. That window always remains recorded as permanent trace loss, and operators must not claim complete audit history; only a separately retained synthetic smoke trace may be reconciled independently. Production remains externally blocked until an authorized operator records a verified D1 recovery point, confirms the production database binding and custom domain, supplies the administrator seed secret only when a seed is actually required, runs post-deploy authentication/upload/query smoke checks, and records evidence that no rollback stop condition was reached. A dedicated synthetic smoke trace and its accepted IDs must be retained through the rollout observation window for reconciliation without changing normal client spool semantics.
