use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use crate::{build_identity_matches, MarkerBoundary, MarkerScope};

/// The maximum accepted clock skew for a server-issued lease.
pub const MAX_CLOCK_SKEW_SECONDS: i64 = 60;

/// The signed wire envelope returned by the authentication API.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedEnvelope {
    pub lease_payload: String,
    pub lease_signature: String,
}

impl fmt::Debug for SignedEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignedEnvelope")
            .field("lease_payload", &"[REDACTED]")
            .field("lease_signature", &"[REDACTED]")
            .finish()
    }
}

/// Claims contained in a signed session lease.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseClaims {
    pub version: u8,
    pub kind: LeaseKind,
    pub username: String,
    pub token_sha256: TokenDigest,
    pub client_version: String,
    pub build_id: String,
    pub process_nonce: String,
    pub session_id: String,
    pub sequence: u64,
    pub issued_at: i64,
    pub expires_at: i64,
}

impl fmt::Debug for LeaseClaims {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeaseClaims")
            .field("version", &self.version)
            .field("kind", &self.kind)
            .field("username", &self.username)
            .field("token_sha256", &"[REDACTED]")
            .field("client_version", &self.client_version)
            .field("build_id", &self.build_id)
            .field("process_nonce", &self.process_nonce)
            .field("session_id", &self.session_id)
            .field("sequence", &self.sequence)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The only signed lease kinds accepted by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    Login,
    Heartbeat,
}

/// An opaque lease that can only be created by successful signature verification.
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedLease(LeaseClaims);

impl VerifiedLease {
    pub fn claims(&self) -> &LeaseClaims {
        &self.0
    }
}

/// A SHA-256 token digest. Its debug representation never reveals digest bytes.
pub struct TokenDigest {
    bytes: SecretBuffer<[u8; 32]>,
}

impl TokenDigest {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            bytes: SecretBuffer::new(bytes),
        }
    }

    /// Hashes a bearer token before it enters a normalized lease decision.
    pub fn sha256(token: &[u8]) -> Self {
        let digest = Zeroizing::new(Sha256::digest(token).to_vec());
        Self::from_decoded(digest).expect("SHA-256 digest is always 32 bytes")
    }

    /// Replaces the digest after zeroizing the previous bytes.
    pub fn replace(&mut self, replacement: [u8; 32]) {
        self.bytes.replace(replacement);
    }

    fn from_decoded(decoded: Zeroizing<Vec<u8>>) -> Option<Self> {
        if decoded.len() != 32 {
            return None;
        }

        let mut bytes = Zeroizing::new([0_u8; 32]);
        bytes.copy_from_slice(&decoded);
        Some(Self {
            bytes: SecretBuffer::from_zeroizing(bytes),
        })
    }

    fn ct_eq(&self, other: &Self) -> bool {
        self.bytes
            .value
            .as_slice()
            .ct_eq(other.bytes.value.as_slice())
            .into()
    }
}

impl PartialEq for TokenDigest {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other)
    }
}

impl Eq for TokenDigest {}

impl Serialize for TokenDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(self.bytes.value.as_slice()));
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for TokenDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = Zeroizing::new(String::deserialize(deserializer)?);
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded.as_bytes())
                .map_err(D::Error::custom)?,
        );
        Self::from_decoded(decoded)
            .ok_or_else(|| D::Error::custom("token SHA-256 must be 32 bytes"))
    }
}

impl fmt::Debug for TokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenDigest([REDACTED])")
    }
}

/// The normalized client/session values to which a lease must be bound.
#[derive(Debug)]
pub struct LeaseBinding {
    username: String,
    token_sha256: TokenDigest,
    client_version: String,
    build_id: String,
    process_nonce: String,
    session_id: String,
}

impl LeaseBinding {
    pub fn new(
        username: impl Into<String>,
        token_sha256: TokenDigest,
        client_version: impl Into<String>,
        build_id: impl Into<String>,
        process_nonce: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            token_sha256,
            client_version: client_version.into(),
            build_id: build_id.into(),
            process_nonce: process_nonce.into(),
            session_id: session_id.into(),
        }
    }
}

/// Reasons a signed lease is rejected after cryptographic verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRejection {
    UnsupportedVersion,
    WrongKind,
    UsernameMismatch,
    TokenDigestMismatch,
    ClientVersionMismatch,
    BuildIdMismatch,
    ProcessNonceMismatch,
    SessionIdMismatch,
    Expired,
    IssuedInFuture,
    InvalidTimeWindow,
    SequenceRollback,
}

/// Wire-format failures that occur before a verified lease can exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseVerificationError {
    MalformedEnvelope,
    InvalidSignature,
    MalformedClaims,
}

/// A locally admitted session capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLease {
    expires_at: i64,
    sequence: u64,
    session_id: String,
}

impl SessionLease {
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Heartbeat processing has only a continue or terminal outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatDecision {
    Continue(SessionLease),
    ExitPending(LeaseRejection),
}

/// Local operations are either admitted or denied without side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationDecision {
    Allow,
    DenyExpired,
}

