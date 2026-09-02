# VivoKsu / NWFlash Project Progress

Last updated: 2026-09-02 (Asia/Shanghai)

## Executive status

- Functional implementation and automated validation: approximately **90%**.
- Release readiness: approximately **68%**.
- Administrator backend and console: implemented, reviewed, tested, and integrated — **not deployed**; production still serves the previous admin page.
- User backend and portal: implemented, reviewed, tested, and integrated — production serves an older build whose source is not in this repository.
- Existing five-leaf VMP runtime/release hardening: integrated and automated gates pass.
- Plan C structured trace client: Wave 1, Wave 2, producer core, process bridge, initial Tauri gates, attested metadata spool facade, producer sentinel-static boundary, process trace adapter, and the concrete metadata spool adapter are integrated through `01f55be`.
- Operation dispatch authority is integrated (`33dc9aa` / `4f062f5`); final Tauri spawn wiring remains.
- Stage B2 low-level spool contract tests are complete on `spool-lowlevel-contract@513cbbe`.
- Real release remains blocked by manual VMProtect GUI work, signing, installer, installation, real-device validation, and the Cloudflare deployment prerequisites in the deployment section below.

These percentages are engineering estimates, not test coverage. "Source-ready" or "gate-ready" does not mean that a binary has been protected, signed, installed, or approved for release.

## Canonical integration baseline

- Authoritative newest tip: `codex/vmp-release-completion@b5e6ccb test(ui): wait for terminal operation snapshot` (outer checkout, clean).
- Integration branch: `codex/integration-staging@01f55be feat(infrastructure): add concrete metadata spool adapter` (clean worktree).
- The outer branch is ahead of integration by the merge `4f64a07`, docs commits `65c4cab` / `5168022`, the trace fix `a25e6ab`, and the UI test fix `b5e6ccb`; `a25e6ab` is the only effective code delta and still needs to be merged back into integration.
- Stage B2 work lives on `spool-lowlevel-contract@513cbbe` (worktree `.worktrees/spool-lowlevel-contract`), branched from `b5e6ccb`.
- Do not reset, clean, checkout, or overwrite the integration worktree.
- Git environment warning: creating or deleting nested branch names (`refs/heads/<dir>/<name>`) via `git update-ref` / `git worktree add -b` is broken in this environment and can silently drop the whole `<dir>` directory of loose refs. Use flat branch names and move nested pointers by writing loose ref files directly.

Integrated validation after the administrator merge:

- Shared Cloudflare Node tests: 77 passed.
- Combined Workerd tests: 179 passed.
- Administrator unit tests: 135 passed.
- Administrator Workerd tests: 51 passed.
- Administrator Chromium tests: 31 passed.
- User portal UI tests: 34 passed.
- User Workerd tests: 27 passed.
- Both Cloudflare typecheck/dry-run gates passed.
- No production deployment was performed.

Latest non-deploying Rust gates on integration:

- `nwflash-application` producer focused tests: 12 passed.
- `nwflash-infrastructure` spool/uploader focused tests: 74 passed.
- `nwflash-windows` package tests: 71 passed.
- Tauri device/root OTA/identity/mirror focused tests: 10 + 11 + 5 + 8 passed.
- Domain/protection Wave 1 tests, compile-fail tests, clippy, fmt, and diff-check passed.

## Completed and integrated

### User backend and portal

- Final reviewed source: `codex/user-api-integration@ac96567`.
- Password-change/token-revocation races are closed with generation-bound CAS and session insertion.
- Low-version `426` versus authenticated `401` behavior is documented and tested.
- Cross-Worker tests use the same D1 binding and cover password change, token exchange, login, heartbeat, and lease verification.
- No confirmed P0/P1/P2 remained at final review.

### Administrator backend and console

- Final branch tip: `codex/admin-ops-console-completion@9d9e110`.
- Integrated by merge commit `8e458b2`.
- Structured trace V2 ingestion, query, retention, projection provenance, audit evidence, static routing, CSP, accessibility, responsive layouts, and operational workflows are implemented.
- Five administrator workspaces and audit views use authoritative APIs; no placeholder data remains.
- Final administrator release gate passed Node, Workerd, unit, Chromium, typecheck, dry-run, syntax, diff, and npm audit checks.
- Final visual evidence: `C:\Users\mi\Desktop\VivoKsu-quarantine\20260828\admin-task13-final-visual` (55 PNG files).
- No deployment was performed.

