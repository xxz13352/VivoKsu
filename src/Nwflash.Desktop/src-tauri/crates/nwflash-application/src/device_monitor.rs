use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRefreshResult {
    SkippedBusy,
    Deferred,
    Applied,
    AppliedAndBroadcast,
}

#[derive(Debug, Clone)]
pub struct DeviceMonitor {
    snapshot: DeviceSnapshot,
    consecutive_automatic_disconnects: u8,
    consecutive_automatic_errors: u8,
}

impl DeviceMonitor {
    pub fn new(snapshot: DeviceSnapshot) -> Self {
        Self {
            snapshot,
            consecutive_automatic_disconnects: 0,
            consecutive_automatic_errors: 0,
        }
    }

    pub fn snapshot(&self) -> &DeviceSnapshot {
        &self.snapshot
    }

    pub fn refresh(
        &mut self,
        next_snapshot: DeviceSnapshot,
        is_device_busy: bool,
        mode: DeviceRefreshMode,
    ) -> MonitorRefreshResult {
        if mode == DeviceRefreshMode::Automatic && is_device_busy {
            return MonitorRefreshResult::SkippedBusy;
        }

        if mode == DeviceRefreshMode::Automatic
            && next_snapshot.connection_state == DeviceConnectionState::Error
            && self.snapshot.connection_state != DeviceConnectionState::Error
        {
            self.consecutive_automatic_errors += 1;
            if self.consecutive_automatic_errors < 3 {
                return MonitorRefreshResult::Deferred;
            }
        } else {
            self.consecutive_automatic_errors = 0;
        }

        if mode == DeviceRefreshMode::Automatic
            && next_snapshot.connection_state == DeviceConnectionState::Disconnected
            && self.snapshot.connection_state != DeviceConnectionState::Disconnected
        {
            self.consecutive_automatic_disconnects += 1;
            if self.consecutive_automatic_disconnects < 2 {
                return MonitorRefreshResult::Deferred;
            }
        } else {
            self.consecutive_automatic_disconnects = 0;
        }

        let identity_changed = self.snapshot.connection_state != next_snapshot.connection_state
            || self.snapshot.serial != next_snapshot.serial;
        self.snapshot = next_snapshot;

        if mode == DeviceRefreshMode::Manual || identity_changed {
            MonitorRefreshResult::AppliedAndBroadcast
        } else {
            MonitorRefreshResult::Applied
        }
    }
}
