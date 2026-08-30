use std::fmt;

use nwflash_domain::{
    TraceApiErrorCodeV2, TraceApiErrorV2, TraceApiResponseV2, TraceId, TraceRejectedCodeV2,
    TraceRejectedEntityV2, TraceRejectedItemV2, TraceUploadResponseV2, TRACE_TEXT_MAX_BYTES,
    TRACE_UPLOAD_MAX_BODY_BYTES,
};
use nwflash_protection::{SentinelAttestedTraceUpload, TraceRedactionError};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use crate::{CloudflareClient, CloudflareError, SecretToken};

pub type TraceHttpResult<T = TraceHttpOutcome> = Result<T, TraceHttpError>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TraceHttpError {
    InvalidCredential,
    InvalidSealedBody,
    Transport,
    Integrity,
    InvalidResponse,
    ResponseTooLarge,
    UnexpectedStatus(u16),
}

impl fmt::Debug for TraceHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for TraceHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredential => formatter.write_str("trace credential is invalid"),
            Self::InvalidSealedBody => formatter.write_str("sealed trace body is invalid"),
            Self::Transport => formatter.write_str("trace transport failed"),
            Self::Integrity => formatter.write_str("trace transport integrity failed"),
            Self::InvalidResponse => formatter.write_str("trace response is invalid"),
            Self::ResponseTooLarge => formatter.write_str("trace response exceeds its limit"),
            Self::UnexpectedStatus(status) => {
                write!(formatter, "trace response status {status} is unsupported")
            }
        }
    }
}

impl std::error::Error for TraceHttpError {}

#[derive(Clone, PartialEq, Eq)]
pub struct TraceSafeId(TraceId);

impl TraceSafeId {
    pub const fn id(&self) -> TraceId {
        self.0
    }
}

