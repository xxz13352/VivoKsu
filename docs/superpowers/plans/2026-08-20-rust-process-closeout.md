# Rust Process Runner Closeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the local Rust/Tauri workspace pass its integrity gate and finish process-pipe reader cleanup/error handling without changing device-facing behavior.

**Architecture:** The platform-tools test remains the source of truth for the immutable bundled executable; only its erroneous manifest text changes.  The Windows process runner keeps immediate reader threads and polling cancellation, but represents reader completion as `io::Result<Vec<u8>>`, joins every reader after the child is reaped, and reports reader failures only on ordinary command completion.

**Tech Stack:** Rust 2021, `std::process`, `std::thread`, `std::io`, `sha2`, Cargo, Vitest/Vite.

## Global Constraints

- Do not modify `cloudflare/**`, platform-tool binaries, device/flash/root workflows, release signing, installers, or real-device acceptance state.
- Preserve all pre-existing dirty and untracked worktree changes.  `process.rs` and the Tauri resource manifest are already untracked, so do not stage or commit either file from this shared worktree.
- Correct only the `fastboot.exe` digest text; do not replace, download, regenerate, or rehash a binary into the repository.
- A normal process exit with a failed or panicked reader returns a generic `DomainError::ExternalTool` output-read error; it must not return a partial `ProcessOutput` as success.
- Cancellation and timeout retain their existing Chinese error text even if a reader observes an error caused by intentional termination.
- Any cancellation/timeout path must terminate, reap, and join every active reader before it returns.
- All verification commands run locally with mock/fixture inputs only.  Do not run device, installer, signing, or WDIO actions that need a display or external approval.

---

### Task 1: Restore the Bundled Fastboot Integrity Declaration

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/resources/platform-tools/PLATFORM_TOOLS.SHA256:4`
- Test: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/platform_tools.rs:451-460`

**Interfaces:**
- Consumes: the existing `REQUIRED_PLATFORM_TOOLS` entry for `fastboot.exe` in `platform_tools.rs` and the existing `fastboot.exe` bytes.
- Produces: a four-entry manifest accepted by `verify_bundled_platform_tools` and `shipped_platform_tools_manifest_matches_binaries`.

- [ ] **Step 1: Reproduce the manifest regression before editing.**

Run:

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows platform_tools::tests::shipped_platform_tools_manifest_matches_binaries -- --exact --nocapture
```

Expected: FAIL with `内置 Android platform-tools 完整性校验失败。`.

- [ ] **Step 2: Confirm the exact source value and current binary digest.**

Run:

```powershell
Get-FileHash -Algorithm SHA256 -LiteralPath src/Nwflash.Desktop/src-tauri/resources/platform-tools/fastboot.exe
```

Expected digest:

```text
77d44117bf98b9716e2bd28fbd148ee34ab22e560fbb9c146fe39e4381bccea4
```

Confirm that it matches the `fastboot.exe` constant in `REQUIRED_PLATFORM_TOOLS`; the manifest currently has the transposed substring `d28bfd`.

- [ ] **Step 3: Apply the one-text-token correction.**

Replace line 4 of `PLATFORM_TOOLS.SHA256` with exactly:

```text
77D44117BF98B9716E2BD28FBD148EE34AB22E560FBB9C146FE39E4381BCCEA4  fastboot.exe
```

Do not change the other three manifest entries or any executable/DLL.

- [ ] **Step 4: Prove the integrity gate is green.**

Run the exact command from Step 1 again.

Expected: PASS with one unit test executed.

- [ ] **Step 5: Preserve the shared worktree boundary.**

Do not stage or commit the manifest because it was already an untracked user-owned file before this task.  Record the changed file and green test in the final verification summary.

### Task 2: Close Process Pipe Reader Completion and Failure Handling

**Files:**
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/process.rs:1-40, 181-335, 381-620`
- Test: the `#[cfg(test)]` module in that same file

**Interfaces:**
- Consumes: `ChildStdout`/`ChildStderr` values obtained from `child.stdout.take()` and `child.stderr.take()`.
- Produces: `PipeReader = Option<JoinHandle<io::Result<Vec<u8>>>>`, `collect_pipe(reader, stream_label) -> Result<Vec<u8>, DomainError>`, and a private reader-reap helper for cancellation/timeout paths.
- Preserves: `run_command_with_cancel`, `run_command_with_file_stdin_and_cancel`, and `run_command_with_file_stdout_and_cancel` public signatures and their current cancellation/timeout messages.