/// Verifies an Ed25519 signature over the original base64url payload ASCII bytes.
#[inline(never)]
#[export_name = "nwflash_protection_verify_signed_lease"]
pub fn verify_signed_lease(
    envelope: &SignedEnvelope,
    verifying_key: &VerifyingKey,
) -> Result<VerifiedLease, LeaseVerificationError> {
    if !envelope.lease_payload.is_ascii() {
        return Err(LeaseVerificationError::MalformedEnvelope);
    }

    let signature_bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&envelope.lease_signature)
            .map_err(|_| LeaseVerificationError::MalformedEnvelope)?,
    );
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| LeaseVerificationError::MalformedEnvelope)?;

    verifying_key
        .verify(envelope.lease_payload.as_bytes(), &signature)
        .map_err(|_| LeaseVerificationError::InvalidSignature)?;

    let payload_bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&envelope.lease_payload)
            .map_err(|_| LeaseVerificationError::MalformedEnvelope)?,
    );
    let claims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| LeaseVerificationError::MalformedClaims)?;

    Ok(VerifiedLease(claims))
}

/// Accepts a verified login lease only when every session binding matches.
#[inline(never)]
#[export_name = "nwflash_protection_accept_login_lease"]
pub fn accept_login_lease(
    lease: &VerifiedLease,
    binding: &LeaseBinding,
    now: i64,
) -> Result<SessionLease, LeaseRejection> {
    let _marker = MarkerScope::enter(MarkerBoundary::LoginLeaseAcceptance);
    validate_lease(lease.claims(), binding, LeaseKind::Login, now, None)
}

/// Classifies a verified heartbeat lease and fails closed on sequence rollback.
#[inline(never)]
#[export_name = "nwflash_protection_classify_heartbeat_lease"]
pub fn classify_heartbeat_lease(
    lease: &VerifiedLease,
    binding: &LeaseBinding,
    previous_sequence: u64,
    now: i64,
) -> HeartbeatDecision {
    let _marker = MarkerScope::enter(MarkerBoundary::HeartbeatLeaseClassification);
    match validate_lease(
        lease.claims(),
        binding,
        LeaseKind::Heartbeat,
        now,
        Some(previous_sequence),
    ) {
        Ok(session) => HeartbeatDecision::Continue(session),
        Err(reason) => HeartbeatDecision::ExitPending(reason),
    }
}

/// Rechecks a locally held capability before a sensitive operation begins.
#[inline(never)]
#[export_name = "nwflash_protection_admit_local_operation"]
pub fn admit_local_operation(session: &SessionLease, now: i64) -> OperationDecision {
    let _marker = MarkerScope::enter(MarkerBoundary::OperationAdmission);
    if session.expires_at <= now {
        OperationDecision::DenyExpired
    } else {
        OperationDecision::Allow
    }
}

fn validate_lease(
    claims: &LeaseClaims,
    binding: &LeaseBinding,
    expected_kind: LeaseKind,
    now: i64,
    previous_sequence: Option<u64>,
) -> Result<SessionLease, LeaseRejection> {
    if claims.version != 1 {
        return Err(LeaseRejection::UnsupportedVersion);
    }
    if claims.kind != expected_kind {
        return Err(LeaseRejection::WrongKind);
    }
    if claims.username != binding.username {
        return Err(LeaseRejection::UsernameMismatch);
    }
    if !binding.token_sha256.ct_eq(&claims.token_sha256) {
        return Err(LeaseRejection::TokenDigestMismatch);
    }
    if claims.client_version != binding.client_version {
        return Err(LeaseRejection::ClientVersionMismatch);
    }
    if !build_identity_matches(&binding.build_id, &claims.build_id) {
        return Err(LeaseRejection::BuildIdMismatch);
    }
    if claims.process_nonce != binding.process_nonce {
        return Err(LeaseRejection::ProcessNonceMismatch);
    }
    if claims.session_id != binding.session_id {
        return Err(LeaseRejection::SessionIdMismatch);
    }
    if claims.expires_at <= claims.issued_at {
        return Err(LeaseRejection::InvalidTimeWindow);
    }
    if claims.issued_at > now.saturating_add(MAX_CLOCK_SKEW_SECONDS) {
        return Err(LeaseRejection::IssuedInFuture);
    }
    if claims.expires_at <= now {
        return Err(LeaseRejection::Expired);
    }
    if previous_sequence.is_some_and(|sequence| claims.sequence <= sequence) {
        return Err(LeaseRejection::SequenceRollback);
    }

    Ok(SessionLease {
        expires_at: claims.expires_at,
        sequence: claims.sequence,
        session_id: claims.session_id.clone(),
    })
}

struct SecretBuffer<T: Zeroize> {
    value: Zeroizing<T>,
}

impl<T: Zeroize> SecretBuffer<T> {
    fn new(value: T) -> Self {
        Self {
            value: Zeroizing::new(value),
        }
    }

    fn from_zeroizing(value: Zeroizing<T>) -> Self {
        Self { value }
    }

    fn replace(&mut self, replacement: T) {
        self.value = Zeroizing::new(replacement);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use zeroize::Zeroize;

    use super::SecretBuffer;

    struct ZeroizeProbe(Rc<Cell<u8>>);

    impl Zeroize for ZeroizeProbe {
        fn zeroize(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn secret_buffer_zeroizes_replaced_and_dropped_values() {
        let replaced = Rc::new(Cell::new(0));
        let dropped = Rc::new(Cell::new(0));
        let mut buffer = SecretBuffer::new(ZeroizeProbe(replaced.clone()));

        buffer.replace(ZeroizeProbe(dropped.clone()));
        assert_eq!(replaced.get(), 1);
        drop(buffer);
        assert_eq!(dropped.get(), 1);
    }
}
