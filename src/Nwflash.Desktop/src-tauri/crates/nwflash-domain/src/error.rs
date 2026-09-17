use thiserror::Error;

pub type DomainResult<T> = Result<T, DomainError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("用户取消: {0}")]
    UserCancelled(String),
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
