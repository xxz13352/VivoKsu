use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{
    CloudflareClient, RemoteAssetDownloader, RemoteAssetSpec, ResourceDownloadError,
    DEFAULT_APP_VERSION,
};
use tokio_util::sync::CancellationToken;
use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

const MAPPING_PATH: &str = "docs/migration-baselines/tauri-test-mapping.md";
const CSHARP_TESTS_PATH: &str = "tests/VivoKsu.App.Tests";

#[derive(Debug)]
struct MappingRow {
    coverage: String,
    evidence: Vec<String>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root should be available")
}

fn markdown_cell(cell: &str) -> &str {
    cell.trim().trim_matches('`').trim()
}

fn mapping_rows(markdown: &str) -> BTreeMap<String, MappingRow> {
    let mut rows = BTreeMap::new();

    for line in markdown.lines() {
        let cells = line.split('|').collect::<Vec<_>>();
        if cells.len() < 5 {
            continue;
        }

        let source = markdown_cell(cells[1]);
        if !source.ends_with("Tests.cs") {
            continue;
        }

        let coverage = markdown_cell(cells[2]).to_string();
        let evidence = cells[3]
            .split("<br>")
            .map(markdown_cell)
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let previous = rows.insert(source.to_string(), MappingRow { coverage, evidence });
        assert!(previous.is_none(), "duplicate mapping row for {source}");
    }

    rows
}

fn csharp_test_files(root: &Path) -> BTreeSet<String> {
    fs::read_dir(root.join(CSHARP_TESTS_PATH))
        .expect("C# test directory should be readable")
        .map(|entry| entry.expect("C# test entry should be readable"))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with("Tests.cs"))
        .collect()
}

fn is_approved_evidence_path(path: &str) -> bool {
    if Path::new(path)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }

    let rust_test = path.starts_with("src/Nwflash.Desktop/src-tauri/")
        && path.contains("/tests/")
        && path.ends_with(".rs");
    let frontend_test = path.starts_with("src/Nwflash.Desktop/src/")
        && (path.ends_with(".test.ts") || path.ends_with(".test.tsx"));
    let native_e2e =
        path.starts_with("src/Nwflash.Desktop/e2e-tests/specs/") && path.ends_with(".e2e.ts");
    rust_test || frontend_test || native_e2e
}

fn approved_evidence_root(path: &str) -> Option<(PathBuf, bool)> {
    let candidate = Path::new(path);
    if path.starts_with("src/Nwflash.Desktop/src-tauri/") && path.ends_with(".rs") {
        let mut test_root = PathBuf::new();
        for component in candidate.components() {
            test_root.push(component.as_os_str());
            if component.as_os_str() == "tests" {
                return Some((test_root, true));
            }
        }
    }
    if path.starts_with("src/Nwflash.Desktop/src/")
        && (path.ends_with(".test.ts") || path.ends_with(".test.tsx"))
    {
        return Some((PathBuf::from("src/Nwflash.Desktop/src"), false));
    }
    if path.starts_with("src/Nwflash.Desktop/e2e-tests/specs/") && path.ends_with(".e2e.ts") {
        return Some((PathBuf::from("src/Nwflash.Desktop/e2e-tests/specs"), false));
    }
    None
}

fn rust_source_has_test_marker(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("#[test]")
            || line.starts_with("#[tokio::test")
            || line.starts_with("#[cfg(test)]")
    })
}

fn validate_evidence_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    if !is_approved_evidence_path(path) {
        return Err(format!("unsupported evidence path: {path}"));
    }

    let (approved_root, is_rust) =
        approved_evidence_root(path).ok_or_else(|| format!("unsupported evidence path: {path}"))?;
    let approved_root = root
        .join(approved_root)
        .canonicalize()
        .map_err(|error| format!("missing approved test root for {path}: {error}"))?;
    let candidate = root
        .join(path)
        .canonicalize()
        .map_err(|error| format!("missing evidence {path}: {error}"))?;
    if !candidate.is_file() || !candidate.starts_with(&approved_root) {
        return Err(format!("evidence escapes its approved test root: {path}"));
    }
    if is_rust {
        let source = fs::read_to_string(&candidate)
            .map_err(|error| format!("unreadable Rust evidence {path}: {error}"))?;
        if !rust_source_has_test_marker(&source) {
            return Err(format!(
                "Rust evidence has no executable test marker: {path}"
            ));
        }
    }
    Ok(candidate)
}

#[test]
fn mapping_evidence_must_be_an_executable_test_file() {
    assert!(is_approved_evidence_path(
        "src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/device_session.rs"
    ));
    assert!(is_approved_evidence_path(
        "src/Nwflash.Desktop/src-tauri/tests/e2e/operation.rs"
    ));
    assert!(is_approved_evidence_path(
        "src/Nwflash.Desktop/src/pages/RootPage.test.tsx"
    ));
    assert!(!is_approved_evidence_path(
        "src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs"
    ));
}

