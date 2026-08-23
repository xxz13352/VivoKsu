use std::fmt::{self, Display, Formatter};

#[cfg(debug_assertions)]
use std::time::Duration;

#[cfg(debug_assertions)]
use ed25519_dalek::VerifyingKey;
#[cfg(debug_assertions)]
use nwflash_protection::verify_signed_lease;
use nwflash_protection::{LeaseVerificationError, SignedEnvelope, VerifiedLease};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, Method, Response, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

use nwflash_domain::UsageLogEntry;

use crate::pinned_tls::{classify_reqwest_error, ApiTlsPolicy, PinnedApiClient, PinsetClaims};
use crate::{ProcessIdentity, SecretToken};

pub const DEFAULT_BASE_URL: &str = "https://api.nwflash.cc.cd";
pub const DEFAULT_APP_VERSION: &str = "1.0.1";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CloudflareError {
    #[error("网络完整性校验失败: {0}")]
    Integrity(crate::pinned_tls::IntegrityFailure),
    #[error("网络错误: {0}")]
    Transport(String),
    #[error("服务端返回 {status}: {message}")]
    ApiError { status: u16, message: String },
    #[error("需要更新: {0}")]
    UpdateRequired(UpdateRequiredInfo),
    #[error("参数错误: {0}")]
    InvalidInput(String),
    #[error("响应解析失败: {0}")]
    InvalidResponse(String),
}

impl CloudflareError {
    pub fn status_code(&self) -> Option<u16> {
        match self {
            CloudflareError::ApiError { status, .. } => Some(*status),
            CloudflareError::UpdateRequired(_) => Some(StatusCode::UPGRADE_REQUIRED.as_u16()),
            _ => None,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            CloudflareError::Integrity(_) => "网络完整性校验失败，已拒绝请求。".to_string(),
            CloudflareError::Transport(_) => "网络连接失败，请稍后重试。".to_string(),
            CloudflareError::ApiError { status: 401, .. } => {
                "用户名或密码错误，或账号不可用。".to_string()
            }
            CloudflareError::ApiError { status: 403, .. } => "服务端拒绝了当前请求。".to_string(),
            CloudflareError::ApiError { status: 409, .. } => {
                "会话已失效或发生冲突，请重新登录。".to_string()
            }
            CloudflareError::ApiError { status: 429, .. } => {
                "请求过于频繁，请稍后重试。".to_string()
            }
            CloudflareError::ApiError {
                status: 500..=599, ..
            } => "服务暂时不可用，请稍后重试。".to_string(),
            CloudflareError::ApiError { .. } => "服务端拒绝了当前请求。".to_string(),
            CloudflareError::UpdateRequired(_) => {
                "需要更新: 客户端版本过低，请更新后重试。".to_string()
            }
            CloudflareError::InvalidInput(_) => "请求参数无效。".to_string(),
            CloudflareError::InvalidResponse(_) => "服务端响应无效。".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateRequiredInfo {
    pub message: String,
    pub latest: Option<String>,
    pub min_version: Option<String>,
    pub download_url: Option<String>,
}

impl Display for UpdateRequiredInfo {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

pub type CloudflareResult<T> = Result<T, CloudflareError>;

struct ZeroizingResponseBody(Zeroizing<String>);

impl ZeroizingResponseBody {
    fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ZeroizingResponseBody {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("ZeroizingResponseBody([REDACTED])")
    }
}

impl Zeroize for ZeroizingResponseBody {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Serialize)]
pub struct LoginRequest {
    pub username: String,
    password: SecretPassword,
    pub client_version: String,
    pub build_id: String,
    pub process_nonce: String,
    pub session_id: String,
}

impl LoginRequest {
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
        client_version: impl Into<String>,
        build_id: impl Into<String>,
        process_nonce: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            password: SecretPassword(Zeroizing::new(password.into())),
            client_version: client_version.into(),
            build_id: build_id.into(),
            process_nonce: process_nonce.into(),
            session_id: session_id.into(),
        }
    }
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("client_version", &self.client_version)
            .field("build_id", &self.build_id)
            .field("process_nonce", &self.process_nonce)
            .field("session_id", &self.session_id)
            .finish()
    }
}

struct SecretPassword(Zeroizing<String>);

impl Serialize for SecretPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

#[derive(Deserialize)]
pub struct LoginResult {
    pub token: SecretToken,
    pub username: String,
    pub name: String,
    #[serde(default)]
    pub lease_payload: String,
    #[serde(default)]
    pub lease_signature: String,
}