### Desktop supporting work

- File manager: `codex/file-manager-cleanup@7c3a189`; ADB-only operations, Fastboot UI disabled, reviewed and integrated.
- Firmware output provenance: `codex/firmware-output-provenance@5ab11db`; capability-based output folder handling, reviewed and integrated.
- Native WDIO determinism: `f249425`; seven native spec files passed, including interaction and visual suites.
- Existing VMP runtime/release hardening tip: `codex/vmp-release-completion@5622642`.
- Existing VMP work includes protection context, CRC/admission gates, signed lease checks, five protected synchronous leaves, release evidence chaining, reparse defenses, fixed SDK identity, and capability graph gates.

## Plan C Wave 1: domain and credential boundary

Location: `C:\Users\mi\Desktop\VivoKsu 工具\.worktrees\integration-staging`

Wave 1 source files (now committed):

- `src/Nwflash.Desktop/src-tauri/Cargo.lock`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/Cargo.toml`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/src/lib.rs`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/src/trace.rs` (new)
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/Cargo.toml`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/lib.rs`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/vmp.rs`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-protection/src/trace_redaction.rs` (new)

Implemented so far:

- Strict inbound V2 domain DTO validation and bounded ACK/error decoding.
- Raw inbound DTOs have no outbound serializer or public proof-trait path.
- Protection owns concrete opaque redacted command/run/event/logical-stream/chunk/upload types.
- Streaming credential removal covers Authorization, Bearer, Basic/Digest, Cookie/Set-Cookie, CLI flags, URL userinfo/query secrets, PEM/private keys, and registered exact secrets.
- Structured command redaction covers program, argv, display command, working directory, paths, URLs, and serial.
- UTF-8-safe output chunking, chunk hashes, redaction counts, bounded JSON encoding, zeroizing buffers, and a sixth VMP trace credential sentinel source leaf are present.
- High-risk output cannot be sealed as a successful logical stream.

Latest frozen test evidence:

- Domain: 43 passed during final Wave 1 freeze.
- Protection: 64 passed, 3 existing nested-build tests ignored.
- Compile-fail API tests: 5 passed.
- Clippy with `-D warnings`, rustfmt, and diff-check passed.

Current review status: **Wave 1 Ready YES and integrated as `0d65c0d`; final probe alignment is `880da47` plus `c3c185c`.**

Wave 1 review closure:

- Exact secrets containing CR/LF are rejected during `ExactSecretSet` construction.
- An invalid/non-private PEM begin marker no longer hides a later real private-key begin marker on the same line.
- Unquoted assignment and CLI credential values include `#` suffixes in the removed value; URL fragment handling remains isolated to the URL parser.
- Duplicate exact-secret inputs are deduplicated before count/byte quota checks.
- Focused redaction tests passed through the final 31-test suite; the complete protection crate passed 64 tests with 3 existing nested-build tests ignored and 5 compile-fail tests passed. Clippy `-D warnings`, fmt, and diff-check passed.

Wave 1 final closure:

- EOF/session provenance is closed by `TraceOutputSession::from_reader`, private scanner finish/chunk/upload constructors, and compile-fail API tests.
- JSON-escaped payloads are dynamically batched to the actual 1 MiB wire limit.
- The sixth VMP leaf passes the real unprotected release probe link/MAP/dumpbin gate; manual Lite GUI protection and protected-runtime checks remain pending.

Required architecture before integration:

- A process-output session must own the stdout/stderr reader, scanner, EOF state, event manifest, and chunk queue.
- Only the terminal session result may construct a sealed upload attempt.
- Spool/uploader must not accept raw DTOs, validated DTOs, trait objects, standalone redacted fragments, or caller-computed hashes.
- Request batching must preserve stable entity IDs while issuing a fresh wire upload ID for each attempt.

## Plan C Wave 2: spool and uploader

Worktree: `C:\Users\mi\Desktop\VivoKsu 工具\.worktrees\plan-c-wave2-spool-uploader`

- Branch: `codex/plan-c-wave2-spool-uploader`
- Committed planning tip: `0914e31 docs(plan-c): define wave2 spool uploader`
- Final implementation was integrated as `fdcac85 feat(trace): add durable owner-scoped spool`; later path/ACK hardening is included in that integration chain.
- The source worktree may be retained for recovery, but its target was cleaned after commit to recover disk space.

