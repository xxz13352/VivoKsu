use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Deserializer};
use zeroize::{Zeroize, Zeroizing};

use crate::pinned_tls::IntegrityFailure;

const RANDOM_ID_BYTES: usize = 24;

/// A bearer token whose owned storage is zeroized on replacement and drop.
pub struct SecretToken(Zeroizing<String>);

impl SecretToken {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Creates the only supported owned copy: another zeroizing request scope.
    pub fn request_scope(&self) -> Self {
        Self::new(self.0.to_string())
    }

    pub fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for SecretToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// One Rust-owned build/process binding generated when AppState is created.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    build_id: String,
    process_nonce: String,
}

impl ProcessIdentity {
    pub fn generate() -> Result<Self, IntegrityFailure> {
        #[cfg(debug_assertions)]
        let build_id = option_env!("NWFLASH_BUILD_ID").unwrap_or("debug-build");
        #[cfg(not(debug_assertions))]
        let build_id =
            option_env!("NWFLASH_BUILD_ID").ok_or(IntegrityFailure::MissingBuildIdentity)?;

        Self::new(build_id, random_identifier("")?)
    }

    #[cfg(debug_assertions)]
    pub fn new_injected(
        build_id: impl Into<String>,
        process_nonce: impl Into<String>,
    ) -> Result<Self, IntegrityFailure> {
        Self::new(build_id, process_nonce)
    }

    fn new(
        build_id: impl Into<String>,
        process_nonce: impl Into<String>,
    ) -> Result<Self, IntegrityFailure> {
        let build_id = build_id.into();
        let process_nonce = process_nonce.into();
        if !valid_bound_identifier(&build_id, 128) || !valid_bound_identifier(&process_nonce, 128) {
            return Err(IntegrityFailure::InvalidProcessIdentity);
        }
        Ok(Self {
            build_id,
            process_nonce,
        })
    }

    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    pub fn process_nonce(&self) -> &str {
        &self.process_nonce
    }

    pub fn fresh_session_id(&self) -> Result<String, IntegrityFailure> {
        random_identifier("session-")
    }
}

impl fmt::Debug for ProcessIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessIdentity")
            .field("build_id", &self.build_id)
            .field("process_nonce", &"[REDACTED]")
            .finish()
    }
}

fn random_identifier(prefix: &str) -> Result<String, IntegrityFailure> {
    let mut bytes = Zeroizing::new([0_u8; RANDOM_ID_BYTES]);
    OsRng
        .try_fill_bytes(bytes.as_mut())
        .map_err(|_| IntegrityFailure::ProcessRandomness)?;
    Ok(format!(
        "{prefix}{}",
        URL_SAFE_NO_PAD.encode(bytes.as_slice())
    ))
}

fn valid_bound_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::ProcessIdentity;

    #[test]
    fn generated_process_and_session_identifiers_are_fresh_and_server_safe() {
        let first = ProcessIdentity::generate().expect("process identity should generate");
        let second = ProcessIdentity::generate().expect("process identity should generate");
        let first_session = first
            .fresh_session_id()
            .expect("session id should generate");
        let second_session = first
            .fresh_session_id()
            .expect("session id should generate");

        assert_ne!(first.process_nonce(), second.process_nonce());
        assert_ne!(first_session, second_session);
        assert!(first_session.len() <= 64);
    }
}