impl LoginResult {
    pub(crate) fn signed_envelope(&self) -> SignedEnvelope {
        SignedEnvelope {
            lease_payload: self.lease_payload.clone(),
            lease_signature: self.lease_signature.clone(),
        }
    }
}

impl fmt::Debug for LoginResult {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginResult")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("name", &self.name)
            .field("lease_payload", &"[REDACTED]")
            .field("lease_signature", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomResolveResponse {
    pub pd: String,
    pub version: String,
    pub url: String,
    pub name: Option<String>,
    #[serde(rename = "sizeBytes", alias = "size_bytes")]
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatRequest {
    pub session_id: String,
    pub client_version: String,
    pub build_id: String,
    pub process_nonce: String,
    pub sequence: u64,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IntegrityReportRequest {
    pub event_id: String,
    pub phase: IntegrityReportPhase,
    pub reason: IntegrityReportReason,
    pub client_version: String,
    pub build_id: String,
    pub occurred_at: i64,
}

impl IntegrityReportRequest {
    fn validate(&self) -> CloudflareResult<()> {
        if !valid_identifier(&self.event_id, 64) {
            return Err(CloudflareError::InvalidInput(
                "integrity event id 格式无效。".to_string(),
            ));
        }
        if !valid_client_version(&self.client_version) {
            return Err(CloudflareError::InvalidInput(
                "integrity client version 格式无效。".to_string(),
            ));
        }
        if !valid_identifier(&self.build_id, 128) {
            return Err(CloudflareError::InvalidInput(
                "integrity build id 格式无效。".to_string(),
            ));
        }
        if self.occurred_at <= 0 {
            return Err(CloudflareError::InvalidInput(
                "integrity occurred_at 必须为正数。".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityReportPhase {
    Startup,
    Login,
    SessionRestore,
    Heartbeat,
    OperationAdmission,
    PinValidation,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityReportReason {
    ImageCrcInvalid,
    LeaseSignatureInvalid,
    LeaseBindingInvalid,
    LeaseExpired,
    SequenceRollback,
    PinMismatch,
    DebuggerDetected,
    VirtualMachineDetected,
    AuthenticodeInvalid,
    ReleaseManifestInvalid,
}

#[derive(Debug, Serialize)]
struct GoodbyeRequest {
    session_id: String,
    active: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HeartbeatResult {
    pub force_exit: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub lease_payload: String,
    #[serde(default)]
    pub lease_signature: String,
}

impl HeartbeatResult {
    pub(crate) fn signed_envelope(&self) -> SignedEnvelope {
        SignedEnvelope {
            lease_payload: self.lease_payload.clone(),
            lease_signature: self.lease_signature.clone(),
        }
    }
}

impl fmt::Debug for HeartbeatResult {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeartbeatResult")
            .field("force_exit", &self.force_exit)
            .field("reason", &self.reason)
            .field("lease_payload", &"[REDACTED]")
            .field("lease_signature", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateTokenResponse {
    pub logged_in: bool,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UsageLogUploadResponse {
    pub ok: bool,
    pub received: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperationAuthorization {
    pub allowed: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OnlineSession {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub client_version: String,
    #[serde(default, deserialize_with = "deserialize_lenient_i64")]
    pub connected_at: i64,
    #[serde(default, deserialize_with = "deserialize_lenient_i64")]
    pub last_seen_at: i64,
    #[serde(default, deserialize_with = "deserialize_lenient_i64")]
    pub duration_seconds: i64,
    #[serde(default, deserialize_with = "deserialize_lenient_bool")]
    pub is_self: bool,
}

/// Accepts a JSON number or a numeric string for the online-session timestamp
/// fields; anything else (or a missing field, via `#[serde(default)]`) becomes
/// 0, matching the WPF `OtaApiClient.GetInt64`.
fn deserialize_lenient_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Number(number) => number.as_i64().unwrap_or(0),
        Value::String(text) => text.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    })
}

/// Treats only a literal JSON `true` as self; every other value is false,
/// matching the WPF `OtaApiClient` (`ValueKind == True`).
fn deserialize_lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(matches!(value, Value::Bool(true)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct OnlineResponse {
    sessions: Option<Vec<OnlineSession>>,
}

#[derive(Debug, Clone)]
pub struct CloudflareClient {
    base_url: String,
    app_version: String,
    http: Client,
    pinned: Option<PinnedApiClient>,
    #[cfg(debug_assertions)]
    injected_lease_key: Option<VerifyingKey>,
}

impl CloudflareClient {
    /// Explicit non-production constructor for wiremock and local adapters.
    #[cfg(debug_assertions)]
    pub fn new_injected(base_url: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            app_version: app_version.into(),
            // A request that accepts the TCP connection but never returns must
            // not hang a poll loop (or the online page's requestInFlight guard)
            // forever; the WPF client bounded the same requests via the
            // HttpClient default timeout.
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client should build"),
            pinned: None,
            injected_lease_key: None,
        }
    }

    /// Explicit debug-only key injection for runtime-generated signing tests.
    #[cfg(debug_assertions)]
    pub fn new_injected_with_lease_key(
        base_url: impl Into<String>,
        app_version: impl Into<String>,
        verifying_key: VerifyingKey,
    ) -> Self {
        let mut client = Self::new_injected(base_url, app_version);
        client.injected_lease_key = Some(verifying_key);
        client
    }

    #[cfg(debug_assertions)]
    pub fn new_pinned(
        policy: ApiTlsPolicy,
        app_version: impl Into<String>,
    ) -> CloudflareResult<Self> {
        Self::from_pinned_policy(policy, app_version)
    }

    fn from_pinned_policy(
        policy: ApiTlsPolicy,
        app_version: impl Into<String>,
    ) -> CloudflareResult<Self> {
        let pinned = PinnedApiClient::new(policy)?;
        Ok(Self {
            base_url: pinned.base_url().to_string(),
            app_version: app_version.into(),
            http: pinned.http_client(),
            pinned: Some(pinned),
            #[cfg(debug_assertions)]
            injected_lease_key: None,
        })
    }

    pub fn new_default() -> CloudflareResult<Self> {
        let policy = ApiTlsPolicy::production().map_err(CloudflareError::Integrity)?;
        Self::from_pinned_policy(policy, DEFAULT_APP_VERSION)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
        identity: &ProcessIdentity,
        session_id: &str,
    ) -> CloudflareResult<LoginResult> {
        if username.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "username 不能为空".to_string(),
            ));
        }
        if password.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "password 不能为空".to_string(),
            ));
        }
        if session_id.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "session id 不能为空".to_string(),
            ));
        }

        let request = LoginRequest::new(
            username,
            password,
            self.app_version(),
            identity.build_id(),
            identity.process_nonce(),
            session_id,
        );
        let response = self
            .send_request(Method::POST, "/api/login", None, Some(&request))
            .await?;
        self.deserialize_response::<LoginResult>(response).await
    }

    pub async fn validate_token(&self, token: &str) -> CloudflareResult<Option<String>> {
        let response = self
            .send_request(
                Method::GET,
                "/api/me",
                Some(token),
                Option::<&LoginRequest>::None,
            )
            .await?;
        self.deserialize_validate_token(response).await
    }

    pub async fn resolve_rom(
        &self,
        token: &str,
        pd: &str,
        version: &str,
    ) -> CloudflareResult<RomResolveResponse> {
        self.ensure_integrity_ready()?;
        if pd.trim().is_empty() || version.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "pd/version 不能为空".to_string(),
            ));
        }

        let request_url = format!(
            "{}?pd={}&version={}",
            self.url("/api/rom"),
            urlencoding::encode(pd),
            urlencoding::encode(version)
        );

        let response = self
            .http
            .get(request_url)
            .headers(self.default_headers(Some(token))?)
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        self.ensure_integrity_response(&response)?;
        let body = self.handle_api_error(response).await?;
        self.parse_json(body).await
    }

    pub async fn heartbeat(
        &self,
        token: &str,
        identity: &ProcessIdentity,
        session_id: &str,
        sequence: u64,
        active: bool,
    ) -> CloudflareResult<HeartbeatResult> {
        if session_id.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "session id 不能为空".to_string(),
            ));
        }