Implemented/tested state:

- Spool/uploader final focused tests: 74/74 on the integrated metadata state machine.
- Metadata-only design; raw/redacted payload adapters and dynamic proof types were removed.
- Owner scope is canonical username hash plus login generation.
- Attempts use opaque sealed upload references, item revisions, single-use dispatch, revision CAS, and durable remediation/loss state.
- Delayed ACKs cannot delete a newer terminal revision.
- Parent acceptance does not cascade-delete unacknowledged children.
- `401/403` pauses the affected owner, `426` is a client-version gate, and retry/backoff behavior is separated from item rejection.

Remaining Wave 2 integration work:

- Bind actual `TraceOutputSession` sealed attempts from the producer to the metadata spool/uploader. The metadata state machine intentionally accepts no raw payload.
- Add the full crash matrix at the producer-to-spool boundary, including re-seal after `credential_rejected` and seven-day durable loss.

## Stage B2: TraceSpoolStore low-level contract tests (2026-09-02)

Worktree: `.worktrees/spool-lowlevel-contract`, branch `spool-lowlevel-contract` (flat name on purpose; nested names cannot be created in this environment), based on `b5e6ccb`.

- `efdab90 test(trace): pin low-level spool ack contract` adds the first direct coverage for `apply_validated_ack_cas`, which previously had none inside `trace_spool.rs`:
  - `peek_due_attempts` performs no expiry, no recovery, and no persist — the manifest is byte-identical afterwards and no loss diagnostic is written, while the same fixture under `expire()` really does emit one retention loss.
  - `apply_validated_ack_cas` rejects duplicate ack keys, accepted/rejected overlap, a missing dispatched member, an unknown member, and a credential rejection aimed at a non-chunk entity; every rejection leaves the manifest byte-identical.
  - A mixed ACK whose manifest replace is injected to fail keeps the old manifest byte-for-byte and does not partially accept the run item; after reopening, all three items remain at their original revision and return to the reseal outbox.
- `513cbbe docs: record stage B2 completion and environment gates` records the same evidence in `TERRA_IMPLEMENTATION_PLAN_2026-08-31.md` section 13.

Gates: `nwflash-infrastructure` full test set passed with 0 failures (lib 111/111 plus every integration test binary), clippy `--all-targets -D warnings`, `cargo fmt --check`, and `git diff --check` all pass.

Environment note: the local `HTTP_PROXY` / `HTTPS_PROXY` point at `http://127.0.0.1:2717`. `trace_http::tests::debug_and_errors_never_expose_token_body_or_response_ids` connects to `http://127.0.0.1:9` and expects the connection to fail; with the proxy present the request is routed to the proxy and returns `502`, so the test fails even though the code is correct. Clear the proxy environment variables before running Rust tests.

## Process observation and driver installation security

Worktree: `C:\Users\mi\Desktop\VivoKsu 工具\.worktrees\planc-process-bridge`

- Branch: `codex/planc-process-bridge`
- Baseline tip: `8e458b2`.
- Driver integrity was integrated as `c6a6494` plus the process compatibility cleanup `da2f22f`.
- Process observation was integrated as `c3c185c` and the allowlist fix `b91cd93`; the source worktree target was cleaned after commit.

Security issue being fixed:

- P0: a current-user-writable bundled driver archive could previously be replaced and then consumed by elevated `pnputil` after UAC approval.
- The fixed release digest is `22FA20B21004A7AE76668716EF51E22FD9E8E9EEEA226A035AD23157441B60EA` for the 12,199,572-byte archive.

Final integrated driver/process design:

- Removes the public extractor/digest injection seam from the production installer.
- Uses the fixed archive and a same-handle verified snapshot.
- Derives the expected file/INF list from authenticated archive entries, not from a later scan of a writable directory.
- Creates expected files exclusively, keeps deny-write/delete handles, records file identity/length/hash, rejects extras and reparse paths, and passes exact canonical INF paths without wildcard or `/subdirs`.
- Revalidates path identity before UAC and holds guards through elevated completion.
- Focused driver installer tests passed 11/11; process tests passed 33/33; the integrated nwflash-windows package passed 71/71. Independent reviews found no P0/P1; residual P2 boundaries are documented in the code and release notes.