#[test]
fn mapping_evidence_rejects_parent_directory_traversal() {
    assert!(!is_approved_evidence_path(
        "src/Nwflash.Desktop/src-tauri/tests/../../../../cloudflare/worker.rs"
    ));
}

#[test]
fn mapping_evidence_requires_an_on_disk_rust_test_marker() {
    const EVIDENCE: &str = "src/Nwflash.Desktop/src-tauri/tests/empty.rs";
    let root = temporary_directory("mapping-marker");
    let evidence_path = root.join(EVIDENCE);
    fs::create_dir_all(evidence_path.parent().expect("evidence parent"))
        .expect("evidence directory should be created");
    fs::write(&evidence_path, "pub fn helper() {}\n")
        .expect("non-test Rust fixture should be written");

    let result = validate_evidence_path(&root, EVIDENCE);

    assert!(
        result.is_err(),
        "Rust source without a test marker was accepted"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be available")
        .as_nanos();
    std::env::temp_dir().join(format!("nwflash-e2e-{label}-{nonce}"))
}

#[test]
fn every_csharp_test_file_has_one_source_grounded_mapping() {
    let root = repository_root();
    let mapping_path = root.join(MAPPING_PATH);
    let markdown = fs::read_to_string(&mapping_path).unwrap_or_else(|error| {
        panic!(
            "mapping manifest {} is missing or unreadable: {error}",
            mapping_path.display()
        )
    });
    let expected = csharp_test_files(&root);
    let rows = mapping_rows(&markdown);
    let actual = rows.keys().cloned().collect::<BTreeSet<_>>();

    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    let stale = actual.difference(&expected).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "mapping inventory mismatch; missing: {missing:?}; stale: {stale:?}"
    );

    for (source, row) in rows {
        assert!(
            matches!(row.coverage.as_str(), "direct" | "merged"),
            "{source} must use direct or merged coverage, got {:?}",
            row.coverage
        );
        assert!(
            !row.evidence.is_empty(),
            "{source} must cite at least one Rust, frontend, or native E2E test"
        );

        for evidence in row.evidence {
            validate_evidence_path(&root, &evidence)
                .unwrap_or_else(|error| panic!("{source} cites invalid evidence: {error}"));
        }
    }
}

#[tokio::test]
async fn cloudflare_authorization_failure_never_becomes_an_allowed_operation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/operation/authorize"))
        .and(header("Authorization", "Bearer session-token"))
        .respond_with(ResponseTemplate::new(503).set_body_string("authorization unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let client = CloudflareClient::new(server.uri(), DEFAULT_APP_VERSION);
    let error = client
        .authorize_operation("session-token", "Flashing", "刷写 boot")
        .await
        .expect_err("a failed authorization service must not allow flashing");

    assert_eq!(error.status_code(), Some(503));
}

#[tokio::test]
async fn failed_github_and_download_candidates_preserve_the_approved_asset() {
    let github = MockServer::start().await;
    let download_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("github unavailable"))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"unverified replacement"))
        .expect(1)
        .mount(&download_server)
        .await;

    let root = temporary_directory("download-failure");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let destination = root.join("approved-tool.exe");
    fs::write(&destination, b"approved").expect("approved fixture should be written");
    let downloader = RemoteAssetDownloader::new(
        None,
        Some(vec![download_server.uri()]),
        Some(Duration::from_secs(1)),
        Some(Duration::from_secs(1)),
    );
    let spec = RemoteAssetSpec::new("approved tool", github.uri()).with_expected_length(8);

    let error = downloader
        .download_to_file(&spec, &destination, None, &CancellationToken::new())
        .await
        .expect_err("all failed or unverified candidates must be rejected");

    assert!(matches!(
        error,
        ResourceDownloadError::AllCandidatesFailed { .. }
    ));
    assert_eq!(
        fs::read(&destination).expect("approved asset should remain readable"),
        b"approved"
    );
    assert_eq!(
        fs::read_dir(&root)
            .expect("fixture directory should be readable")
            .count(),
        1,
        "candidate staging files should be cleaned"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn cancellation_after_download_progress_removes_partial_staging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x5a; 128 * 1024]))
        .expect(1)
        .mount(&server)
        .await;

    let root = temporary_directory("download-cancellation");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let destination = root.join("cancelled-tool.exe");
    let cancellation = CancellationToken::new();
    let cancel_after_progress = cancellation.clone();
    let progress = move |_| cancel_after_progress.cancel();
    let downloader = RemoteAssetDownloader::new(
        None,
        Some(Vec::new()),
        Some(Duration::from_secs(1)),
        Some(Duration::from_secs(1)),
    );
    let spec = RemoteAssetSpec::new("cancelled tool", server.uri());

    let error = downloader
        .download_to_file(&spec, &destination, Some(&progress), &cancellation)
        .await
        .expect_err("cancellation after the first chunk should stop the download");

    assert!(matches!(error, ResourceDownloadError::Cancelled));
    assert!(!destination.exists());
    assert_eq!(
        fs::read_dir(&root)
            .expect("fixture directory should be readable")
            .count(),
        0,
        "canceled staging files should be removed"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