        let response = if active {
            self.send_request(
                Method::POST,
                "/api/heartbeat",
                Some(token),
                Some(&HeartbeatRequest {
                    session_id: session_id.to_string(),
                    client_version: self.app_version().to_string(),
                    build_id: identity.build_id().to_string(),
                    process_nonce: identity.process_nonce().to_string(),
                    sequence,
                    active: true,
                }),
            )
            .await?
        } else {
            self.send_request(
                Method::POST,
                "/api/heartbeat",
                Some(token),
                Some(&GoodbyeRequest {
                    session_id: session_id.to_string(),
                    active: false,
                }),
            )
            .await?
        };
        self.deserialize_response::<HeartbeatResult>(response).await
    }

    pub(crate) fn verify_session_lease(
        &self,
        envelope: &SignedEnvelope,
    ) -> CloudflareResult<VerifiedLease> {
        let verified = if let Some(pinned) = self.pinned.as_ref() {
            pinned.verify_session_lease(envelope)
        } else {
            #[cfg(debug_assertions)]
            {
                let key = self.injected_lease_key.as_ref().ok_or({
                    CloudflareError::Integrity(
                        crate::pinned_tls::IntegrityFailure::MissingVerificationKey,
                    )
                })?;
                verify_signed_lease(envelope, key)
            }
            #[cfg(not(debug_assertions))]
            {
                return Err(CloudflareError::Integrity(
                    crate::pinned_tls::IntegrityFailure::MissingVerificationKey,
                ));
            }
        };

        verified.map_err(map_lease_verification_error)
    }

    pub async fn get_online(&self, token: &str) -> CloudflareResult<Vec<OnlineSession>> {
        let response = self
            .send_request(
                Method::GET,
                "/api/online",
                Some(token),
                Option::<&LoginRequest>::None,
            )
            .await?;
        let body = self.handle_api_error(response).await?;
        let payload = self.parse_json::<OnlineResponse>(body).await?;
        Ok(payload.sessions.unwrap_or_default())
    }

    pub async fn authorize_operation(
        &self,
        token: &str,
        operation: &str,
        title: &str,
    ) -> CloudflareResult<OperationAuthorization> {
        if operation.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "operation 不能为空".to_string(),
            ));
        }

        let response = self
            .send_request(
                Method::POST,
                "/api/operation/authorize",
                Some(token),
                Some(&serde_json::json!({
                    "operation": operation,
                    "title": title,
                })),
            )
            .await?;
        self.deserialize_response::<OperationAuthorization>(response)
            .await
    }

    pub async fn upload_usage_logs(
        &self,
        token: &str,
        logs: &[UsageLogEntry],
    ) -> CloudflareResult<UsageLogUploadResponse> {
        let body = serde_json::json!({ "logs": logs });
        let response = self
            .send_request(Method::POST, "/api/usage/logs", Some(token), Some(&body))
            .await?;

        self.deserialize_response::<UsageLogUploadResponse>(response)
            .await
    }

    pub async fn report_integrity(
        &self,
        token: Option<&SecretToken>,
        request: &IntegrityReportRequest,
    ) -> CloudflareResult<()> {
        request.validate()?;
        let response = self
            .send_request(
                Method::POST,
                "/api/integrity/report",
                token.map(SecretToken::as_str),
                Some(request),
            )
            .await?;
        self.handle_api_error(response).await?;
        Ok(())
    }

    pub async fn check_version_policy(
        &self,
    ) -> CloudflareResult<super::version_client::VersionCheckResponse> {
        self.ensure_integrity_ready()?;
        let mut request_url = Url::parse(&self.url("/api/app/version"))
            .map_err(|err| CloudflareError::Transport(format!("构造请求失败: {err}")))?;
        request_url
            .query_pairs_mut()
            .append_pair("current", self.app_version());

        let response = self
            .http
            .get(request_url)
            .headers(self.default_headers(None)?)
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        self.ensure_integrity_response(&response)?;
        let body = self.handle_api_error(response).await?;
        self.parse_json::<super::version_client::VersionCheckResponse>(body)
            .await
    }

    pub async fn refresh_pinset(&self) -> CloudflareResult<PinsetClaims> {
        self.pinned
            .as_ref()
            .ok_or_else(|| {
                CloudflareError::InvalidInput(
                    "pinset refresh is unavailable for an injected client".to_string(),
                )
            })?
            .refresh_pinset()
            .await
    }

    async fn send_request<T: Serialize + ?Sized>(
        &self,
        method: Method,
        relative_path: &str,
        token: Option<&str>,
        body: Option<&T>,
    ) -> CloudflareResult<Response> {
        self.ensure_integrity_ready()?;
        let builder = self
            .http
            .request(method, self.url(relative_path))
            .headers(self.default_headers(token)?);

        let request = if let Some(body) = body {
            builder.json(body)
        } else {
            builder
        };

        // reqwest/rustls necessarily own serialized header/body bytes while encrypting and
        // transmitting the request. Client-owned bearer/password intermediates remain
        // zeroizing, and the Authorization value is marked sensitive for library Debug output.

        let response = request.send().await.map_err(classify_reqwest_error)?;
        self.ensure_integrity_response(&response)?;

        Ok(response)
    }

    fn ensure_integrity_ready(&self) -> CloudflareResult<()> {
        if let Some(pinned) = self.pinned.as_ref() {
            pinned.ensure_active()?;
        }
        Ok(())
    }

    fn ensure_integrity_response(&self, response: &Response) -> CloudflareResult<()> {
        if let Some(pinned) = self.pinned.as_ref() {
            pinned.validate_response(response)?;
        }
        Ok(())
    }

    async fn handle_api_error(
        &self,
        response: Response,
    ) -> CloudflareResult<ZeroizingResponseBody> {
        let status = response.status();
        let status_code = status.as_u16();

        let text =
            ZeroizingResponseBody::new(response.text().await.map_err(classify_reqwest_error)?);

        if status == StatusCode::UPGRADE_REQUIRED {
            let update = self.parse_update_required(text.as_str())?;
            return Err(CloudflareError::UpdateRequired(update));
        }

        if !status.is_success() {
            let message = fallback_message(status_code);
            return Err(CloudflareError::ApiError {
                status: status_code,
                message,
            });
        }

        Ok(text)
    }

    async fn parse_json<T: DeserializeOwned>(
        &self,
        body: ZeroizingResponseBody,
    ) -> CloudflareResult<T> {
        serde_json::from_str(body.as_str())
            .map_err(|err| CloudflareError::InvalidResponse(format!("响应格式无效: {err}")))
    }

    async fn deserialize_response<T: DeserializeOwned>(
        &self,
        response: Response,
    ) -> CloudflareResult<T> {
        let text = self.handle_api_error(response).await?;
        self.parse_json(text).await
    }

    async fn deserialize_validate_token(
        &self,
        response: Response,
    ) -> CloudflareResult<Option<String>> {
        let body = self.handle_api_error(response).await?;
        let payload = self.parse_json::<ValidateTokenResponse>(body).await?;
        if payload.logged_in {
            Ok(payload.name)
        } else {
            Ok(None)
        }
    }

    fn parse_update_required(&self, text: &str) -> CloudflareResult<UpdateRequiredInfo> {
        let root = serde_json::from_str::<Value>(text)
            .map_err(|error| CloudflareError::InvalidResponse(format!("响应格式无效: {error}")))?;

        let get = |name: &str| {
            root.get(name)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        Ok(UpdateRequiredInfo {
            message: "需要更新 VivoKsu 后才能继续使用。".to_string(),
            latest: get("latest"),
            min_version: get("min"),
            download_url: get("download_url"),
        })
    }

    #[cfg(debug_assertions)]
    pub fn authenticated_headers_for_test(&self, token: &str) -> CloudflareResult<HeaderMap> {
        self.default_headers(Some(token))
    }

    fn default_headers(&self, token: Option<&str>) -> CloudflareResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        let app_version = HeaderValue::from_str(&self.app_version)
            .map_err(|_| CloudflareError::InvalidInput("客户端版本头格式无效。".to_string()))?;
        headers.insert("X-Nwflash-Version", app_version);

        if let Some(token) = token {
            if token.is_empty() {
                return Err(CloudflareError::InvalidInput(
                    "认证令牌格式无效。".to_string(),
                ));
            }
            let bearer = Zeroizing::new(format!("Bearer {token}"));
            let mut authorization = HeaderValue::from_str(&bearer)
                .map_err(|_| CloudflareError::InvalidInput("认证令牌格式无效。".to_string()))?;
            authorization.set_sensitive(true);
            headers.insert(AUTHORIZATION, authorization);
        }

        Ok(headers)
    }

    fn url(&self, relative_path: &str) -> String {
        if relative_path.starts_with("http://") || relative_path.starts_with("https://") {
            return relative_path.to_string();
        }
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            relative_path.trim_start_matches('/')
        )
    }
}

