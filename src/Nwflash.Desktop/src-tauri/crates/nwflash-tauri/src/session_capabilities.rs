use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionCapabilityLease {
    pub(crate) epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionCapabilityUnavailable;

#[derive(Debug, Default)]
struct SessionCapabilityState {
    active: bool,
    epoch: u64,
}

#[derive(Debug, Default)]
pub(crate) struct SessionCapabilityScope {
    state: Mutex<SessionCapabilityState>,
}

impl SessionCapabilityScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn activate(&self) -> SessionCapabilityLease {
        let mut state = self.lock_state();
        state.epoch = state
            .epoch
            .checked_add(1)
            .expect("session capability epoch exhausted");
        state.active = true;
        SessionCapabilityLease { epoch: state.epoch }
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

    use super::{SessionCapabilityLease, SessionCapabilityScope};

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
