use std::io::Cursor;

use nwflash_domain::{TraceEventKindV2, TraceEventStatusV2, TraceId, TraceOutputStreamV2};
use nwflash_infrastructure::{
    CloudflareClient, SecretToken, TraceMetadataOwnerScope, TraceMetadataSpoolAdapter,
    TraceMetadataSpoolEntity, TraceMetadataSpoolError, TraceMetadataUploadOutcome,
    DEFAULT_APP_VERSION,
};
use nwflash_protection::{
    ExactSecretSet, SentinelAttestedTraceUpload, TraceEventText, TraceOutputSession,
};
use tempfile::TempDir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn owner(seed: u8, generation: u64, build: u8) -> TraceMetadataOwnerScope {
    TraceMetadataOwnerScope::try_new([seed; 32], generation, [build; 32]).unwrap()
}

fn event_attempts(
    run_id: TraceId,
    event_id: TraceId,
    sequence: u64,
    output: Vec<u8>,
) -> Vec<SentinelAttestedTraceUpload> {
    let mut reader = Cursor::new(output);
    let secrets = ExactSecretSet::try_new(["adapter-secret"]).unwrap();
    let session = TraceOutputSession::from_reader(
        event_id,
        TraceOutputStreamV2::Stdout,
        &mut reader,
        &secrets,
    )
    .expect("EOF must be consumed before sealing");
    let event = TraceEventText {
        event_id,
        run_id,
        sequence,
        kind: TraceEventKindV2::Command,
        step_name: "trace",
        partition_name: None,
        status: TraceEventStatusV2::Success,
        started_at_ms: 1,
        ended_at_ms: Some(2),
        duration_ms: Some(1),
        command: None,
        exit_code: Some(0),
        verification: None,
        device_state: None,
        retry_safe: Some(true),
        remedies: &[],
        error_class: None,
        error_code: None,
        error_message: None,
    };
    session
        .into_event_upload_attempts(event, &secrets)
        .expect("sealed event")
        .into_iter()
        .map(|upload| {
            SentinelAttestedTraceUpload::try_from(upload).expect("sentinel-attested upload")
        })
        .collect()
}

fn json_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn concrete_attested_metadata_registers_without_persisting_payload() {
    let root = TempDir::new().unwrap();
    let run_id = TraceId::try_new_v7().unwrap();
    let event_id = TraceId::try_new_v7().unwrap();
    let attempts = event_attempts(
        run_id,
        event_id,
        1,
        b"before adapter-secret after\n".to_vec(),
    );
    let upload_ids = attempts
        .iter()
        .map(SentinelAttestedTraceUpload::upload_id)
        .collect::<Vec<_>>();
    let scope = owner(0x11, 7, 0x22);
    let adapter = TraceMetadataSpoolAdapter::open(root.path(), scope.clone()).unwrap();

    let registered = adapter
        .register_attested_event_batch(run_id, 1, &attempts)
        .unwrap();

    assert_eq!(registered.owner(), &scope);
    assert_eq!(registered.attempts().len(), attempts.len());
    assert_eq!(
        registered
            .attempts()
            .iter()
            .map(|attempt| attempt.upload_id())
            .collect::<Vec<_>>(),
        upload_ids
    );
    assert!(registered.attempts().iter().any(|attempt| {
        attempt
            .items()
            .iter()
            .any(|item| item.entity() == TraceMetadataSpoolEntity::Event)
    }));
    assert!(registered
        .attempts()
        .iter()
        .all(|attempt| { attempt.items().iter().all(|item| item.trace_id() == run_id) }));
    assert_eq!(adapter.pending_attempt_count(0).unwrap(), attempts.len());

    let persisted = json_files(root.path())
        .into_iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<String>();
    for forbidden in [
        "before",
        "adapter-secret",
        "after",
        "Authorization",
        "https://",
    ] {
        assert!(!persisted.contains(forbidden), "spool leaked {forbidden}");
    }
}

#[test]
fn event_attempt_batch_is_one_transaction_with_zero_partial_on_error() {
    let root = TempDir::new().unwrap();
    let run_id = TraceId::try_new_v7().unwrap();
    let first_event = TraceId::try_new_v7().unwrap();
    let second_event = TraceId::try_new_v7().unwrap();
    let mut mixed = event_attempts(run_id, first_event, 1, b"first\n".to_vec());
    mixed.extend(event_attempts(
        run_id,
        second_event,
        2,
        b"second\n".to_vec(),
    ));
    let scope = owner(0x31, 9, 0x41);
    let adapter = TraceMetadataSpoolAdapter::open(root.path(), scope.clone()).unwrap();

    assert!(matches!(
        adapter.register_attested_event_batch(run_id, 1, &mixed),
        Err(TraceMetadataSpoolError::InvalidEventBatch)
    ));
    assert_eq!(adapter.pending_attempt_count(0).unwrap(), 0);
    drop(adapter);

    let reopened = TraceMetadataSpoolAdapter::open(root.path(), scope).unwrap();
    assert_eq!(reopened.pending_attempt_count(0).unwrap(), 0);
}