fn map_lease_verification_error(error: LeaseVerificationError) -> CloudflareError {
    use crate::pinned_tls::IntegrityFailure;

    let failure = match error {
        LeaseVerificationError::MalformedEnvelope => IntegrityFailure::LeaseEnvelope,
        LeaseVerificationError::InvalidSignature => IntegrityFailure::LeaseSignature,
        LeaseVerificationError::MalformedClaims => IntegrityFailure::LeaseClaims,
    };
    CloudflareError::Integrity(failure)
}

fn valid_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_client_version(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn fallback_message(status: u16) -> String {
    match status {
        404 => "未找到对应版本的 ROM。".to_string(),
        402 => "服务端信用点不足,无法解析下载链接。".to_string(),
        401 => "服务端认证失败。".to_string(),
        400 => "查询参数不合法。".to_string(),
        429 => "请求过于频繁,请稍后再试。".to_string(),
        _ => "服务端返回错误。".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use zeroize::Zeroize as _;

    use super::ZeroizingResponseBody;

    #[test]
    fn aggregate_response_body_is_redacted_and_explicitly_zeroizable() {
        let mut body = ZeroizingResponseBody::new("response-secret".to_string());

        assert!(!format!("{body:?}").contains("response-secret"));
        body.zeroize();
        assert!(body.is_empty());
    }
}