impl fmt::Debug for TraceSafeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TraceSafeId([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TraceSafeRejectedItem {
    entity: TraceRejectedEntityV2,
    id: Option<TraceId>,
    code: TraceRejectedCodeV2,
}

impl TraceSafeRejectedItem {
    fn from_domain(item: &TraceRejectedItemV2) -> Self {
        Self {
            entity: item.entity(),
            id: item.id(),
            code: item.code(),
        }
    }

    pub const fn entity(&self) -> TraceRejectedEntityV2 {
        self.entity
    }

    pub const fn id(&self) -> Option<TraceId> {
        self.id
    }

    pub const fn code(&self) -> TraceRejectedCodeV2 {
        self.code
    }
}

impl fmt::Debug for TraceSafeRejectedItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceSafeRejectedItem")
            .field("entity", &self.entity)
            .field("has_id", &self.id.is_some())
            .field("code", &self.code)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TraceHttpAck {
    accepted_runs: Vec<TraceSafeId>,
    accepted_events: Vec<TraceSafeId>,
    accepted_output_chunks: Vec<TraceSafeId>,
    rejected: Vec<TraceSafeRejectedItem>,
}

impl TraceHttpAck {
    fn from_domain(response: TraceUploadResponseV2) -> Self {
        Self {
            accepted_runs: response
                .accepted()
                .runs()
                .iter()
                .copied()
                .map(TraceSafeId)
                .collect(),
            accepted_events: response
                .accepted()
                .events()
                .iter()
                .copied()
                .map(TraceSafeId)
                .collect(),
            accepted_output_chunks: response
                .accepted()
                .output_chunks()
                .iter()
                .copied()
                .map(TraceSafeId)
                .collect(),
            rejected: response
                .rejected()
                .iter()
                .map(TraceSafeRejectedItem::from_domain)
                .collect(),
        }
    }

    pub fn accepted_runs(&self) -> &[TraceSafeId] {
        &self.accepted_runs
    }

    pub fn accepted_events(&self) -> &[TraceSafeId] {
        &self.accepted_events
    }

    pub fn accepted_output_chunks(&self) -> &[TraceSafeId] {
        &self.accepted_output_chunks
    }

    pub fn rejected(&self) -> &[TraceSafeRejectedItem] {
        &self.rejected
    }
}

impl fmt::Debug for TraceHttpAck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceHttpAck")
            .field("run_count", &self.accepted_runs().len())
            .field("event_count", &self.accepted_events().len())
            .field("output_chunk_count", &self.accepted_output_chunks().len())
            .field("rejected_count", &self.rejected().len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TraceHttpApiError {
    code: TraceApiErrorCodeV2,
    details: Vec<TraceSafeRejectedItem>,
}

impl TraceHttpApiError {
    fn from_domain(error: TraceApiErrorV2) -> Self {
        Self {
            code: error.error().code(),
            details: error
                .error()
                .details()
                .unwrap_or_default()
                .iter()
                .map(TraceSafeRejectedItem::from_domain)
                .collect(),
        }
    }

    pub const fn code(&self) -> TraceApiErrorCodeV2 {
        self.code
    }

    pub fn details(&self) -> &[TraceSafeRejectedItem] {
        &self.details
    }
}

impl fmt::Debug for TraceHttpApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceHttpApiError")
            .field("code", &self.code)
            .field("detail_count", &self.details.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TraceHttpUpdateRequired {
    latest: Option<String>,
    min_version: Option<String>,
    download_url: Option<String>,
}

impl TraceHttpUpdateRequired {
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    pub fn min_version(&self) -> Option<&str> {
        self.min_version.as_deref()
    }

    pub fn download_url(&self) -> Option<&str> {
        self.download_url.as_deref()
    }
}

impl fmt::Debug for TraceHttpUpdateRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TraceHttpUpdateRequired([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum TraceHttpOutcome {
    Accepted(TraceHttpAck),
    InvalidRequest(TraceHttpApiError),
    Unauthorized(TraceHttpApiError),
    Forbidden(TraceHttpApiError),
    OwnershipConflict(TraceHttpApiError),
    BodyTooLarge(TraceHttpApiError),
    Incomplete(TraceHttpApiError),
    UpdateRequired(TraceHttpUpdateRequired),
    RateLimited {
        error: Option<TraceHttpApiError>,
    },
    ServerFailure {
        status: u16,
        error: Option<TraceHttpApiError>,
    },
}

impl TraceHttpOutcome {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Accepted(_) => 200,
            Self::InvalidRequest(_) => 400,
            Self::Unauthorized(_) => 401,
            Self::Forbidden(_) => 403,
            Self::OwnershipConflict(_) => 409,
            Self::BodyTooLarge(_) => 413,
            Self::Incomplete(_) => 422,
            Self::UpdateRequired(_) => 426,
            Self::RateLimited { .. } => 429,
            Self::ServerFailure { status, .. } => *status,
        }
    }

    pub fn api_error(&self) -> Option<&TraceHttpApiError> {
        match self {
            Self::InvalidRequest(error)
            | Self::Unauthorized(error)
            | Self::Forbidden(error)
            | Self::OwnershipConflict(error)
            | Self::BodyTooLarge(error)
            | Self::Incomplete(error) => Some(error),
            Self::RateLimited { error } | Self::ServerFailure { error, .. } => error.as_ref(),
            Self::Accepted(_) | Self::UpdateRequired(_) => None,
        }
    }
}

impl fmt::Debug for TraceHttpOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted(ack) => formatter
                .debug_struct("TraceHttpOutcome::Accepted")
                .field("run_count", &ack.accepted_runs().len())
                .field("event_count", &ack.accepted_events().len())
                .field("output_chunk_count", &ack.accepted_output_chunks().len())
                .field("rejected_count", &ack.rejected().len())
                .finish(),
            Self::UpdateRequired(_) => formatter.write_str("TraceHttpOutcome::UpdateRequired"),
            other => formatter
                .debug_struct("TraceHttpOutcome::Rejected")
                .field("status", &other.status_code())
                .field("code", &other.api_error().map(TraceHttpApiError::code))
                .field(
                    "detail_count",
                    &other.api_error().map_or(0, |error| error.details().len()),
                )
                .finish(),
        }
    }
}

pub(crate) struct ZeroizingTraceRequestBody(Zeroizing<Vec<u8>>);

impl ZeroizingTraceRequestBody {
    #[cfg(test)]
    pub(crate) fn new(value: Vec<u8>) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(crate) fn from_attested(
        upload: &SentinelAttestedTraceUpload,
    ) -> Result<Self, TraceRedactionError> {
        let body = upload.to_json_body()?;
        if body.len() > TRACE_UPLOAD_MAX_BODY_BYTES {
            return Err(TraceRedactionError::RequestTooLarge);
        }
        Ok(Self(body))
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ZeroizingTraceRequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ZeroizingTraceRequestBody([REDACTED])")
    }
}

impl Zeroize for ZeroizingTraceRequestBody {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl CloudflareClient {
    /// Sends one protection-attested trace attempt exactly once.
    ///
    /// ```compile_fail
    /// use nwflash_infrastructure::{CloudflareClient, SecretToken};
    /// use nwflash_protection::SealedTraceUpload;
    ///
    /// fn raw_upload_cannot_reach_http(
    ///     client: &CloudflareClient,
    ///     token: &SecretToken,
    ///     raw: SealedTraceUpload,
    /// ) {
    ///     let _ = client.upload_trace_v2(token, raw);
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use nwflash_infrastructure::{CloudflareClient, SecretToken};
    /// use nwflash_protection::SentinelAttestedTraceUpload;
    ///
    /// async fn one_receipt_cannot_be_sent_twice(
    ///     client: &CloudflareClient,
    ///     token: &SecretToken,
    ///     upload: SentinelAttestedTraceUpload,
    /// ) {
    ///     let _ = client.upload_trace_v2(token, upload).await;
    ///     let _ = client.upload_trace_v2(token, upload).await;
    /// }
    /// ```
    pub async fn upload_trace_v2(
        &self,
        owner_token: &SecretToken,
        upload: SentinelAttestedTraceUpload,
    ) -> TraceHttpResult {
        let response = self.send_trace_upload_v2(owner_token, upload).await?;
        let status = response.status().as_u16();
        let body = read_bounded_response(response).await?;
        if status == 426 {
            return parse_update_required(&body);
        }
        if status == 429 || (500..=599).contains(&status) {
            return map_retryable_gateway_response(status, &body);
        }
        if !matches!(
            status,
            200 | 400 | 401 | 403 | 409 | 413 | 422 | 429 | 500..=599
        ) {
            return Err(TraceHttpError::UnexpectedStatus(status));
        }

        let envelope = TraceApiResponseV2::from_json_slice(&body)
            .map_err(|_| TraceHttpError::InvalidResponse)?;
        map_v2_response(status, envelope)
    }
}

pub(crate) fn map_client_error(error: CloudflareError) -> TraceHttpError {
    match error {
        CloudflareError::Integrity(_) => TraceHttpError::Integrity,
        CloudflareError::InvalidInput(_) => TraceHttpError::InvalidCredential,
        _ => TraceHttpError::Transport,
    }
}

async fn read_bounded_response(
    mut response: reqwest::Response,
) -> TraceHttpResult<Zeroizing<Vec<u8>>> {
    if response
        .content_length()
        .is_some_and(|length| length > TRACE_UPLOAD_MAX_BODY_BYTES as u64)
    {
        return Err(TraceHttpError::ResponseTooLarge);
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| TraceHttpError::Transport)?
    {
        if chunk.len() > TRACE_UPLOAD_MAX_BODY_BYTES.saturating_sub(body.len()) {
            return Err(TraceHttpError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_v2_response(status: u16, response: TraceApiResponseV2) -> TraceHttpResult {
    match (status, response) {
        (200, TraceApiResponseV2::Success(response)) => Ok(TraceHttpOutcome::Accepted(
            TraceHttpAck::from_domain(response),
        )),
        (400, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::Invalid)?;
            Ok(TraceHttpOutcome::InvalidRequest(
                TraceHttpApiError::from_domain(error),
            ))
        }
        (401, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::Unauthorized)?;
            Ok(TraceHttpOutcome::Unauthorized(
                TraceHttpApiError::from_domain(error),
            ))
        }
        (403, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::Forbidden)?;
            Ok(TraceHttpOutcome::Forbidden(TraceHttpApiError::from_domain(
                error,
            )))
        }
        (409, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::OwnershipConflict)?;
            Ok(TraceHttpOutcome::OwnershipConflict(
                TraceHttpApiError::from_domain(error),
            ))
        }
        (413, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::BodyTooLarge)?;
            Ok(TraceHttpOutcome::BodyTooLarge(
                TraceHttpApiError::from_domain(error),
            ))
        }
        (422, TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::Incomplete)?;
            Ok(TraceHttpOutcome::Incomplete(
                TraceHttpApiError::from_domain(error),
            ))
        }
        _ => Err(TraceHttpError::InvalidResponse),
    }
}

