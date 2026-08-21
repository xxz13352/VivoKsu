use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use nwflash_application::{HeartbeatCallback, SessionLifecycle, SessionLifecycleError};
use nwflash_infrastructure::api_client::{CloudflareError, HeartbeatResult, UpdateRequiredInfo};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};

#[tokio::test]
async fn session_lifecycle_starts_and_stops_send_goodbye() {
    let (heartbeats_tx, mut heartbeats_rx) = mpsc::unbounded_channel::<bool>();
    let callback: HeartbeatCallback = {
        let heartbeats_tx = heartbeats_tx.clone();
        Arc::new(move |_token: String, _session_id: String, active: bool| {
            let heartbeats_tx = heartbeats_tx.clone();
            Box::pin(async move {
                let _ = heartbeats_tx.send(active);
                Ok(HeartbeatResult {
                    force_exit: false,
                    reason: None,
                })
            })
        })
    };

    let lifecycle = SessionLifecycle::with_intervals(
        callback,
        None,
        None,
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(5),
    );

    assert!(lifecycle
        .start("session-1".to_string(), "token-1".to_string())
        .await
        .is_ok());
    let running = timeout(Duration::from_millis(100), async {
        while !lifecycle.is_running().await {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    assert!(running);

    let saw_active = timeout(Duration::from_millis(200), async {
        while let Some(active) = heartbeats_rx.recv().await {
            if active {
                break;
            }
        }
    })
    .await
    .is_ok();
    assert!(saw_active);

    lifecycle.stop().await.unwrap();
    assert!(!lifecycle.is_running().await);

    let saw_goodbye = timeout(Duration::from_millis(100), async {
        while let Some(active) = heartbeats_rx.recv().await {
            if !active {
                break;
            }
        }
    })
    .await
    .is_ok();
    assert!(saw_goodbye);
}

#[tokio::test]
async fn session_lifecycle_triggers_force_exit_callback() {
    let seen_force_exit = Arc::new(AtomicBool::new(false));
    let force_exit_notified = seen_force_exit.clone();
    let (force_exit_tx, mut force_exit_rx) = mpsc::unbounded_channel::<String>();

    let heartbeat_should_force_exit = Arc::new(AtomicBool::new(false));
    let callback: HeartbeatCallback = {
        let heartbeat_should_force_exit = heartbeat_should_force_exit.clone();
        Arc::new(move |_token: String, _session_id: String, _active: bool| {
            let heartbeat_should_force_exit = heartbeat_should_force_exit.clone();
            Box::pin(async move {
                if heartbeat_should_force_exit
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    Ok(HeartbeatResult {
                        force_exit: true,
                        reason: Some("server force exit".to_string()),
                    })
                } else {
                    Ok(HeartbeatResult {
                        force_exit: false,
                        reason: None,
                    })
                }
            })
        })
    };

    let on_force_exit = {
        let force_exit_tx = force_exit_tx.clone();
        Arc::new(move |reason: String| {
            force_exit_notified.store(true, Ordering::Release);
            let _ = force_exit_tx.send(reason);
        })
    };

    let lifecycle = SessionLifecycle::with_intervals(
        callback,
        Some(on_force_exit),
        None,
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(5),
    );

    assert!(lifecycle
        .start("session-2".to_string(), "token-2".to_string())
        .await
        .is_ok());
    let reason = timeout(Duration::from_millis(200), force_exit_rx.recv())
        .await
        .expect("force_exit should be sent")
        .expect("force_exit callback should send reason");
    assert_eq!(reason, "server force exit");

    let stopped = timeout(Duration::from_millis(100), async {
        while lifecycle.is_running().await {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    assert!(stopped);
    assert!(seen_force_exit.load(Ordering::Acquire));
}

#[tokio::test]
async fn session_lifecycle_triggers_update_required_callback() {
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<Option<String>>();
    let callback = {
        let heartbeat_callback: HeartbeatCallback = Arc::new(|_token, _session_id, _active| {
            Box::pin(async {
                Err(CloudflareError::UpdateRequired(UpdateRequiredInfo {
                    message: "need update".to_string(),
                    latest: Some("2.0.0".to_string()),
                    min_version: Some("2.0.0".to_string()),
                    download_url: Some("https://example.com/fw.bin".to_string()),
                }))
            })
        });
        heartbeat_callback
    };
    let on_update = Arc::new(move |info: UpdateRequiredInfo| {
        let _ = update_tx.send(info.latest.clone());
    });

    let lifecycle = SessionLifecycle::with_intervals(
        callback,
        None,
        Some(on_update),
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(5),
    );

    assert!(lifecycle
        .start("session-3".to_string(), "token-3".to_string())
        .await
        .is_ok());

    let got = timeout(Duration::from_millis(200), update_rx.recv())
        .await
        .expect("update callback should fire")
        .expect("update callback should send payload");
    assert_eq!(got, Some("2.0.0".to_string()));

    let stopped = timeout(Duration::from_millis(100), async {
        while lifecycle.is_running().await {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    assert!(stopped);
}

#[tokio::test]
async fn session_lifecycle_sends_goodbye_before_notifying_update_required() {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<&'static str>();
    let callback: HeartbeatCallback = {
        let events_tx = events_tx.clone();
        Arc::new(move |_token, _session_id, active| {
            let events_tx = events_tx.clone();
            Box::pin(async move {
                if active {
                    Err(CloudflareError::UpdateRequired(UpdateRequiredInfo {
                        message: "need update".to_string(),
                        latest: Some("2.0.0".to_string()),
                        min_version: Some("2.0.0".to_string()),
                        download_url: None,
                    }))
                } else {
                    let _ = events_tx.send("goodbye");
                    Ok(HeartbeatResult {
                        force_exit: false,
                        reason: None,
                    })
                }
            })
        })
    };
    let on_update = {
        let events_tx = events_tx.clone();
        Arc::new(move |_info: UpdateRequiredInfo| {
            let _ = events_tx.send("update");
        })
    };

    let lifecycle = SessionLifecycle::with_intervals(
        callback,
        None,
        Some(on_update),
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(50),
    );

    lifecycle
        .start("session-update".to_string(), "token-update".to_string())
        .await
        .expect("session should start");

    let first_event = timeout(Duration::from_millis(200), events_rx.recv())
        .await
        .expect("update flow should emit an event")
        .expect("event channel should remain open");
    assert_eq!(first_event, "goodbye");

    let second_event = timeout(Duration::from_millis(200), events_rx.recv())
        .await
        .expect("update callback should run after goodbye")
        .expect("event channel should remain open");
    assert_eq!(second_event, "update");
}

#[tokio::test]
async fn session_lifecycle_rejects_invalid_session_values() {
    let callback: HeartbeatCallback = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(HeartbeatResult {
                force_exit: false,
                reason: None,
            })
        })
    });
    let lifecycle = SessionLifecycle::new(callback, None, None);
    let result = lifecycle.start(String::new(), "token".to_string()).await;
    assert!(matches!(result, Err(SessionLifecycleError::Message(_))));
}

#[tokio::test]
async fn session_lifecycle_stop_without_start_returns_not_started() {
    let callback: HeartbeatCallback = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(HeartbeatResult {
                force_exit: false,
                reason: None,
            })
        })
    });

    let lifecycle = SessionLifecycle::new(callback, None, None);
    let err = lifecycle.stop().await;
    assert!(matches!(err, Err(SessionLifecycleError::NotStarted)));
}