The process observer remains intentionally conservative: it has bounded non-blocking observation, bounded output/loss, bounded reader reaping, fixed System32 taskkill, and explicit `TerminationUnconfirmed` when Job Object-grade descendant proof is unavailable. It still needs producer wiring and final operation-entry migration.

## Sixth VMP trace leaf gate

Worktree: `C:\Users\mi\Desktop\VivoKsu 工具\.worktrees\vmp-plan-c-sentinel-gate`

- Branch/tip: `codex/vmp-plan-c-sentinel-gate@db21a7f`
- Commit: `db21a7f build(vmp): gate trace credential sentinel leaf`
- Worktree was clean after commit.
- Independent delta review: P0/P1/P2 = 0/0/0, Ready YES for the gate patch.

The patch:

- Expands the exact protected marker contract from five to six.
- Adds `nwflash_protection_trace_credential_sentinel` / `NWFlash.TraceCredentialSentinel` in Ultra mode.
- Keeps the VMProtect SDK import set exactly eight.
- Requires one MAP symbol and exactly one BeginUltra/End pair in correct order, with no early return in the protected region.
- Adds the sixth leaf to prepared/manual-review/accepted evidence and fails closed when it is missing.
- Keeps the link-probe result live with `black_box`.

Conditional boundary:

- The gate patch is integrated and its link probe compiles against the final Wave 1 API.
- Real unprotected Rust link, six-symbol MAP/dumpbin, and fixed SDK contract gates pass; VMProtect compiler log, protected runtime, CRC, signing, installer, and device gates remain pending.
- This commit does not prove that a production binary is VMProtect-protected.

## Operation producer and lifecycle work still required

Producer core is implemented and integrated as `328cee8 fix(producer): enforce reserved trace sequence order`.

Required state order (core API is tested; production command adapters remain):

1. Validate and freeze `{username, login_generation, epoch, sequence}`.
2. Generate a UUIDv7 run ID.
3. Persist an open run before remote operation authorization.
4. Record remote denial as an authorization event plus terminal denied outcome.
5. Record command/stage/partition/verification events through one run handle.
6. Persist terminal event and terminal run before returning the coordinator to idle.
7. Recover abandoned open runs as aborted plus durable loss.

Producer focused tests passed 12/12, including reserved-sequence ordering, sealed-upload consumption, sink failure retryability, stale identity rejection, and all seven operation kinds. It is not yet the sink used by every Tauri command.

Seven operation classes to cover:

- Discovering
- Rebooting
- Installing
- Transferring
- Hashing
- Flashing
- Mirroring

Known bypasses still requiring producer/observer adapters:

- Manual/automatic device discovery and `root_ota_check` now have coordinator admission gates and safe denied/skipped audit records; their process output still needs the producer observer adapter.
- Direct scrcpy spawn/taskkill now stays under a long-lived coordinator permit and suppresses unredacted console output; sealed child stdout/stderr trace ingestion remains.
- Elevated driver execution has archive/UAC integrity gates; observed start/final trace integration remains.
- Direct `run_command` paths in files, partitions, quick flash, root, firmware extraction, and safe flash still use legacy/discarding observers in several read/command paths.
- Internal taskkill/timeout/cancel termination evidence.
- Old V1 usage reporter tail loss, cross-user queue reuse, and shutdown timeout loss.

The durable V1 compatibility bridge is integrated as `0ca1ab1` plus `b87d9b5` and has 4/4 focused tests, but the Tauri `UsageLogReporter` is still active. It must be retired only after all producer adapters and the V2 projection path are live.

## Cloudflare production deployment status (probed 2026-09-02, read-only)

All four Worker domains are live. "No production deployment was performed" in the sections above refers to the recent integration rounds only; an earlier production deployment exists but is behind the current source.

