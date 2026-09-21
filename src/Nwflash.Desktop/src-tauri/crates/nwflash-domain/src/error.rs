use thiserror::Error;

pub type DomainResult<T> = Result<T, DomainError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("用户取消: {0}")]
    UserCancelled(String),
    /// 写入过程中被反调试机制挂起。与 `UserCancelled` **语义不同**:取消是
    /// 用户主动中止并走收尾逻辑;挂起是"暂停推进、等待用户处置",设备会话
    /// 必须保持不动,因为设备可能正处于写了一半的分区上。
    #[error("写入已挂起: {0}")]
    WriteSuspended(String),
    #[error("设备不可用: {0}")]
    DeviceUnavailable(String),
    #[error("授权被拒绝: {0}")]
    AuthorizationDenied(String),
    #[error("服务端错误: {0}")]
    RemoteApi(String),
    #[error("外部工具执行失败: {0}")]
    ExternalTool(String),
    #[error("文件格式不合法: {0}")]
    InvalidFormat(String),
    #[error("参数错误: {0}")]
    InvalidInput(String),
    #[error("非法操作: {0}")]
    InvalidOperation(String),
    #[error("内部错误: {0}")]
    Internal(String),
}