- [ ] **Step 1: Add focused failing reader-result tests.**

Add these test fixtures to `process.rs`'s test module:

```rust
struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture read failure"))
    }
}

struct PanickingReader;

impl Read for PanickingReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("fixture reader panic");
    }
}
```

Then add tests with these exact contracts:

```rust
#[test]
fn collect_pipe_rejects_reader_io_failures() {
    let error = collect_pipe(spawn_pipe_reader(Some(FailingReader)), "stdout")
        .expect_err("a reader I/O error must not become partial success");
    assert!(matches!(error, DomainError::ExternalTool(_)));
    assert!(error.to_string().contains("stdout"));
}

#[test]
fn collect_pipe_rejects_panicked_reader_threads() {
    let error = collect_pipe(spawn_pipe_reader(Some(PanickingReader)), "stderr")
        .expect_err("a panicked reader must not become an empty stream");
    assert!(matches!(error, DomainError::ExternalTool(_)));
    assert!(error.to_string().contains("stderr"));
}
```

Add the explicit join-lifecycle regression:

```rust
#[test]
fn reap_pipe_waits_for_reader_thread() {
    let completed = Arc::new(AtomicBool::new(false));
    let reader_completed = Arc::clone(&completed);
    let reader = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        reader_completed.store(true, Ordering::SeqCst);
        Ok::<Vec<u8>, io::Error>(Vec::new())
    });

    reap_pipe(Some(reader), "stderr");

    assert!(completed.load(Ordering::SeqCst));
}
```

Add cancellation and timeout integration regressions that keep a reader alive after a large write by appending a long `ping` after `type`:

```rust
#[test]
fn run_command_with_timeout_reaps_readers_after_large_output() {
    let (path, _) = write_bulk_fixture("pipe-timeout");
    let error = run_command_with_timeout(
        ProcessCommand::new(
            "cmd",
            [
                "/C".to_string(),
                format!(
                    "type \"{}\" & ping 127.0.0.1 -n 10 > nul",
                    path.to_string_lossy()
                ),
            ],
        ),
        Some(Duration::from_millis(100)),
    )
    .expect_err("the delayed child must time out");
    let _ = std::fs::remove_file(path);

    assert!(error.to_string().contains("命令执行超时"));
}

#[test]
fn run_command_with_cancel_reaps_readers_after_large_output() {
    let (path, _) = write_bulk_fixture("pipe-cancel");
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_source = Arc::clone(&cancelled);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        cancel_source.store(true, Ordering::SeqCst);
    });

    let error = run_command_with_cancel(
        ProcessCommand::new(
            "cmd",
            [
                "/C".to_string(),
                format!(
                    "type \"{}\" & ping 127.0.0.1 -n 10 > nul",
                    path.to_string_lossy()
                ),
            ],
        ),
        None,
        || cancelled.load(Ordering::SeqCst),
    )
    .expect_err("the delayed child must be cancelled");
    let _ = std::fs::remove_file(path);

    assert!(error.to_string().contains("运行被用户取消"));
}
```

- [ ] **Step 2: Run the focused tests to demonstrate the old contract is inadequate.**

Run:

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows collect_pipe_ -- --nocapture
```

Expected: compile failure or assertion failure because the current `collect_pipe` accepts one argument and silently returns an empty `Vec<u8>` for I/O errors or panics.

- [ ] **Step 3: Make reader results and joins explicit.**

Change the top-level import and helpers to use `std::io::{self, Read}` and the following shape:

```rust
type PipeReader = Option<JoinHandle<io::Result<Vec<u8>>>>;

fn spawn_pipe_reader<R>(stream: Option<R>) -> PipeReader
where
    R: Read + Send + 'static,
{
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).map(|_| buffer)
        })
    })
}

fn collect_pipe(reader: PipeReader, stream_label: &str) -> Result<Vec<u8>, DomainError> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) | Err(_) => Err(DomainError::ExternalTool(format!(
            "读取命令 {stream_label} 输出失败。"
        ))),
    }
}