| Worker | Domain | Observed state |
|---|---|---|
| `nwflash-rom` (API) | `api.nwflash.cc.cd` | `/health` returns 200. Legacy routes are live: `/api/login`, `/api/heartbeat`, `/api/usage/logs`, `/api/operation/authorize` (empty POST returns 401/400, so the routes exist). **Not deployed:** `/api/security/pins`, `/api/integrity/report`, and `/api/usage/traces/v2` all return 404 — the VMP integrity reporting path and the entire trace V2 ingest backend are absent from production. |
| `nwflash-web` (admin) | `web.nwflash.cc.cd` | Serves the **old** page "Nwflash · 控制中心", which corresponds to `cloudflare/web/src/admin.html` (deleted in `f8fe8cf`). The new "Nwflash · 运营控制台" console (`web/src/admin/index.html`) with the audit/overview/version workspaces is **not deployed**. |
| `nwflash-user` (portal) | `user.nwflash.cc.cd` | Serves "Nwflash · 我的账户" with my-logs / online-sessions / password-change. Source located (2026-09-02 correction): the live page is byte-identical to `cloudflare/user/src/user.html` at `2eea36e fix(user): 用户门户对抗审查修复(11 项)`. That file was created in `0aa0907`, deleted in `8c9175a docs(user): freeze personal ops handoff`, and superseded by `b34a362 feat(user): build personal ops portal` (`portal/index.html`, "Personal Ops") — the replacement has never been deployed. Recover with `git show 2eea36e:cloudflare/user/src/user.html`. An earlier probe wrongly reported "no source in the repository" because the history walk was aborted by a broken ref under `refs/codex/`; use `--branches` instead of `--all` when searching this repository's history. |
| `nwflash-site` (website) | `nwflash.cc.cd` | "奶蛙Flash · Nwflash", consistent with `website/src/index.html`. |

Verification limits: the `cloudflare/` directories have no `node_modules` (removed to reclaim disk), there is no local wrangler CLI, no `~/.wrangler` login state, and no `CLOUDFLARE_*` credentials, so remote deployment IDs, versions, and timestamps could not be queried. Everything above is inferred from HTTPS behavior plus source/history comparison — no deployment, secret write, or D1 mutation was performed.

Deployment blockers, in order:

1. Configure the production secret `SESSION_SIGNING_PRIVATE_KEY_PKCS8` (and `VOTA_API_TOKEN` if not yet present); `npm run deploy` preflight fails closed without it.
2. Resolve the V2 A/B keyring acceptance gate from `sidecar-ed25519-key-rotation.md` before the first authorized production deployment of the hardened client/API.
3. Decide the fate of the undocumented live `user.nwflash.cc.cd` build before replacing it: either recover its source or accept the repository's "Personal Ops" portal as the replacement.
4. Apply `web/schema.sql` additions (`session_leases`, `integrity_event_claims`, `integrity_events`, `integrity_rate_limits`) and the trace V2 migrations to remote D1 before deploying the new API/admin.
5. Only then deploy `nwflash-rom` + `nwflash-web` (+ `nwflash-user`) and re-run the documented post-deploy checks.

## Release blockers

No release claim is permitted until all of the following pass:

- Plan C Wave 1 final Ready YES review and clean commit (`0d65c0d`) — integrated.
- Wave 2 spool/uploader final review and metadata tests — integrated through `01f55be`; the crash/restart loss matrix and the completed-attempt ledger (stage B3/C) remain.
- Producer/process/Tauri lifecycle wiring is complete for coordinator gates and mirror lifetime, but all seven operation classes still need actual sealed trace production adapters.
- Driver archive-to-elevated-consumer P0 final review and commit (`c6a6494`), with the unintegrated observer helper removed by `da2f22f` — integrated.
- Sixth VMP leaf real link/MAP/dumpbin verification (`verify-link-layout.ps1` and `test-contracts.ps1`) passed.
- Full Rust workspace fmt, clippy, and tests on the newest tip (`b5e6ccb` / the merged B2 branch); the last full run predates the outer-branch merge.
- Full desktop unit/build and native WDIO E2E after integration.
- Cloudflare shared/admin/user Node, Workerd, typecheck, dry-run, and browser gates after integration.
- The Cloudflare deployment prerequisites listed in the deployment section above (signing secret, V2 keyring gate, D1 migrations).
- Manual VMProtect Lite 3.10.4 Build 2668 GUI protection and compiler-log review.
- Protected output verification, `VMProtectIsProtected`, CRC, Authenticode signing, NSIS packaging, installer install/uninstall, login/heartbeat, and real-device smoke tests.
- Explicit production deployment authorization.

VMProtect SDK directory supplied by the user:

`C:\Users\mi\Downloads\VMProtect Lite v3.10.4 Build 2668`

## Workspace hygiene and recovery

