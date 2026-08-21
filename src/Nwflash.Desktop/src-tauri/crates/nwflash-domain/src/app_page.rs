use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AppPage {
    Overview,
    AdbActions,
    FileTransfer,
    FastbootFlash,
    RootTools,
    LineFlash,
    FirmwareExtract,
    OperationLog,
    SafeFlash,
    OnlineStatus,
    Software,
}