fn map_retryable_gateway_response(status: u16, body: &[u8]) -> TraceHttpResult {
    let error = match TraceApiResponseV2::from_json_slice(body) {
        Ok(TraceApiResponseV2::Error(error)) => {
            require_code(&error, TraceApiErrorCodeV2::Internal)?;
            Some(TraceHttpApiError::from_domain(error))
        }
        Ok(TraceApiResponseV2::Success(_)) => return Err(TraceHttpError::InvalidResponse),
        Err(_) => None,
    };
    if status == 429 {
        Ok(TraceHttpOutcome::RateLimited { error })
    } else {
        Ok(TraceHttpOutcome::ServerFailure { status, error })
    }
}

fn require_code(error: &TraceApiErrorV2, expected: TraceApiErrorCodeV2) -> TraceHttpResult<()> {
    if error.error().code() == expected {
        Ok(())
    } else {
        Err(TraceHttpError::InvalidResponse)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequiredWire {
    error: String,
    code: String,
    latest: Option<String>,
    #[serde(rename = "min")]
    min_version: Option<String>,
    download_url: Option<String>,
}

fn parse_update_required(body: &[u8]) -> TraceHttpResult {
    let wire: UpdateRequiredWire =
        serde_json::from_slice(body).map_err(|_| TraceHttpError::InvalidResponse)?;
    if wire.code != "UPDATE_REQUIRED"
        || wire.error.is_empty()
        || wire.error.len() > TRACE_TEXT_MAX_BYTES
        || [
            wire.latest.as_deref(),
            wire.min_version.as_deref(),
            wire.download_url.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.is_empty() || value.len() > TRACE_TEXT_MAX_BYTES)
    {
        return Err(TraceHttpError::InvalidResponse);
    }
    Ok(TraceHttpOutcome::UpdateRequired(TraceHttpUpdateRequired {
        latest: wire.latest,
        min_version: wire.min_version,
        download_url: wire.download_url,
    }))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use nwflash_domain::{TraceApiErrorCodeV2, TraceOutputStreamV2, TRACE_UPLOAD_MAX_BODY_BYTES};
    use nwflash_protection::{ExactSecretSet, SentinelAttestedTraceUpload, TraceOutputSession};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    use zeroize::Zeroize as _;

    use crate::{CloudflareClient, SecretToken};

    use super::{TraceHttpError, TraceHttpOutcome, ZeroizingTraceRequestBody};

    const TOKEN: &str = "owner-scoped-bearer-secret";
    const REQUEST_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const ACCEPTED_CHUNK_ID: &str = "01890f3a-3b4c-7def-8123-456789abcdef";

    fn client(server: &MockServer) -> CloudflareClient {
        CloudflareClient::new_injected(server.uri(), "2.4.6")
    }

    fn sealed_upload() -> SentinelAttestedTraceUpload {
        let event_id = "01890f3a-3b4c-7def-8123-456789abcdea"
            .parse()
            .expect("fixed UUIDv7");
        let mut source = Cursor::new(b"safe-http-body-marker\n".to_vec());
        let session = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut source,
            &ExactSecretSet::empty(),
        )
        .expect("complete stream must seal");
        let sealed = session
            .into_upload_attempts()
            .expect("sealed attempt")
            .remove(0);
        SentinelAttestedTraceUpload::try_from(sealed).expect("sentinel-attested attempt")
    }

    fn success_body() -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "accepted": { "runs": [], "events": [], "output_chunks": [] },
            "rejected": []
        })
    }

    fn error_body(code: &str) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": code,
                "message": "safe server message",
                "request_id": REQUEST_ID,
                "details": []
            }
        })
    }

    async fn respond_once(server: &MockServer, status: u16, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/api/usage/traces/v2"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn posts_only_sealed_body_with_owner_bearer_and_version_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/usage/traces/v2"))
            .and(header("Authorization", format!("Bearer {TOKEN}")))
            .and(header("X-Nwflash-Version", "2.4.6"))
            .and(header("Content-Type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
            .expect(1)
            .mount(&server)
            .await;

        let token = SecretToken::new(TOKEN.to_owned());
        let outcome = client(&server)
            .upload_trace_v2(&token, sealed_upload())
            .await
            .expect("sealed request succeeds");
        assert!(matches!(outcome, TraceHttpOutcome::Accepted(_)));

        let requests = server.received_requests().await.expect("request journal");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.len() <= TRACE_UPLOAD_MAX_BODY_BYTES);
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("sealed JSON");
        assert_eq!(body["schema_version"], 2);
        assert_eq!(body["output_chunks"][0]["text"], "safe-http-body-marker\n");
        assert!(!String::from_utf8_lossy(&requests[0].body).contains(TOKEN));
    }

    #[tokio::test]
    async fn parses_mixed_200_ack_without_collapsing_rejections() {
        let server = MockServer::start().await;
        respond_once(
            &server,
            200,
            serde_json::json!({
                "ok": true,
                "accepted": {
                    "runs": [],
                    "events": [],
                    "output_chunks": [ACCEPTED_CHUNK_ID]
                },
                "rejected": [{
                    "entity": "output_chunk",
                    "id": null,
                    "code": "credential_rejected",
                    "message": "redact and reseal"
                }]
            }),
        )
        .await;

        let outcome = client(&server)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect("mixed ACK is valid");
        let TraceHttpOutcome::Accepted(ack) = outcome else {
            panic!("expected accepted outcome");
        };
        assert_eq!(ack.accepted_output_chunks().len(), 1);
        assert_eq!(
            ack.accepted_output_chunks()[0].id().to_string(),
            ACCEPTED_CHUNK_ID
        );
        assert_eq!(ack.rejected().len(), 1);
        let accepted_debug = format!("{:?}", ack.accepted_output_chunks());
        let rejected_debug = format!("{:?}", ack.rejected());
        assert!(!accepted_debug.contains(ACCEPTED_CHUNK_ID));
        assert!(!rejected_debug.contains("redact and reseal"));
    }

    #[tokio::test]
    async fn maps_frozen_error_statuses_to_distinct_outcomes() {
        let cases = [
            (400, "TRACE_INVALID", "invalid"),
            (401, "TRACE_UNAUTHORIZED", "unauthorized"),
            (403, "TRACE_FORBIDDEN", "forbidden"),
            (409, "TRACE_OWNERSHIP_CONFLICT", "ownership_conflict"),
            (413, "TRACE_BODY_TOO_LARGE", "body_too_large"),
            (422, "TRACE_INCOMPLETE", "incomplete"),
            (429, "TRACE_INTERNAL", "rate_limited"),
            (500, "TRACE_INTERNAL", "server_failure"),
            (503, "TRACE_INTERNAL", "server_failure"),
        ];

        for (status, code, expected) in cases {
            let server = MockServer::start().await;
            respond_once(&server, status, error_body(code)).await;
            let outcome = client(&server)
                .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
                .await
                .expect("strict error envelope");
            let actual = match &outcome {
                TraceHttpOutcome::InvalidRequest(_) => "invalid",
                TraceHttpOutcome::Unauthorized(_) => "unauthorized",
                TraceHttpOutcome::Forbidden(_) => "forbidden",
                TraceHttpOutcome::OwnershipConflict(_) => "ownership_conflict",
                TraceHttpOutcome::BodyTooLarge(_) => "body_too_large",
                TraceHttpOutcome::Incomplete(_) => "incomplete",
                TraceHttpOutcome::RateLimited { .. } => "rate_limited",
                TraceHttpOutcome::ServerFailure { .. } => "server_failure",
                _ => "unexpected",
            };
            assert_eq!(actual, expected, "status {status}");
            let error = outcome.api_error().expect("error envelope retained");
            assert_eq!(error.code(), code_to_enum(code));
        }
    }

    #[tokio::test]
    async fn gateway_429_and_5xx_remain_retryable_without_a_v2_envelope() {
        let rate_limited = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("<html>rate limited</html>"))
            .expect(1)
            .mount(&rate_limited)
            .await;
        let outcome = client(&rate_limited)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect("gateway rate limit is status-authoritative");
        assert!(matches!(
            outcome,
            TraceHttpOutcome::RateLimited { error: None }
        ));

        let unavailable = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string(String::new()))
            .expect(1)
            .mount(&unavailable)
            .await;
        let outcome = client(&unavailable)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect("gateway failure is status-authoritative");
        assert!(matches!(
            outcome,
            TraceHttpOutcome::ServerFailure {
                status: 503,
                error: None
            }
        ));
    }

    #[tokio::test]
    async fn injected_transport_does_not_follow_or_replay_a_307_redirect() {
        let target = MockServer::start().await;
        let source = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/usage/traces/v2"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/redirect-target", target.uri())),
            )
            .expect(1)
            .mount(&source)
            .await;

        assert_eq!(
            client(&source)
                .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
                .await,
            Err(TraceHttpError::UnexpectedStatus(307))
        );
        assert!(
            target
                .received_requests()
                .await
                .expect("target request journal")
                .is_empty(),
            "redirect target must not receive the single-use upload"
        );
    }

    #[tokio::test]
    async fn maps_legacy_426_without_treating_it_as_v2_error() {
        let server = MockServer::start().await;
        respond_once(
            &server,
            426,
            serde_json::json!({
                "error": "update required",
                "code": "UPDATE_REQUIRED",
                "latest": "3.0.0",
                "min": "2.9.0",
                "download_url": "https://download.example/app"
            }),
        )
        .await;

        let outcome = client(&server)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect("legacy update envelope");
        let TraceHttpOutcome::UpdateRequired(update) = outcome else {
            panic!("expected update outcome");
        };
        assert_eq!(update.latest(), Some("3.0.0"));
        assert_eq!(update.min_version(), Some("2.9.0"));
    }

    #[tokio::test]
    async fn rejects_status_envelope_mismatch_and_unexpected_status() {
        let mismatch = MockServer::start().await;
        respond_once(&mismatch, 200, error_body("TRACE_INTERNAL")).await;
        assert_eq!(
            client(&mismatch)
                .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
                .await,
            Err(TraceHttpError::InvalidResponse)
        );

        let unexpected = MockServer::start().await;
        respond_once(&unexpected, 418, error_body("TRACE_INTERNAL")).await;
        assert_eq!(
            client(&unexpected)
                .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
                .await,
            Err(TraceHttpError::UnexpectedStatus(418))
        );
    }

    #[tokio::test]
    async fn rejects_malformed_or_oversized_response_without_reflecting_body() {
        let malformed_secret = "malformed-response-secret";
        let malformed = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("{{not-json:{malformed_secret}}}")),
            )
            .expect(1)
            .mount(&malformed)
            .await;
        let error = client(&malformed)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect_err("malformed body rejected");
        assert_eq!(error, TraceHttpError::InvalidResponse);
        assert!(!format!("{error:?} {error}").contains(malformed_secret));

        let oversized = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                TRACE_UPLOAD_MAX_BODY_BYTES
                    + 1
            ]))
            .expect(1)
            .mount(&oversized)
            .await;
        assert_eq!(
            client(&oversized)
                .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
                .await,
            Err(TraceHttpError::ResponseTooLarge)
        );
    }

    #[tokio::test]
    async fn debug_and_errors_never_expose_token_body_or_response_ids() {
        let server = MockServer::start().await;
        respond_once(
            &server,
            401,
            serde_json::json!({
                "ok": false,
                "error": {
                    "code": "TRACE_UNAUTHORIZED",
                    "message": "server-message-secret",
                    "request_id": REQUEST_ID,
                    "details": [{
                        "entity": "run",
                        "id": ACCEPTED_CHUNK_ID,
                        "code": "invalid",
                        "message": "detail-secret"
                    }]
                }
            }),
        )
        .await;
        let outcome = client(&server)
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect("typed unauthorized");
        let debug = format!("{outcome:?}");
        let nested_debug = format!("{:?}", outcome.api_error().expect("API error"));
        let details_debug = format!("{:?}", outcome.api_error().expect("API error").details());
        assert_eq!(
            outcome.api_error().expect("API error").details()[0]
                .id()
                .expect("detail id")
                .to_string(),
            ACCEPTED_CHUNK_ID
        );
        for forbidden in [
            TOKEN,
            "safe-http-body-marker",
            "server-message-secret",
            "detail-secret",
            REQUEST_ID,
            ACCEPTED_CHUNK_ID,
        ] {
            assert!(!debug.contains(forbidden), "debug leaked {forbidden}");
            assert!(
                !nested_debug.contains(forbidden),
                "nested debug leaked {forbidden}"
            );
            assert!(
                !details_debug.contains(forbidden),
                "details debug leaked {forbidden}"
            );
        }

        let unreachable = CloudflareClient::new_injected("http://127.0.0.1:9", "2.4.6");
        let error = unreachable
            .upload_trace_v2(&SecretToken::new(TOKEN.to_owned()), sealed_upload())
            .await
            .expect_err("closed port must fail");
        assert!(!format!("{error:?} {error}").contains(TOKEN));
    }

    #[test]
    fn request_body_owner_is_redacted_and_explicitly_zeroizable() {
        let secret = b"zeroize-request-body-secret";
        let mut body = ZeroizingTraceRequestBody::new(secret.to_vec());
        assert!(!format!("{body:?}").contains("zeroize-request-body-secret"));
        body.zeroize();
        assert!(body.is_empty());
    }

    fn code_to_enum(code: &str) -> TraceApiErrorCodeV2 {
        match code {
            "TRACE_INVALID" => TraceApiErrorCodeV2::Invalid,
            "TRACE_UNAUTHORIZED" => TraceApiErrorCodeV2::Unauthorized,
            "TRACE_FORBIDDEN" => TraceApiErrorCodeV2::Forbidden,
            "TRACE_OWNERSHIP_CONFLICT" => TraceApiErrorCodeV2::OwnershipConflict,
            "TRACE_BODY_TOO_LARGE" => TraceApiErrorCodeV2::BodyTooLarge,
            "TRACE_INCOMPLETE" => TraceApiErrorCodeV2::Incomplete,
            "TRACE_INTERNAL" => TraceApiErrorCodeV2::Internal,
            other => panic!("unknown fixture code: {other}"),
        }
    }
}
