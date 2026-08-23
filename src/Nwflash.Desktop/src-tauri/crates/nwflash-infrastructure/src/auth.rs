use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_protection::{
    accept_login_lease, classify_heartbeat_lease, HeartbeatDecision, LeaseBinding, LeaseRejection,
    SessionLease, TokenDigest,
};

use crate::{
    api_client::{CloudflareClient, CloudflareError, CloudflareResult, LoginResult},
    pinned_tls::IntegrityFailure,
    ProcessIdentity, SecretToken,
};

#[derive(Debug, Clone)]
pub struct AuthService {
    client: CloudflareClient,
}

pub struct AuthSession {
    pub token: SecretToken,
    pub username: String,
    pub name: String,
    pub lease: SessionLease,
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthSession")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("name", &self.name)
            .field("lease", &self.lease)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatAdmission {
    Accepted(SessionLease),
    ForceExit(String),
    Goodbye,
}

impl AuthService {
    #[cfg(debug_assertions)]
    pub fn new(base_url: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            client: CloudflareClient::new_injected(base_url, app_version),
        }
    }

    pub fn with_client(client: CloudflareClient) -> Self {
        Self { client }
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
        identity: &ProcessIdentity,
        session_id: &str,
    ) -> CloudflareResult<AuthSession> {
        let requested_username = username.trim();
        let result = self
            .client
            .login(requested_username, password, identity, session_id)
            .await?;

        // Cryptographic verification deliberately precedes every claim/binding check.
        let verified = self
            .client
            .verify_session_lease(&result.signed_envelope())?;
        if result.username != requested_username {
            return Err(CloudflareError::Integrity(IntegrityFailure::LeaseBinding));
        }
        let binding = LeaseBinding::new(
            requested_username,
            TokenDigest::sha256(result.token.as_str().as_bytes()),
            self.client.app_version(),
            identity.build_id(),
            identity.process_nonce(),
            session_id,
        );
        let lease =
            accept_login_lease(&verified, &binding, unix_now()).map_err(map_lease_rejection)?;
        if !result.token.is_header_safe() {
            return Err(CloudflareError::InvalidInput(
                "认证令牌格式无效。".to_string(),
            ));
        }

        Ok(AuthSession::from_login(result, lease))
    }

    pub async fn heartbeat(
        &self,
        token: &SecretToken,
        username: &str,
        identity: &ProcessIdentity,
        lease: &SessionLease,
        active: bool,
    ) -> CloudflareResult<HeartbeatAdmission> {
        let result = self
            .client
            .heartbeat(
                token.as_str(),
                identity,
                lease.session_id(),
                lease.sequence(),
                active,
            )
            .await?;

        if !active {
            return Ok(HeartbeatAdmission::Goodbye);
        }
        if result.force_exit {
            return Ok(HeartbeatAdmission::ForceExit(
                result
                    .reason
                    .unwrap_or_else(|| "服务端要求退出".to_string()),
            ));
        }

        // As with login, no payload fields are considered until Ed25519 succeeds.
        let verified = self
            .client
            .verify_session_lease(&result.signed_envelope())?;
        let binding = LeaseBinding::new(
            username,
            TokenDigest::sha256(token.as_str().as_bytes()),
            self.client.app_version(),
            identity.build_id(),
            identity.process_nonce(),
            lease.session_id(),
        );
        match classify_heartbeat_lease(&verified, &binding, lease.sequence(), unix_now()) {
            HeartbeatDecision::Continue(next) => Ok(HeartbeatAdmission::Accepted(next)),
            HeartbeatDecision::ExitPending(reason) => Err(map_lease_rejection(reason)),
        }
    }

    pub async fn validate_token(&self, token: &str) -> CloudflareResult<Option<String>> {
        match self.client.validate_token(token).await {
            Ok(name) => Ok(name),
            Err(CloudflareError::UpdateRequired(info)) => {
                Err(CloudflareError::UpdateRequired(info))
            }
            Err(CloudflareError::Integrity(error)) => Err(CloudflareError::Integrity(error)),
            Err(_) => Ok(None),
        }
    }

    pub fn default_client() -> Self {
        let client = CloudflareClient::new_default()
            .unwrap_or_else(|_| panic!("pinned API client initialization failed closed"));
        Self::with_client(client)
    }

    pub fn client(&self) -> &CloudflareClient {
        &self.client
    }
}

impl AuthSession {
    fn from_login(result: LoginResult, lease: SessionLease) -> Self {
        Self {
            token: result.token,
            username: result.username,
            name: result.name,
            lease,
        }
    }
}

fn map_lease_rejection(error: LeaseRejection) -> CloudflareError {
    let failure = match error {
        LeaseRejection::UnsupportedVersion | LeaseRejection::WrongKind => {
            IntegrityFailure::LeaseKind
        }
        LeaseRejection::UsernameMismatch
        | LeaseRejection::TokenDigestMismatch
        | LeaseRejection::ClientVersionMismatch
        | LeaseRejection::BuildIdMismatch
        | LeaseRejection::ProcessNonceMismatch
        | LeaseRejection::SessionIdMismatch => IntegrityFailure::LeaseBinding,
        LeaseRejection::Expired
        | LeaseRejection::IssuedInFuture
        | LeaseRejection::InvalidTimeWindow => IntegrityFailure::LeaseTime,
        LeaseRejection::SequenceRollback => IntegrityFailure::LeaseSequence,
    };
    CloudflareError::Integrity(failure)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
