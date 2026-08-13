use thiserror::Error;

#[derive(Debug, Error)]
pub enum FastbootError {
    #[error("传输失败：{0}")]
    Transport(#[from] TransportError),

    #[error("协议异常：{0}")]
    Protocol(String),

    #[error("设备失败：{0}")]
    Device(String),

    #[error("参数错误：{0}")]
    InvalidArg(String),

    #[error("超时")]
    Timeout,

    #[error("Sparse 处理失败：{0}")]
    Sparse(#[from] SparseError),

    #[error("IO 失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("无设备")]
    NoDevice,

    #[error("多设备，请使用 -s 指定")]
    MultipleDevices,

    #[error("分区不存在：{0}")]
    PartitionNotFound(String),

    #[error("镜像未找到：{0}")]
    ImageNotFound(String),

    #[error("镜像过大：{0} > {1}")]
    ImageTooLarge(u64, u64),

    #[error("设备已锁定")]
    DeviceLocked,

    #[error("需要 fastbootd 模式")]
    NeedsFastbootd,

    #[error("adb: {0}")]
    Adb(String),
}

impl FastbootError {
    pub fn recovery_hint(&self) -> Option<&'static str> {
        match self {
            FastbootError::NoDevice => Some(
                "请检查:\n\
                 1. 设备是否已连接并进入 bootloader 模式\n\
                 2. USB 线是否正常\n\
                 3. 驱动是否已安装",
            ),
            FastbootError::Transport(TransportError::Disconnected) => Some(
                "设备连接中断，请:\n\
                 1. 检查 USB 连接\n\
                 2. 重新进入 bootloader 模式\n\
                 3. 重试操作",
            ),
            FastbootError::Transport(TransportError::NoLink) => Some(
                "传输中断 (no link)，请:\n\
                 1. 拔掉 USB 线\n\
                 2. 长按电源键重启设备\n\
                 3. 重新进入 bootloader 模式\n\
                 4. 重新连接并重试",
            ),
            FastbootError::DeviceLocked => Some(
                "设备已锁定，无法刷写。请先解锁:\n\
                 1. fastboot flashing unlock\n\
                 2. 在设备上确认解锁\n\
                 3. 注意: 解锁会清除所有数据",
            ),
            FastbootError::NeedsFastbootd => Some(
                "逻辑分区需要在 fastbootd 模式下刷写:\n\
                 fastboot reboot fastboot",
            ),
            FastbootError::Timeout => Some(
                "设备响应超时，可能原因:\n\
                 1. 设备正在处理大文件\n\
                 2. 设备卡住了，需要重启\n\
                 3. USB 连接不稳定",
            ),
            _ => None,
        }
    }

    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            FastbootError::Timeout
                | FastbootError::Transport(TransportError::Timeout)
                | FastbootError::Transport(TransportError::Io(_))
        )
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO：{0}")]
    Io(#[from] std::io::Error),

    #[error("超时")]
    Timeout,

    #[error("断开")]
    Disconnected,

    #[error("无链接")]
    NoLink,

    #[error("USB：{0}")]
    Usb(String),

    #[error("协议：{0}")]
    Protocol(String),

    #[error("连接拒绝")]
    ConnectionRefused,

    #[error("设备忙")]
    DeviceBusy,
}

impl TransportError {
    pub fn is_usb_access_denied(&self) -> bool {
        match self {
            Self::Io(e) => {
                e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5)
            }
            Self::Usb(s) => {
                let t = s.to_lowercase();
                t.contains("os error 5")
                    || t.contains("access is denied")
                    || t.contains("拒绝访问")
                    || t.contains("拒绝")
            }
            _ => false,
        }
    }

    pub fn is_no_link(&self) -> bool {
        matches!(self, TransportError::NoLink | TransportError::Disconnected)
    }

    pub fn from_usb_error(msg: &str) -> Self {
        let msg_lower = msg.to_lowercase();
        if msg_lower.contains("no link")
            || msg_lower.contains("pipe")
            || msg_lower.contains("disconnected")
            || msg_lower.contains("not found")
        {
            TransportError::NoLink
        } else if msg_lower.contains("timeout") {
            TransportError::Timeout
        } else if msg_lower.contains("busy") {
            TransportError::DeviceBusy
        } else {
            TransportError::Usb(msg.to_string())
        }
    }
}

#[derive(Debug, Error)]
pub enum SparseError {
    #[error("无效 sparse 魔数: 0x{0:08X}")]
    InvalidMagic(u32),

    #[error("无效 sparse 头: {0}")]
    InvalidHeader(String),

    #[error("不支持的 sparse 版本: {0}.{1}")]
    UnsupportedVersion(u16, u16),

    #[error("无效 chunk 类型: 0x{0:04X}")]
    InvalidChunkType(u16),

    #[error("校验和不匹配")]
    ChecksumMismatch,

    #[error("文件太大: {0}")]
    FileTooLarge(u64),

    #[error("IO：{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetCode {
    Success,
    BadArg,
    IoError,
    BadDeviceResponse,
    DeviceFail,
    Timeout,
}

impl std::fmt::Display for RetCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetCode::Success => write!(f, "成功"),
            RetCode::BadArg => write!(f, "参数错误"),
            RetCode::IoError => write!(f, "IO 错误"),
            RetCode::BadDeviceResponse => write!(f, "设备响应错误"),
            RetCode::DeviceFail => write!(f, "设备错误"),
            RetCode::Timeout => write!(f, "超时"),
        }
    }
}

pub type FastbootResult<T> = Result<T, FastbootError>;

pub type TransportResult<T> = Result<T, TransportError>;
