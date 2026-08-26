use nwflash_infrastructure::ProcessIdentity;
use nwflash_protection::{admit_local_operation, OperationDecision, SessionLease};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionCapabilityLease {
    pub(crate) epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionCapabilityUnavailable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSecurityState {
    pub(crate) epoch: u64,
    pub(crate) generation: String,
    pub(crate) username: String,
    pub(crate) lease: SessionLease,
    pub(crate) last_verified_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalLeaseAdmission {
    pub(crate) generation: String,
    pub(crate) sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalLeaseAdmissionFailure {
    Inactive,
    StaleEpoch,
    SequenceMismatch,
    Expired,
    BuildIdMismatch,
    ProcessNonceMismatch,
}

#[derive(Debug, Default)]
struct SessionCapabilityState {
    active: bool,
    epoch: u64,
    security: Option<SessionSecurityState>,
}

#[derive(Debug, Default)]
pub(crate) struct SessionCapabilityScope {
    state: Mutex<SessionCapabilityState>,
}

impl SessionCapabilityScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn activate_verified(
        &self,
        generation: String,
        username: String,
        lease: SessionLease,
    ) -> SessionCapabilityLease {
        let mut state = self.lock_state();
        state.epoch = state
            .epoch
            .checked_add(1)
            .expect("session capability epoch exhausted");
        state.active = true;
        let capability = SessionCapabilityLease { epoch: state.epoch };
        let last_verified_sequence = lease.sequence();
        state.security = Some(SessionSecurityState {
            epoch: capability.epoch,
            generation,
            username,
            lease,
            last_verified_sequence,
        });
        capability
    }

    #[cfg(test)]
    pub(crate) fn activate(&self) -> SessionCapabilityLease {
        let mut state = self.lock_state();
        state.epoch = state
            .epoch
            .checked_add(1)
            .expect("session capability epoch exhausted");
        state.active = true;
        state.security = None;
        SessionCapabilityLease { epoch: state.epoch }
    }

    pub(crate) fn security(&self) -> Result<SessionSecurityState, SessionCapabilityUnavailable> {
        let state = self.lock_state();
        if !state.active {
            return Err(SessionCapabilityUnavailable);
        }
        state.security.clone().ok_or(SessionCapabilityUnavailable)
    }

    pub(crate) fn refresh_verified(
        &self,
        capability: SessionCapabilityLease,
        previous_sequence: u64,
        next: SessionLease,
    ) -> Result<(), SessionCapabilityUnavailable> {
        self.refresh_verified_inner(capability, previous_sequence, next, || {})
    }

    #[cfg(test)]
    fn refresh_verified_with_hook(
        &self,
        capability: SessionCapabilityLease,
        previous_sequence: u64,
        next: SessionLease,
        before_publish: impl FnOnce(),
    ) -> Result<(), SessionCapabilityUnavailable> {
        self.refresh_verified_inner(capability, previous_sequence, next, before_publish)
    }

    fn refresh_verified_inner(
        &self,
        capability: SessionCapabilityLease,
        previous_sequence: u64,
        next: SessionLease,
        before_publish: impl FnOnce(),
    ) -> Result<(), SessionCapabilityUnavailable> {
        let mut state = self.lock_state();
        if !state.active || state.epoch != capability.epoch {
            return Err(SessionCapabilityUnavailable);
        }
        let security = state
            .security
            .as_mut()
            .ok_or(SessionCapabilityUnavailable)?;
        if security.epoch != capability.epoch
            || security.lease.sequence() != previous_sequence
            || security.lease.session_id() != next.session_id()
            || next.sequence() <= previous_sequence
        {
            return Err(SessionCapabilityUnavailable);
        }
        before_publish();
        security.last_verified_sequence = next.sequence();
        security.lease = next;
        Ok(())
    }

    pub(crate) fn admit_local(
        &self,
        identity: &ProcessIdentity,
        now: i64,
    ) -> Result<LocalLeaseAdmission, LocalLeaseAdmissionFailure> {
        let snapshot = {
            let state = self.lock_state();
            if !state.active {
                return Err(LocalLeaseAdmissionFailure::Inactive);
            }
            let security = state
                .security
                .as_ref()
                .ok_or(LocalLeaseAdmissionFailure::Inactive)?;
            if security.epoch != state.epoch {
                return Err(LocalLeaseAdmissionFailure::StaleEpoch);
            }
            if security.last_verified_sequence < 1
                || security.lease.sequence() != security.last_verified_sequence
            {
                return Err(LocalLeaseAdmissionFailure::SequenceMismatch);
            }
            security.clone()
        };

        match admit_local_operation(
            &snapshot.lease,
            identity.build_id(),
            identity.process_nonce(),
            now,
        ) {
            OperationDecision::Allow => Ok(LocalLeaseAdmission {
                generation: snapshot.generation,
                sequence: snapshot.last_verified_sequence,
            }),
            OperationDecision::DenyExpired => Err(LocalLeaseAdmissionFailure::Expired),
            OperationDecision::DenyBuildIdMismatch => {
                Err(LocalLeaseAdmissionFailure::BuildIdMismatch)
            }
            OperationDecision::DenyProcessNonceMismatch => {
                Err(LocalLeaseAdmissionFailure::ProcessNonceMismatch)
            }
        }
    }

    pub(crate) fn capture(&self) -> Result<SessionCapabilityLease, SessionCapabilityUnavailable> {
        let state = self.lock_state();
        state
            .active
            .then_some(SessionCapabilityLease { epoch: state.epoch })
            .ok_or(SessionCapabilityUnavailable)
    }

    pub(crate) fn commit<T>(
        &self,
        lease: SessionCapabilityLease,
        publish: impl FnOnce() -> T,
    ) -> Result<T, SessionCapabilityUnavailable> {
        let state = self.lock_state();
        if !state.active || state.epoch != lease.epoch {
            return Err(SessionCapabilityUnavailable);
        }

        let published = publish();
        drop(state);
        Ok(published)
    }

    pub(crate) fn is_current(&self, lease: SessionCapabilityLease) -> bool {
        let state = self.lock_state();
        state.active && state.epoch == lease.epoch
    }

    pub(crate) fn invalidate<T>(&self, clear: impl FnOnce() -> T) -> T {
        let mut state = self.lock_state();
        state.epoch = state
            .epoch
            .checked_add(1)
            .expect("session capability epoch exhausted");
        state.active = false;
        state.security = None;
        let cleared = clear();
        drop(state);
        cleared
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SessionCapabilityState> {
        self.state
            .lock()
            .expect("session capability scope lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::{Signer as _, SigningKey};
    use nwflash_infrastructure::ProcessIdentity;
    use nwflash_protection::{
        accept_login_lease, classify_heartbeat_lease, verify_signed_lease, HeartbeatDecision,
        LeaseBinding, LeaseClaims, LeaseKind, SessionLease, SignedEnvelope, TokenDigest,
    };
    use rand_core::OsRng;

    use super::{LocalLeaseAdmissionFailure, SessionCapabilityLease, SessionCapabilityScope};

    fn verified_lease(sequence: u64) -> SessionLease {
        let signing_key = SigningKey::generate(&mut OsRng);
        let binding = LeaseBinding::new(
            "user",
            TokenDigest::sha256(b"token"),
            "1.0.1",
            "debug-build",
            "process-nonce",
            "signed-session",
        );
        let make_verified = |kind, sequence| {
            let claims = LeaseClaims {
                version: 1,
                kind,
                username: "user".to_string(),
                token_sha256: TokenDigest::sha256(b"token"),
                client_version: "1.0.1".to_string(),
                build_id: "debug-build".to_string(),
                process_nonce: "process-nonce".to_string(),
                session_id: "signed-session".to_string(),
                sequence,
                issued_at: 1_800_000_000,
                expires_at: 1_800_000_300,
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
        let mut lease = accept_login_lease(&login, &binding, 1_800_000_001).unwrap();
        for next in 2..=sequence {
            let heartbeat = make_verified(LeaseKind::Heartbeat, next);
            lease = match classify_heartbeat_lease(
                &heartbeat,
                &binding,
                lease.sequence(),
                1_800_000_001,
            ) {
                HeartbeatDecision::Continue(lease) => lease,
                HeartbeatDecision::ExitPending(reason) => panic!("fixture rejected: {reason:?}"),
            };
        }
        lease
    }

    #[test]
    fn verified_activation_publishes_security_state_with_the_artifact_epoch() {
        let scope = SessionCapabilityScope::new();

        let epoch = scope.activate_verified(
            "generation-one".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        let security = scope
            .security()
            .expect("verified session should be visible");

        assert_eq!(security.epoch, epoch.epoch);
        assert_eq!(security.generation, "generation-one");
        assert_eq!(security.username, "user");
        assert_eq!(security.lease.session_id(), "signed-session");
        assert_eq!(security.lease.sequence(), 1);
        assert_eq!(security.last_verified_sequence, 1);
    }

    #[test]
    fn heartbeat_refresh_is_atomic_and_rejection_preserves_the_previous_sequence() {
        let scope = SessionCapabilityScope::new();
        let epoch = scope.activate_verified(
            "generation-one".to_string(),
            "user".to_string(),
            verified_lease(1),
        );

        scope
            .refresh_verified(epoch, 1, verified_lease(2))
            .expect("current signed heartbeat should refresh");
        let rejected = scope.refresh_verified(epoch, 1, verified_lease(3));
        let security = scope
            .security()
            .expect("rejection must preserve capability");

        assert!(rejected.is_err());
        assert_eq!(security.lease.sequence(), 2);
        assert_eq!(security.last_verified_sequence, 2);
        assert!(scope.is_current(epoch));
    }

    #[test]
    fn local_admission_rejects_inactive_stale_sequence_and_process_binding_snapshots() {
        let identity = ProcessIdentity::new_injected("debug-build", "process-nonce").unwrap();
        let scope = SessionCapabilityScope::new();

        assert_eq!(
            scope.admit_local(&identity, 1_800_000_001),
            Err(LocalLeaseAdmissionFailure::Inactive)
        );

        scope.activate_verified(
            "generation-one".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        {
            let mut state = scope.state.lock().unwrap();
            state.security.as_mut().unwrap().epoch -= 1;
        }
        assert_eq!(
            scope.admit_local(&identity, 1_800_000_001),
            Err(LocalLeaseAdmissionFailure::StaleEpoch)
        );

        let scope = SessionCapabilityScope::new();
        scope.activate_verified(
            "generation-two".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        {
            let mut state = scope.state.lock().unwrap();
            state.security.as_mut().unwrap().last_verified_sequence = 2;
        }
        assert_eq!(
            scope.admit_local(&identity, 1_800_000_001),
            Err(LocalLeaseAdmissionFailure::SequenceMismatch)
        );

        let scope = SessionCapabilityScope::new();
        scope.activate_verified(
            "generation-three".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        let wrong_build = ProcessIdentity::new_injected("other-build", "process-nonce").unwrap();
        assert_eq!(
            scope.admit_local(&wrong_build, 1_800_000_001),
            Err(LocalLeaseAdmissionFailure::BuildIdMismatch)
        );
        let wrong_nonce = ProcessIdentity::new_injected("debug-build", "other-nonce").unwrap();
        assert_eq!(
            scope.admit_local(&wrong_nonce, 1_800_000_001),
            Err(LocalLeaseAdmissionFailure::ProcessNonceMismatch)
        );
        assert_eq!(
            scope.admit_local(&identity, 1_800_000_300),
            Err(LocalLeaseAdmissionFailure::Expired)
        );
    }

    #[test]
    fn local_admission_returns_the_current_generation_after_atomic_refresh() {
        let identity = ProcessIdentity::new_injected("debug-build", "process-nonce").unwrap();
        let scope = SessionCapabilityScope::new();
        let capability = scope.activate_verified(
            "generation-current".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        scope
            .refresh_verified(capability, 1, verified_lease(2))
            .unwrap();

        let admission = scope.admit_local(&identity, 1_800_000_001).unwrap();

        assert_eq!(admission.generation, "generation-current");
        assert_eq!(admission.sequence, 2);
    }

    #[test]
    fn heartbeat_refresh_and_invalidation_are_serialized_by_the_capability_barrier() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let epoch = scope.activate_verified(
            "generation-one".to_string(),
            "user".to_string(),
            verified_lease(1),
        );
        let (refresh_entered_tx, refresh_entered_rx) = mpsc::channel();
        let (release_refresh_tx, release_refresh_rx) = mpsc::channel();
        let refresher = std::thread::spawn({
            let scope = scope.clone();
            move || {
                scope.refresh_verified_with_hook(epoch, 1, verified_lease(2), || {
                    refresh_entered_tx.send(()).unwrap();
                    release_refresh_rx.recv().unwrap();
                })
            }
        });
        refresh_entered_rx.recv().unwrap();

        let (invalidate_attempt_tx, invalidate_attempt_rx) = mpsc::channel();
        let (clear_entered_tx, clear_entered_rx) = mpsc::channel();
        let invalidator = std::thread::spawn({
            let scope = scope.clone();
            move || {
                invalidate_attempt_tx.send(()).unwrap();
                scope.invalidate(|| clear_entered_tx.send(()).unwrap());
            }
        });
        invalidate_attempt_rx.recv().unwrap();
        let invalidation_interleaved = clear_entered_rx
            .recv_timeout(Duration::from_millis(50))
            .is_ok();

        release_refresh_tx.send(()).unwrap();
        refresher.join().unwrap().unwrap();
        if !invalidation_interleaved {
            clear_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
        }
        invalidator.join().unwrap();

        assert!(!invalidation_interleaved);
        assert!(scope.capture().is_err());
        assert!(scope.security().is_err());
    }

    #[test]
    fn activation_exposes_only_the_current_epoch() {
        let scope = SessionCapabilityScope::new();

        assert!(scope.capture().is_err());

        let lease = scope.activate();

        assert_eq!(scope.capture(), Ok(lease));
        assert!(scope.is_current(lease));
    }

    #[test]
    fn invalidation_rejects_stale_commits_after_reactivation() {
        let scope = SessionCapabilityScope::new();
        let first = scope.activate();
        scope.invalidate(|| {});
        let second = scope.activate();

        assert_ne!(first, second);
        assert!(!scope.is_current(first));
        assert!(scope.is_current(second));
        assert!(scope.commit(first, || ()).is_err());
        assert!(scope.commit(second, || ()).is_ok());
    }

    #[test]
    fn invalidation_prevents_a_delayed_producer_from_publishing() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let published = Arc::new(AtomicBool::new(false));
        let (resume_tx, resume_rx) = mpsc::channel();

        let producer = std::thread::spawn({
            let scope = scope.clone();
            let published = published.clone();
            move || {
                resume_rx
                    .recv()
                    .expect("producer should be released after invalidation");
                scope.commit(lease, || published.store(true, Ordering::Release))
            }
        });

        scope.invalidate(|| {});
        resume_tx
            .send(())
            .expect("delayed producer should still be waiting");

        assert!(producer.join().expect("producer should join").is_err());
        assert!(!published.load(Ordering::Acquire));
    }

    #[test]
    fn commit_publication_blocks_invalidation_clear() {
        let scope = Arc::new(SessionCapabilityScope::new());
        let lease = scope.activate();
        let (publish_entered_tx, publish_entered_rx) = mpsc::channel();
        let (release_publish_tx, release_publish_rx) = mpsc::channel();

        let producer = std::thread::spawn({
            let scope = scope.clone();
            move || {
                scope.commit(lease, || {
                    publish_entered_tx
                        .send(())
                        .expect("test should observe publication start");
                    release_publish_rx
                        .recv()
                        .expect("test should release publication");
                })
            }
        });

        publish_entered_rx
            .recv()
            .expect("publication should begin while holding the scope barrier");

        let (invalidation_attempted_tx, invalidation_attempted_rx) = mpsc::channel();
        let (clear_entered_tx, clear_entered_rx) = mpsc::channel();
        let invalidator = std::thread::spawn({
            let scope = scope.clone();
            move || {
                invalidation_attempted_tx
                    .send(())
                    .expect("test should observe the invalidation attempt");
                scope.invalidate(|| {
                    clear_entered_tx
                        .send(())
                        .expect("test should observe clear entering the barrier");
                });
            }
        });

        invalidation_attempted_rx
            .recv()
            .expect("invalidation should attempt to enter the scope barrier");
        let clear_interleaved = clear_entered_rx
            .recv_timeout(Duration::from_millis(50))
            .is_ok();

        release_publish_tx
            .send(())
            .expect("publication should still be waiting");
        producer
            .join()
            .expect("producer should join")
            .expect("current producer should publish");
        if !clear_interleaved {
            clear_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("clear should run after publication releases the barrier");
        }
        invalidator.join().expect("invalidator should join");

        assert!(
            !clear_interleaved,
            "invalidation clear must not interleave with publication"
        );
    }

    #[test]
    fn epoch_exhaustion_poison_never_allows_stale_publication() {
        let scope = SessionCapabilityScope::new();
        {
            let mut state = scope.state.lock().expect("scope should begin healthy");
            state.active = true;
            state.epoch = u64::MAX;
        }
        let stale_lease = SessionCapabilityLease { epoch: u64::MAX };

        let exhausted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scope.invalidate(|| ());
        }));
        assert!(exhausted.is_err());

        let published = AtomicBool::new(false);
        let after_poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = scope.commit(stale_lease, || published.store(true, Ordering::Release));
        }));

        assert!(after_poison.is_err());
        assert!(!published.load(Ordering::Acquire));
    }
}
