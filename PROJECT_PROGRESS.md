# VivoKsu / NWFlash Project Progress

Last updated: 2026-09-02 (Asia/Shanghai)

## Executive status

- Functional implementation and automated validation: approximately **92%**.
- Release readiness: approximately **76%**.
- Administrator backend and console: implemented, reviewed, tested, and integrated.
- User backend and portal: implemented, reviewed, tested, and integrated.
- Existing five-leaf VMP runtime/release hardening: integrated and automated gates pass.
- Plan C structured trace client: Wave 1, Wave 2, producer core, process bridge, metadata-spool adapter, and initial Tauri gates are integrated.
- Real release remains blocked by manual VMProtect GUI work, signing, installer, installation, and real-device validation.

These percentages are engineering estimates, not test coverage. "Source-ready" or "gate-ready" does not mean that a binary has been protected, signed, installed, or approved for release.

## Canonical integration baseline

- Worktree: `C:\Users\17254\Desktop\存档\TOOL\VivoKsu 工具\.worktrees\integration-staging`
- Branch: `codex/integration-staging`
- Committed tip: `f565fc4 fix(trace): account for unrecoverable restart attempts` (includes `196a6c2` refresh/identity spawn-race closure).
- The integration worktree is clean at the recorded tip; Rust toolchain validation is still pending because `cargo`, `rustc`, and `rustup` are unavailable on this host.
- Do not reset, clean, checkout, or overwrite this worktree.

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

Location: `C:\Users\17254\Desktop\存档\TOOL\VivoKsu 工具\.worktrees\integration-staging`

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

## 2026-09-02 continuation checkpoint

Integrated commits on `codex/integration-staging`:

- `196a6c2 fix(tauri): close refresh and identity spawn race` adds a final admission check immediately before the real `ProcessExecutor::run` for discovery, ADB identity/battery reads, and ROOT OTA identity reads. The denied/skipped failure path remains redacted and does not expose serials, command lines, or remote URLs.
- `f565fc4 fix(trace): account for unrecoverable restart attempts` adds startup orphan sweeping for metadata-only attempts and durable loss tombstones with reason `restart_payload_unrecoverable`; tombstone reads accept both retention loss and restart loss.

Validation recorded for this continuation:

- `git diff --check` passed against the integrated tip.
- Rust gates (`cargo fmt --check`, `cargo test`, and `cargo clippy -- -D warnings`) could not be executed because `cargo`, `rustc`, and `rustup` are not installed or discoverable on this host.
- No deployment, signing, VMProtect GUI run, installer run, or real-device access was performed.

The restart behavior is intentionally honest: after a process restart, metadata alone cannot reconstruct the attested HTTP body. The implementation prevents unsafe replay and records durable loss; it does not claim recovery.

## Release blockers

No release claim is permitted until all of the following pass:

- Plan C Wave 1 final Ready YES review and clean commit (`0d65c0d`).
- Wave 2 spool/uploader final review and metadata tests are integrated; producer-to-metadata adapter is integrated, while true restart payload replay remains blocked on a protected payload vault/run-record.
- Producer/process/Tauri lifecycle wiring is complete for coordinator gates and mirror lifetime, but all seven operation classes still need actual sealed trace production adapters.
- Driver archive-to-elevated-consumer P0 final review and commit (`c6a6494`), with the unintegrated observer helper removed by `da2f22f`.
- Sixth VMP leaf real link/MAP/dumpbin verification (`verify-link-layout.ps1` and `test-contracts.ps1`) passed.
- Full Rust workspace fmt, clippy, and tests after the latest Tauri/producer integration (not run on 2026-09-02 because the Rust toolchain is unavailable; focused historical gates remain recorded separately).
- Full desktop unit/build and native WDIO E2E after integration.
- Cloudflare shared/admin/user Node, Workerd, typecheck, dry-run, and browser gates after integration.
- Manual VMProtect Lite 3.10.4 Build 2668 GUI protection and compiler-log review.
- Protected output verification, `VMProtectIsProtected`, CRC, Authenticode signing, NSIS packaging, installer install/uninstall, login/heartbeat, and real-device smoke tests.
- Explicit production deployment authorization. No deployment has been performed.

VMProtect SDK directory supplied by the user:

`C:\Users\mi\Downloads\VMProtect Lite v3.10.4 Build 2668`

## Workspace hygiene and recovery

Active worktrees:

- Outer: `codex/vmp-release-completion@5622642`
- `.worktrees/integration-staging` at the current integrated Tauri/Plan C tip.
- `.worktrees/f-tauri-wiring` retains the independently reviewed F wiring history through `c465480`.
- `.worktrees/plan-c-wave2-spool-uploader`, `.worktrees/planc-process-bridge`, and `.worktrees/planc-producer-core` retain clean source histories; their rebuildable targets were cleaned.
- `.worktrees/vmp-plan-c-sentinel-gate` at clean `db21a7f`.

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

1. Restore/install the pinned Rust toolchain, then run `cargo fmt --check`, focused tests, full workspace tests, and `cargo clippy -- -D warnings` from `src/Nwflash.Desktop/src-tauri`.
2. Run the non-deploying desktop/Cloudflare/native validation matrix on `f565fc4`.
3. Continue migrating the remaining legacy process observers and retire the active Tauri V1 reporter only after the V2 producer/projection path is complete.
4. If restart replay is a product requirement, design and implement a protected payload vault or attested recoverable run-record; otherwise keep the durable-loss semantics and document it as an explicit product boundary.
5. Create a new verified Git bundle, then perform the separately authorized manual VMProtect/signing/installer/device release sequence.

## Coordination note

Several final subagent turns were interrupted by the current agent-usage quota on 2026-08-29. Their filesystem work is preserved in the worktrees above. Treat interrupted reviews as incomplete; do not infer Ready status from an agent stopping.
