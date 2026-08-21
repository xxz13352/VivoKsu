# NWflash Resource Provisioning Closeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the Rust external-resource provisioning slice with observable retry, integrity, archive-safety, and cancellation tests before later device workflows depend on it.

**Architecture:** `nwflash-infrastructure` remains the sole owner of remote resource acquisition and validation. `RemoteAssetDownloader` attempts an immutable candidate sequence into an attempt-local staging path, verifies length/hash before an atomic destination update, and makes cancellation terminal; provisioners consume this boundary without exposing arbitrary filesystem or network access to React.

**Tech Stack:** Rust 2021, Tokio, Reqwest, Wiremock, SHA-256, ZIP.

## Global Constraints

- Do not modify `cloudflare/**`.
- React must not fetch resources or retain credentials.
- Every successful resource write must be staged, integrity-checked, then committed.
- Candidate transport failure, including a per-candidate timeout, must advance to the next configured candidate; user cancellation must not advance.
- Test servers and temporary directories must be local and must not contact GitHub, Cloudflare, or a device.

---

### Task 1: Preserve Candidate Fallback After a Candidate Timeout

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/resource_downloader.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/resource_downloader.rs`
- Modify: `docs/architecture-tauri-migration.md`

**Consumes:** `RemoteAssetDownloader::download_to_file(&RemoteAssetSpec, &Path, Option<&ProgressSink>, &CancellationToken) -> Result<u64, ResourceDownloadError>` and a local Wiremock HTTP server.

**Produces:** A verified fallback contract: a timed-out candidate cannot prevent a later healthy mirror from producing the exact verified destination file. `ResourceDownloadError::CandidateTimeout` is retained only when every candidate times out or fails.

- [ ] **Step 1: Write the failing integration test**

```rust
#[tokio::test]
async fn falls_back_when_the_first_candidate_times_out() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(80)))
        .mount(&first)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"good"))
        .mount(&second)
        .await;

    let downloader = RemoteAssetDownloader::new(
        None,
        Some(vec![second.uri()]),
        Some(Duration::from_secs(1)),
        Some(Duration::from_millis(20)),
    );
    let spec = RemoteAssetSpec::new("fixture", first.uri()).with_expected_length(4);
    let destination = temporary_directory().join("fixture.bin");

    let written = downloader
        .download_to_file(&spec, &destination, None, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(written, 4);
    assert_eq!(std::fs::read(destination).unwrap(), b"good");
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure --test resource_downloader falls_back_when_the_first_candidate_times_out`

Expected: FAIL because `download_to_file` returns `ResourceDownloadError::CandidateTimeout` after the first candidate instead of attempting the healthy candidate.

- [ ] **Step 3: Implement the minimal fallback correction**

```rust
Err(_) => {
    last_error = Some(format!("下载候选源 {candidate} 超时"));
    let _ = self.try_delete_path(&staging);
    continue;
}
```

Keep terminal cancellation behavior unchanged. After the loop, surface `AllCandidatesFailed` with the final error detail; do not make a timed-out partial staging file visible at `destination`.

- [ ] **Step 4: Run the focused test and all infrastructure tests**

Run: `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml -p nwflash-infrastructure --test resource_downloader --test api_contract --test auth_contract --test paths --test version_contract`

Expected: PASS with no warnings in modified production files.

- [ ] **Step 5: Record the verified architecture boundary**

Document the ordered-candidate behavior and terminal cancellation distinction in `docs/architecture-tauri-migration.md`, then confirm no placeholder markers exist:

Run: `rg -n 'T[O]D[O]|T[B]D|implement[ ]later|fill[ ]in[ ]details' docs/superpowers/plans/2026-08-16-resource-provisioning-closeout.md`

Expected: no matches.

- [ ] **Step 6: Commit the self-contained slice**

```powershell
git add docs/superpowers/plans/2026-08-16-resource-provisioning-closeout.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/resource_downloader.rs src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/resource_downloader.rs
git commit -m "test(resources): cover timeout fallback"
```

## Plan Self-Review

- Coverage: the pending Task20 timeout/fallback behavior has one executable integration test and a source-level correction task.
- Scope: this plan does not change download UI, APIs, credentials, or resource formats.
- Type consistency: the test calls the existing public downloader contract and asserts only the destination-file outcome.
- Placeholder scan: no incomplete requirement markers are present.
