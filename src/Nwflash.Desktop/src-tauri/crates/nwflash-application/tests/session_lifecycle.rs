use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_application::{
    HeartbeatCallback, HeartbeatInput, SessionLifecycle, SessionLifecycleError,
    SessionLifecycleSession,
};
use nwflash_infrastructure::{
    CloudflareError, HeartbeatAdmission, IntegrityFailure, SecretToken, UpdateRequiredInfo,
};
use nwflash_protection::{
    accept_login_lease, classify_heartbeat_lease, verify_signed_lease, HeartbeatDecision,
    LeaseBinding, LeaseClaims, LeaseKind, SessionLease, SignedEnvelope, TokenDigest,
};
use rand_core::OsRng;
use tokio::{sync::mpsc, time::timeout};

const TOKEN: &str = "lifecycle-token";
const USERNAME: &str = "lifecycle-user";
const SESSION_ID: &str = "lifecycle-session";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_secs() as i64
}

fn signed_lease(sequence: u64) -> SessionLease {
    let signing_key = SigningKey::generate(&mut OsRng);
    let binding = LeaseBinding::new(
        USERNAME,
        TokenDigest::sha256(TOKEN.as_bytes()),
        "1.0.1",
        "debug-build",
        "process-nonce",
        SESSION_ID,
    );
    let issued_at = now();
    let make_verified = |kind, sequence| {
        let claims = LeaseClaims {
            version: 1,
            kind,
            username: USERNAME.to_string(),
            token_sha256: TokenDigest::sha256(TOKEN.as_bytes()),
            client_version: "1.0.1".to_string(),
            build_id: "debug-build".to_string(),
            process_nonce: "process-nonce".to_string(),
            session_id: SESSION_ID.to_string(),
            sequence,
            issued_at,
            expires_at: issued_at + 300,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
        verify_signed_lease(
            &SignedEnvelope {
                lease_payload: payload,
                lease_signature: signature,
            },
            &signing_key.verifying_key(),
        )
        .unwrap()
    };

    let login = make_verified(LeaseKind::Login, 1);
    let mut lease = accept_login_lease(&login, &binding, issued_at).unwrap();
    for next in 2..=sequence {
        let heartbeat = make_verified(LeaseKind::Heartbeat, next);
        lease = match classify_heartbeat_lease(&heartbeat, &binding, lease.sequence(), issued_at) {
            HeartbeatDecision::Continue(lease) => lease,
            HeartbeatDecision::ExitPending(reason) => panic!("fixture rejected: {reason:?}"),
        };
    }
    lease
}

fn lifecycle_session(sequence: u64) -> SessionLifecycleSession {
    SessionLifecycleSession::new(
        SecretToken::new(TOKEN.to_string()),
        USERNAME.to_string(),
        signed_lease(sequence),
    )
}

fn short_lifecycle(
    callback: HeartbeatCallback,
    on_force_exit: Option<nwflash_application::ForceExitCallback>,
    on_update_required: Option<nwflash_application::UpdateRequiredCallback>,
) -> SessionLifecycle {
    SessionLifecycle::with_intervals(
        callback,
        on_force_exit,
        on_update_required,
        Duration::from_millis(5),
        Duration::from_millis(25),
        Duration::from_millis(25),
    )
}

#[tokio::test]
async fn explicit_stop_sends_one_bounded_authenticated_goodbye() {
    let (calls_tx, mut calls_rx) = mpsc::unbounded_channel::<HeartbeatInput>();
    let callback: HeartbeatCallback = Arc::new(move |input| {
        let calls_tx = calls_tx.clone();
        Box::pin(async move {
            let active = input.active;
            calls_tx.send(input).unwrap();
            if active {
                Ok(HeartbeatAdmission::Accepted(signed_lease(2)))
            } else {
                Ok(HeartbeatAdmission::Goodbye)
            }
        })
    });
    let lifecycle = short_lifecycle(callback, None, None);

    lifecycle.start(lifecycle_session(1)).await.unwrap();
    let active = timeout(Duration::from_millis(150), calls_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(active.active);
    assert_eq!(active.token.as_str(), TOKEN);
    assert_eq!(active.lease.session_id(), SESSION_ID);
    assert_eq!(active.lease.sequence(), 1);

    lifecycle.stop().await.unwrap();
    let goodbye = timeout(Duration::from_millis(150), async {
        loop {
            let call = calls_rx.recv().await.unwrap();
            if !call.active {
                break call;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(goodbye.lease.session_id(), SESSION_ID);
    assert!(!lifecycle.is_running().await);
}

#[tokio::test]
async fn explicit_stop_interrupts_the_heartbeat_interval_before_goodbye() {
    let (active_tx, mut active_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = Arc::new(move |input| {
        let active_tx = active_tx.clone();
        Box::pin(async move {
            active_tx.send(input.active).unwrap();
            if input.active {
                Ok(HeartbeatAdmission::Accepted(signed_lease(2)))
            } else {
                Ok(HeartbeatAdmission::Goodbye)
            }
        })
    });
    let lifecycle = SessionLifecycle::with_intervals(
        callback,
        None,
        None,
        Duration::from_secs(30),
        Duration::from_millis(25),
        Duration::from_millis(25),
    );
    lifecycle.start(lifecycle_session(1)).await.unwrap();
    assert_eq!(active_rx.recv().await, Some(true));

    timeout(Duration::from_millis(150), lifecycle.stop())
        .await
        .expect("stop must interrupt interval sleep")
        .unwrap();
    assert_eq!(active_rx.recv().await, Some(false));
}

#[tokio::test]
async fn accepted_heartbeat_advances_the_next_input_sequence() {
    let (sequence_tx, mut sequence_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = Arc::new(move |input| {
        let sequence_tx = sequence_tx.clone();
        Box::pin(async move {
            sequence_tx.send(input.lease.sequence()).unwrap();
            Ok(HeartbeatAdmission::Accepted(signed_lease(
                input.lease.sequence() + 1,
            )))
        })
    });
    let lifecycle = short_lifecycle(callback, None, None);
    lifecycle.start(lifecycle_session(1)).await.unwrap();

    assert_eq!(
        timeout(Duration::from_millis(150), sequence_rx.recv())
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        timeout(Duration::from_millis(150), sequence_rx.recv())
            .await
            .unwrap(),
        Some(2)
    );
    lifecycle.stop().await.unwrap();
}

#[tokio::test]
async fn force_exit_is_terminal_once_and_does_not_send_goodbye_early() {
    let (active_tx, mut active_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = Arc::new(move |input| {
        let active_tx = active_tx.clone();
        Box::pin(async move {
            active_tx.send(input.active).unwrap();
            Ok(HeartbeatAdmission::ForceExit(
                "server force exit".to_string(),
            ))
        })
    });
    let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
    let lifecycle = short_lifecycle(
        callback,
        Some(Arc::new(move |reason| terminal_tx.send(reason).unwrap())),
        None,
    );
    lifecycle.start(lifecycle_session(1)).await.unwrap();

    assert_eq!(
        timeout(Duration::from_millis(150), terminal_rx.recv())
            .await
            .unwrap(),
        Some("server force exit".to_string())
    );
    assert_eq!(active_rx.recv().await, Some(true));
    assert!(timeout(Duration::from_millis(40), active_rx.recv())
        .await
        .is_err());
    assert!(!lifecycle.is_running().await);

    lifecycle.stop().await.unwrap();
    assert_eq!(
        timeout(Duration::from_millis(150), active_rx.recv())
            .await
            .unwrap(),
        Some(false)
    );
}

#[tokio::test]
async fn integrity_and_terminal_status_failures_stop_on_the_first_occurrence() {
    let failures = [
        CloudflareError::Integrity(IntegrityFailure::LeaseSignature),
        CloudflareError::Integrity(IntegrityFailure::LeaseBinding),
        CloudflareError::Integrity(IntegrityFailure::LeaseTime),
        CloudflareError::Integrity(IntegrityFailure::LeaseSequence),
        CloudflareError::ApiError {
            status: 401,
            message: "unauthorized".into(),
        },
        CloudflareError::ApiError {
            status: 403,
            message: "forbidden".into(),
        },
        CloudflareError::ApiError {
            status: 409,
            message: "conflict".into(),
        },
    ];

    for failure in failures {
        let attempts = Arc::new(Mutex::new(0_u32));
        let callback: HeartbeatCallback = {
            let attempts = attempts.clone();
            Arc::new(move |_input| {
                *attempts.lock().unwrap() += 1;
                let failure = failure.clone();
                Box::pin(async move { Err(failure) })
            })
        };
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let lifecycle = short_lifecycle(
            callback,
            Some(Arc::new(move |reason| terminal_tx.send(reason).unwrap())),
            None,
        );
        lifecycle.start(lifecycle_session(1)).await.unwrap();

        timeout(Duration::from_millis(150), terminal_rx.recv())
            .await
            .unwrap();
        assert_eq!(*attempts.lock().unwrap(), 1);
        assert!(!lifecycle.is_running().await);
        lifecycle.stop().await.unwrap();
    }
}

#[tokio::test]
async fn update_required_is_terminal_on_the_first_occurrence_without_early_goodbye() {
    let (active_tx, mut active_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = Arc::new(move |input| {
        let active_tx = active_tx.clone();
        Box::pin(async move {
            active_tx.send(input.active).unwrap();
            Err(CloudflareError::UpdateRequired(UpdateRequiredInfo {
                message: "need update".to_string(),
                latest: Some("2.0.0".to_string()),
                min_version: Some("2.0.0".to_string()),
                download_url: None,
            }))
        })
    });
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let lifecycle = short_lifecycle(
        callback,
        None,
        Some(Arc::new(move |update| update_tx.send(update).unwrap())),
    );
    lifecycle.start(lifecycle_session(1)).await.unwrap();

    let update = timeout(Duration::from_millis(150), update_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update.latest.as_deref(), Some("2.0.0"));
    assert_eq!(active_rx.recv().await, Some(true));
    assert!(timeout(Duration::from_millis(40), active_rx.recv())
        .await
        .is_err());
    lifecycle.stop().await.unwrap();
}

#[tokio::test]
async fn transient_failures_become_terminal_on_exactly_the_third_consecutive_attempt() {
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = Arc::new(move |_input| {
        let attempt_tx = attempt_tx.clone();
        Box::pin(async move {
            attempt_tx.send(()).unwrap();
            Err(CloudflareError::Transport("offline".to_string()))
        })
    });
    let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
    let lifecycle = short_lifecycle(
        callback,
        Some(Arc::new(move |reason| terminal_tx.send(reason).unwrap())),
        None,
    );
    lifecycle.start(lifecycle_session(1)).await.unwrap();

    attempt_rx.recv().await.unwrap();
    attempt_rx.recv().await.unwrap();
    assert!(terminal_rx.try_recv().is_err());
    attempt_rx.recv().await.unwrap();
    timeout(Duration::from_millis(150), terminal_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(timeout(Duration::from_millis(40), attempt_rx.recv())
        .await
        .is_err());
    lifecycle.stop().await.unwrap();
}

#[tokio::test]
async fn accepted_heartbeat_resets_the_transient_failure_counter() {
    let responses = Arc::new(Mutex::new(VecDeque::from([
        Err(CloudflareError::ApiError {
            status: 500,
            message: "one".into(),
        }),
        Err(CloudflareError::ApiError {
            status: 429,
            message: "two".into(),
        }),
        Ok(HeartbeatAdmission::Accepted(signed_lease(2))),
        Err(CloudflareError::Transport("three".into())),
        Err(CloudflareError::ApiError {
            status: 503,
            message: "four".into(),
        }),
    ])));
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let callback: HeartbeatCallback = {
        let responses = responses.clone();
        Arc::new(move |_input| {
            let response = responses.lock().unwrap().pop_front();
            let attempt_tx = attempt_tx.clone();
            Box::pin(async move {
                attempt_tx.send(()).unwrap();
                response.unwrap_or_else(|| Ok(HeartbeatAdmission::Accepted(signed_lease(3))))
            })
        })
    };
    let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
    let lifecycle = short_lifecycle(
        callback,
        Some(Arc::new(move |reason| terminal_tx.send(reason).unwrap())),
        None,
    );
    lifecycle.start(lifecycle_session(1)).await.unwrap();

    for _ in 0..5 {
        timeout(Duration::from_millis(150), attempt_rx.recv())
            .await
            .unwrap();
    }
    assert!(terminal_rx.try_recv().is_err());
    assert!(lifecycle.is_running().await);
    lifecycle.stop().await.unwrap();
}

#[tokio::test]
async fn start_rejects_an_empty_secret_and_stop_without_context_is_not_started() {
    let callback: HeartbeatCallback =
        Arc::new(|_input| Box::pin(async { Ok(HeartbeatAdmission::Goodbye) }));
    let lifecycle = SessionLifecycle::new(callback, None, None);
    let invalid = SessionLifecycleSession::new(
        SecretToken::new(String::new()),
        USERNAME.to_string(),
        signed_lease(1),
    );

    assert!(matches!(
        lifecycle.start(invalid).await,
        Err(SessionLifecycleError::Message(_))
    ));
    assert!(matches!(
        lifecycle.stop().await,
        Err(SessionLifecycleError::NotStarted)
    ));
}
