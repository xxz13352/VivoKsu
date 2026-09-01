# web.nwflash.cc.cd administrator console

`nwflash-web` is the Cloudflare Worker administrator console for Nwflash. It shares the `nwflash-db` D1 database with the API Worker, but uses a separate administrator session-cookie boundary. This directory owns administrator UI delivery, administrator APIs, V1/V2 trace queries, retention visibility, and operator actions. It does not deploy the desktop client or the user portal.

## Workspaces

The console exposes exactly six primary workspaces:

1. **概览** — authoritative user, session, operation, failure, trend, and recent-failure summaries.
2. **版本策略** — client version policy and destructive version changes with authoritative reload.
3. **用户管理** — API-user state, ban/delete, and one-time token rotation.
4. **在线会话** — visibility-aware polling and audited force-exit state.
5. **操作审计** — persisted user → run → event → command/output evidence, explicit V1 degradation, and audited NDJSON export.
6. **ROM 查询** — persisted ROM activity, complete safe URLs, failure reason, and opaque cursor navigation.

Change password and logout remain in the account menu and are not primary workspaces.

## Module tree

```text
web/
├─ wrangler.toml
├─ schema.sql
├─ migrate-usage-traces-v2*.sql
└─ src/
   ├─ index.ts                 # exact static manifest, administrator routes, session auth
   ├─ trace-v2-query.ts        # V1/V2 query, output, overview, ROM, audited export
   └─ admin/
      ├─ index.html
      ├─ styles.css
      ├─ app.js                # shell, auth recovery, page lifecycle
      ├─ api.js                # same-origin API client and safe export URL
      ├─ router.js             # bounded non-sensitive URL/history state
      ├─ components.js         # DOM-safe components, dialog, focus and page states
      └─ pages/
         ├─ overview.js
         ├─ versions.js
         ├─ users.js
         ├─ sessions.js
         ├─ audit.js
         └─ rom.js
```

The Worker imports browser files as build-time Text modules and serves a closed 12-path manifest. It does not scan directories, resolve request paths through the filesystem, or provide an SPA fallback. Static requests execute before administrator seed checks and do not touch D1.

## Security boundaries

- Administrator authentication uses an HttpOnly, Secure, SameSite=Lax cookie. Browser code does not read or persist it.
- Mutations send `X-Requested-With: XMLHttpRequest`; a protected 401 clears the shell, while login failure stays local to the form.
- All response classes use `no-store`, HSTS, `nosniff`, `DENY`, `no-referrer`, a restrictive permissions policy, COOP/CORP `same-origin`, and a CSP without `unsafe-inline` or `unsafe-eval`.
- UI values are created with DOM APIs and text nodes. Shared URL validation rejects network-path, mixed-slash, control-character, userinfo, and unsafe-scheme links.
- Page activations, retries, polling, reloads, and mutations are generation-bound and abort on navigation/logout. Dangerous actions are single-flight and require an accessible confirmation dialog.
- Trace V2 data is rendered only from persisted server responses. The browser never infers success from exit code or invents legacy evidence.

## Local nondeployment verification

Run from `cloudflare/`:

```powershell
npm test
npm run test:workerd
npm run typecheck
npm run test:admin:unit
npm run test:admin:workerd
npm run test:admin:browser
npm run test:admin
```

The administrator suite includes Vitest, real Workerd+D1, Playwright Chromium, axe WCAG 2 A/AA, keyboard/focus paths, malicious fixtures, mutation failures, and 320/360/768/1024/1440 layouts. Browser output is task-scoped under `web/.artifacts/admin-website/` and must be removed or quarantined after the gate.

Dry-run only:

```powershell
npm run dry-run:api
npm run dry-run:web
```

These commands do not authorize production deployment.

## D1 migration and rollout order

For structured trace V2, apply and verify migrations in this order:

1. `migrate-usage-traces-v2.sql`
2. `migrate-usage-traces-v2-p0.sql`
3. `migrate-usage-traces-v2-retention-stage.sql`

All three must succeed before V2 upload traffic is admitted. API/schema rollout precedes the static UI rollout. A 30-day open run is permanently detail-sealed; retention later removes event metadata and finally the V2 run plus its exact V1 compatibility projection.

Production secrets, remote migrations, deployment, and custom-domain changes require a separate authorized release operation. The website-stage work in this branch performs tests and Wrangler dry-runs only.

### Rollback and external blockers

Before production work, the release operator must record a verified D1 recovery point and confirm the production D1 binding, custom domain, and required secrets. If any migration fails, stop before admitting V2 traffic and either restore that recovery point or apply a reviewed forward repair. Do not continue to P0/retention-stage after an earlier migration failure.

If the migrated API fails verification, roll back only to a Worker version proven compatible with the additive schema. If only the modular UI fails, keep the schema/API and restore the previous website Worker. Before any V2 acknowledgement is issued, a coordinated D1+Worker recovery may use the verified pre-migration recovery point. After the first accepted acknowledgement, preserve current D1 and prefer a forward repair or compatible Worker rollback: clients may already have deleted accepted spool items, so restoring an older D1 point is not a lossless rollback.

An unavoidable disaster restore behind issued acknowledgements requires explicit incident authorization and a recorded affected acknowledgement window. Treat that window as permanent trace loss, do not claim complete audit history, and reconcile only separately retained synthetic smoke evidence. Never use unreviewed reverse SQL or delete/reinterpret V2 rows as a rollback shortcut.

Production remains blocked until the operator verifies administrator login/logout, one authorized synthetic V2 upload and query, static manifest/security headers, and the absence of rollback stop conditions. Retain that synthetic request evidence and its accepted IDs through the rollout observation window. This repository task does not hold production credentials or authorize those actions.