fn reap_pipe(reader: PipeReader, stream_label: &str) {
    let _ = collect_pipe(reader, stream_label);
}
```

For normal completion, call `collect_pipe` for stdout and stderr before applying `?` to either result, so both thread handles are joined even if the first reader failed:

```rust
let stdout_result = collect_pipe(stdout_reader, "stdout");
let stderr_result = collect_pipe(stderr_reader, "stderr");
let stdout = stdout_result?;
let stderr = stderr_result?;
```

For each cancellation or timeout branch, call `child.wait()`, then `reap_pipe` for every reader before returning the existing `UserCancelled` or timeout `ExternalTool` error.  Apply the same pattern to the file-stdout variant's stderr reader.  If `try_wait()` itself errors after readers exist, terminate/reap the child and reap all readers before returning its existing waiting error.

- [ ] **Step 4: Run the new reader tests and existing large-output regressions.**

Run each command separately because Cargo accepts one test filter at a time:

```powershell
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows collect_pipe_ -- --nocapture
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows reap_pipe_ -- --nocapture
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows process::tests::run_command_drains_large_stdout_without_deadlocking -- --exact
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows process::tests::run_command_with_file_stdout_drains_large_stderr_without_deadlocking -- --exact
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows process::tests::run_command_with_timeout_reaps_readers_after_large_output -- --exact
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows process::tests::run_command_with_cancel_reaps_readers_after_large_output -- --exact
```

Expected: every command passes; normal completion retains complete pipe output, reader failures are explicit, and cancellation/timeout retain their existing text.

- [ ] **Step 5: Preserve the shared worktree boundary.**

Do not stage or commit `process.rs`: it existed as an untracked user-owned file before the task.  Leave the source change visible for the user's existing migration commit and include it in the final diff summary.

### Task 3: Reconcile Task 4 Documentation and Verify the Local Closeout

**Files:**
- Modify: `docs/index.md:69`
- Modify: `docs/architecture.md:412`
- Modify: `docs/project-architecture.md:215`
- Modify: `docs/architecture-tauri-migration.md:547`
- Modify: `src/Nwflash.Desktop/docs/rust-tauri-architecture.md:111`

**Interfaces:**
- Consumes: the green process-runner regressions from Task 2.
- Produces: documentation that accurately states Task 4 drains pipes concurrently, joins readers on all exit paths, and still distinguishes local verification from signing/real-device acceptance.

- [ ] **Step 1: Replace only the false Task 4 claims.**

Replace each statement that says concurrent stdout/stderr draining is "not implemented" or "a future remediation" with this factual boundary:

```text
进程 stdout/stderr 在子进程运行期间由独立 reader 并发排空；正常完成会在构造输出前回收 reader，取消或超时会在终止并回收子进程后回收 reader。大输出与 reader 失败回归测试覆盖该边界。
```

Keep the existing Task 5 SHA-256 language unchanged.  Do not change historical task checkboxes unrelated to Task 4.

- [ ] **Step 2: Inspect only the planned diff.**

Run:

```powershell
git diff --check -- docs/index.md docs/architecture.md docs/project-architecture.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
git diff -- docs/index.md docs/architecture.md docs/project-architecture.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
```

Expected: no whitespace errors and no content changes outside the five explicit Task 4 statements.

- [ ] **Step 3: Run all local validation gates.**

Run:

```powershell
cargo fmt --check --all
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-windows
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace --no-fail-fast
cargo clippy --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace -- -D warnings
npm --prefix src/Nwflash.Desktop run test
npm --prefix src/Nwflash.Desktop run build
```

Expected: all commands exit 0.  The frontend console line intentionally emitted by `AppSessionAuthFlow.test.tsx` is test-fixture output, not a test failure.

- [ ] **Step 4: Record the remaining external gates precisely.**

State in the final handoff that native WDIO, signed installer/VMProtect validation, and approved real-device matrix entries were intentionally not run because this task has no display session, signing credentials, installer target, or approved hardware.  Do not call those checks completed.

- [ ] **Step 5: Commit only the plan-owned documentation if safe.**

The five documentation files already contain user changes or are untracked, so do not stage or commit them.  The implementation-plan document itself may be committed separately after its self-review; record all other edits as uncommitted shared-worktree changes.