async fn mount_status(server: &MockServer, status: u16) {
    let body = match status {
        401 => serde_json::json!({
            "ok": false,
            "error": {
                "code": "TRACE_UNAUTHORIZED",
                "message": "unauthorized",
                "request_id": "123e4567-e89b-12d3-a456-426614174000"
            }
        }),
        403 => serde_json::json!({
            "ok": false,
            "error": {
                "code": "TRACE_FORBIDDEN",
                "message": "forbidden",
                "request_id": "123e4567-e89b-12d3-a456-426614174000"
            }
        }),
        426 => serde_json::json!({
            "error": "update required",
            "code": "UPDATE_REQUIRED",
            "latest": "2.0.0",
            "min": "2.0.0",
            "download_url": "https://example.test/update"
        }),
        _ => unreachable!(),
    };
    Mock::given(method("POST"))
        .and(path("/api/usage/traces/v2"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn unauthorized_and_forbidden_pause_only_the_dispatched_owner() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        mount_status(&server, status).await;
        let root = TempDir::new().unwrap();
        let run_id = TraceId::try_new_v7().unwrap();
        let event_id = TraceId::try_new_v7().unwrap();
        let mut attempts = event_attempts(run_id, event_id, 1, b"owner-a\n".to_vec());
        let adapter_a =
            TraceMetadataSpoolAdapter::open(root.path(), owner(status as u8, 1, 0x55)).unwrap();
        adapter_a
            .register_attested_event_batch(run_id, 1, &attempts)
            .unwrap();
        let dispatch = adapter_a
            .begin_dispatch_for_upload(&attempts[0], 0)
            .unwrap()
            .unwrap();
        let upload = attempts.remove(0);
        let client = CloudflareClient::new_injected(server.uri(), DEFAULT_APP_VERSION);
        let outcome = client
            .upload_trace_v2(&SecretToken::new("owner-a-token".into()), upload)
            .await
            .unwrap();

        let applied = adapter_a.apply_http_outcome(dispatch, outcome, 0).unwrap();
        assert!(matches!(
            (status, applied),
            (401, TraceMetadataUploadOutcome::Unauthorized)
                | (403, TraceMetadataUploadOutcome::Forbidden)
        ));

        let adapter_b =
            TraceMetadataSpoolAdapter::open(root.path(), owner(status as u8 + 1, 1, 0x55)).unwrap();
        let other_run = TraceId::try_new_v7().unwrap();
        let other = event_attempts(
            other_run,
            TraceId::try_new_v7().unwrap(),
            1,
            b"owner-b\n".to_vec(),
        );
        adapter_b
            .register_attested_event_batch(other_run, 1, &other)
            .expect("another owner must remain active");

        let retry = event_attempts(
            run_id,
            TraceId::try_new_v7().unwrap(),
            1,
            b"retry\n".to_vec(),
        );
        assert!(matches!(
            adapter_a.register_attested_event_batch(run_id, 1, &retry),
            Err(TraceMetadataSpoolError::OwnerPaused)
        ));
    }
}

#[tokio::test]
async fn update_required_is_a_global_build_epoch_gate_across_owners() {
    let server = MockServer::start().await;
    mount_status(&server, 426).await;
    let root = TempDir::new().unwrap();
    let run_id = TraceId::try_new_v7().unwrap();
    let event_id = TraceId::try_new_v7().unwrap();
    let mut attempts = event_attempts(run_id, event_id, 1, b"old-build\n".to_vec());
    let adapter_a = TraceMetadataSpoolAdapter::open(root.path(), owner(0x61, 1, 0x70)).unwrap();
    adapter_a
        .register_attested_event_batch(run_id, 1, &attempts)
        .unwrap();
    let dispatch = adapter_a
        .begin_dispatch_for_upload(&attempts[0], 0)
        .unwrap()
        .unwrap();
    let client = CloudflareClient::new_injected(server.uri(), DEFAULT_APP_VERSION);
    let outcome = client
        .upload_trace_v2(
            &SecretToken::new("old-build-token".into()),
            attempts.remove(0),
        )
        .await
        .unwrap();
    assert_eq!(
        adapter_a.apply_http_outcome(dispatch, outcome, 0).unwrap(),
        TraceMetadataUploadOutcome::UpdateRequired
    );

    let blocked_run = TraceId::try_new_v7().unwrap();
    let blocked = event_attempts(
        blocked_run,
        TraceId::try_new_v7().unwrap(),
        1,
        b"same-build\n".to_vec(),
    );
    let same_build = TraceMetadataSpoolAdapter::open(root.path(), owner(0x62, 1, 0x70)).unwrap();
    assert!(matches!(
        same_build.register_attested_event_batch(blocked_run, 1, &blocked),
        Err(TraceMetadataSpoolError::BuildEpochBlocked)
    ));

    let fresh_run = TraceId::try_new_v7().unwrap();
    let fresh = event_attempts(
        fresh_run,
        TraceId::try_new_v7().unwrap(),
        1,
        b"fresh-build\n".to_vec(),
    );
    let fresh_build = TraceMetadataSpoolAdapter::open(root.path(), owner(0x63, 1, 0x71)).unwrap();
    fresh_build
        .register_attested_event_batch(fresh_run, 1, &fresh)
        .expect("a different build epoch must remain eligible");
}