Active worktrees:

- Outer: `codex/vmp-release-completion@b5e6ccb` (clean).
- `.worktrees/integration-staging` at `01f55be` (clean).
- `.worktrees/spool-lowlevel-contract` at `513cbbe` (clean) — stage B2.
- `.worktrees/refresh-spawn-race-fix` at `35e0248` retains uncommitted stage E work (`device.rs`, `device_identity.rs`, `root_ota.rs`, +711/−46); do not reset or clean it.
- `.worktrees/operation-dispatch-guard` (`8ca8731`), `.worktrees/planc-producer-spool-adapter` (`c420b77`), `.worktrees/process-trace-adapter` (`11d796a`), `.worktrees/producer-sentinel-static` (`ba7e81c`), and `.worktrees/sealed-spool-facade` (`d62d649`) are clean; their content is already integrated under different commit SHAs (verified by patch-id equivalence).
- `.worktrees/planc-producer-core` is an empty leftover directory held by a Windows handle; it is not a Git worktree.

Recovery bundles:

- `C:\Users\mi\Desktop\VivoKsu-backups\VivoKsu-20260826-pre-integration.bundle`
  - SHA-256: `82EC55D464576DC623981615E3FBB3F19BAE4A9DCD57562851BBD901A0F96062`
- `C:\Users\mi\Desktop\VivoKsu-backups\VivoKsu-20260829-admin-final-pre-merge.bundle`
  - SHA-256: `FC89C14A6E9BA812F3A7F6FBF8BD420BBFC8C86106276EA327A0D09E64782FC6`
- `C:\Users\mi\Desktop\VivoKsu-backups\VivoKsu-20260829-post-admin-integration.bundle`
  - SHA-256: `A2A51757EC88C96D772B512C9115D6AE5B60CF3853B37157BBE2C4BD38731F59`
- `C:\Users\mi\Desktop\VivoKsu-backups\VivoKsu-20260830-planc-integration.bundle`
  - SHA-256: `0E06AD60A41F98F5C88AFDB25ACEE27C752CB56C19FC149BB7459C4FF84551E1`
  - Size: 350,823,182 bytes; `git bundle verify` passed with complete history.

Quarantine data is preserved under `C:\Users\mi\Desktop\VivoKsu-quarantine`. Do not delete it without separate verification and authorization.

Workspace rules:

- Do not run `git reset --hard`, `git clean`, broad worktree prune, or recursive deletion.
- Do not touch another worktree's `target`, `.wrangler`, index, or uncommitted files.
- Use focused tests while disk space is constrained.
- Only clean a worktree's build target after its source is committed, tests/review evidence is recorded, no process is using it, and the exact resolved target path is verified.
- Do not deploy, sign, install, run VMProtect GUI, or access a real device without explicit authorization.

## Recommended next execution order

1. Merge `a25e6ab` (and the outer docs commits) back into `codex/integration-staging`, then merge `spool-lowlevel-contract` so stage B2 lands on the canonical branch.
2. Stage B3: add the bounded completed-attempt ledger/tombstone so the 256-attempt ceiling no longer permanently fails registration for an owner generation; include the 300+ attempt stress tests and keep duplicate detection across active attempts and the ledger.
3. Stage C: the restart loss closed loop — persist a process/build epoch per registered attempt, treat pending metadata from a previous epoch as orphans, write durable loss tombstones atomically, and cover the seven-crash matrix.
4. Stage D3: wire the producer → metadata spool → sentinel-attested HTTP transport at runtime with a static identity provider and a bounded shutdown deadline.
5. Stage E: finish the final spawn authority — rebase the uncommitted `refresh-spawn-race-fix` work onto the newest integration tip, apply `with_running_dispatch` at the real device/root OTA OS spawn points, then review and commit.
6. Stage F: run the full non-deploying gate matrix on the final tip (Rust workspace, desktop frontend, Cloudflare, PowerShell/VMP gates) and cut a new verified Git bundle.
7. Only after explicit authorization, the Cloudflare deployment sequence in the deployment section, followed by the manual VMProtect/signing/installer/device release sequence.

## Coordination note

Several final subagent turns were interrupted by the current agent-usage quota on 2026-08-29. Their filesystem work is preserved in the worktrees above. Treat interrupted reviews as incomplete; do not infer Ready status from an agent stopping.
