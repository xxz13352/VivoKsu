use std::fmt::{self, Display, Formatter};

#[cfg(debug_assertions)]
use std::time::Duration;

use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, Method, Response, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;

use nwflash_domain::UsageLogEntry;

use crate::pinned_tls::{classify_reqwest_error, ApiTlsPolicy, PinnedApiClient, PinsetClaims};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResult {
    pub token: String,
    pub username: String,
    pub name: String,
    #[serde(default)]
    pub lease_payload: String,
    #[serde(default)]
    pub lease_signature: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    pub session_id: String,
    pub client_version: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HeartbeatResult {
    pub force_exit: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub lease_payload: String,
    #[serde(default)]
    pub lease_signature: String,
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
        }
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

    pub async fn login(&self, username: &str, password: &str) -> CloudflareResult<LoginResult> {
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

        let response = self
            .send_request(
                Method::POST,
                "/api/login",
                None,
                Some(&LoginRequest {
                    username: username.to_string(),
                    password: password.to_string(),
                }),
            )
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
            .headers(self.default_headers(Some(token)))
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
        session_id: &str,
        active: bool,
    ) -> CloudflareResult<HeartbeatResult> {
        if session_id.trim().is_empty() {
            return Err(CloudflareError::InvalidInput(
                "session id 不能为空".to_string(),
            ));
        }

        let response = self
            .send_request(
                Method::POST,
                "/api/heartbeat",
                Some(token),
                Some(&HeartbeatRequest {
                    session_id: session_id.to_string(),
                    client_version: self.app_version().to_string(),
                    active,
                }),
            )
            .await?;
        self.deserialize_response::<HeartbeatResult>(response).await
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
            .headers(self.default_headers(None))
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
            .headers(self.default_headers(token));

        let request = if let Some(body) = body {
            builder.json(body)
        } else {
            builder
        };

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

    async fn handle_api_error(&self, response: Response) -> CloudflareResult<String> {
        let status = response.status();
        let status_code = status.as_u16();

        let text = response.text().await.map_err(classify_reqwest_error)?;

        if status == StatusCode::UPGRADE_REQUIRED {
            let update = self.parse_update_required(&text)?;
            return Err(CloudflareError::UpdateRequired(update));
        }

        if !status.is_success() {
            let message = self
                .parse_error_message(&text)
                .unwrap_or_else(|| fallback_message(status_code));
            return Err(CloudflareError::ApiError {
                status: status_code,
                message,
            });
        }

        Ok(text)
    }

    async fn parse_json<T: DeserializeOwned>(&self, body: String) -> CloudflareResult<T> {
        serde_json::from_str(&body)
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

    fn parse_error_message(&self, text: &str) -> Option<String> {
        let root = serde_json::from_str::<Value>(text).ok()?;
        root.get("error")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    }

    fn parse_update_required(&self, text: &str) -> CloudflareResult<UpdateRequiredInfo> {
        let root = serde_json::from_str::<Value>(text)
            .map_err(|error| CloudflareError::InvalidResponse(format!("响应格式无效: {error}")))?;

        let get = |name: &str| {
            root.get(name)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        let fallback = "需要更新 VivoKsu 后才能继续使用。".to_string();
        Ok(UpdateRequiredInfo {
            message: get("error").unwrap_or(fallback),
            latest: get("latest"),
            min_version: get("min"),
            download_url: get("download_url"),
        })
    }

    fn default_headers(&self, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let app_version = HeaderValue::from_str(&self.app_version)
            .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_APP_VERSION));
        headers.insert("X-Nwflash-Version", app_version);

        if let Some(token) = token {
            if !token.trim().is_empty() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))
                        .expect("固定构造的 Authorization 头应始终有效"),
                );
            }
        }

        headers
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
