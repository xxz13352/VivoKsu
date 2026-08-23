use std::{
    error::Error as StdError,
    fmt::{self, Debug, Formatter},
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    Engine as _,
};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{Client, Response};
use rustls::{
    client::{
        danger::HandshakeSignatureValid, danger::ServerCertVerified, danger::ServerCertVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, Error as RustlsError, OtherError, RootCertStore,
    SignatureScheme,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use x509_parser::parse_x509_certificate;

use crate::api_client::{CloudflareError, CloudflareResult};

pub const API_HOST: &str = "api.nwflash.cc.cd";
pub const BUILTIN_LEAF_SPKI_PIN: &str = "kavrs5Bk3Tjn+0G+uPjWGBqJsXzW5kHFNPzgxuvrcKY=";
pub const BUILTIN_WE1_SPKI_PIN: &str = "kIdp6NNEd8wsugYyyIYFsi1ylMCED3hZbSR8ZFsa/A4=";
/// Lowest signed pinset version accepted at process startup. Task 8 release
/// maintenance raises this alongside the embedded verification key when needed.
pub const EMBEDDED_PINSET_VERSION_FLOOR: u64 = 1;

const API_BASE_URL: &str = "https://api.nwflash.cc.cd";
const PINSET_CACHE_FILE: &str = "nwflash-api-pinset.json";
const MAX_PINSET_CACHE_BYTES: u64 = 16 * 1024;

type SpkiDigest = [u8; 32];

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum IntegrityFailure {
    #[error("API TLS endpoint policy rejected the configuration")]
    InvalidApiEndpoint,
    #[error("API TLS pin policy is malformed")]
    InvalidPinset,
    #[error("API TLS pin validation failed")]
    SpkiMismatch,
    #[error("API pinset signature validation failed")]
    PinsetSignature,
    #[error("API pinset host validation failed")]
    PinsetHost,
    #[error("API pinset validity window failed")]
    PinsetTime,
    #[error("API pinset version rollback was rejected")]
    PinsetRollback,
    #[error("API pinset cache validation failed")]
    PinsetCache,
    #[error("API pinset envelope is malformed")]
    PinsetEnvelope,
    #[error("API session verification key is not configured")]
    MissingVerificationKey,
    #[error("API session verification key is malformed")]
    InvalidVerificationKey,
    #[error("API TLS client construction failed")]
    TlsConfiguration,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedPinsetEnvelope {
    pub pinset_payload: String,
    pub pinset_signature: String,
}

impl Debug for SignedPinsetEnvelope {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignedPinsetEnvelope")
            .field("pinset_payload", &"[REDACTED]")
            .field("pinset_signature", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PinsetClaims {
    pub version: u64,
    pub host: String,
    pub not_before: i64,
    pub expires_at: i64,
    pub primary_pin: String,
    pub backup_pin: String,
}

impl Debug for PinsetClaims {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinsetClaims")
            .field("version", &self.version)
            .field("host", &self.host)
            .field("not_before", &self.not_before)
            .field("expires_at", &self.expires_at)
            .field("primary_pin", &"[REDACTED]")
            .field("backup_pin", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
pub struct ApiTlsPolicy {
    base_url: Url,
    roots: RootCertStore,
    initial_pins: Vec<SpkiDigest>,
    verifying_key: VerifyingKey,
    cache_path: Option<PathBuf>,
    resolve_to: Option<SocketAddr>,
    embedded_version_floor: u64,
}

impl Debug for ApiTlsPolicy {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiTlsPolicy")
            .field("base_url", &self.base_url)
            .field("root_count", &self.roots.len())
            .field("initial_pins", &"[REDACTED]")
            .field("cache_path", &self.cache_path)
            .field("resolve_to", &self.resolve_to)
            .finish_non_exhaustive()
    }
}

impl ApiTlsPolicy {
    pub fn production() -> Result<Self, IntegrityFailure> {
        let encoded_key = option_env!("NWFLASH_SESSION_VERIFY_KEY_B64")
            .ok_or(IntegrityFailure::MissingVerificationKey)?;
        let verifying_key = decode_verifying_key(encoded_key)?;
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::injected_with_floor(
            API_BASE_URL,
            roots,
            vec![
                BUILTIN_LEAF_SPKI_PIN.to_string(),
                BUILTIN_WE1_SPKI_PIN.to_string(),
            ],
            verifying_key,
            Some(default_cache_path()),
            None,
            EMBEDDED_PINSET_VERSION_FLOOR,
        )
    }

    pub fn injected(
        base_url: impl AsRef<str>,
        roots: RootCertStore,
        initial_pins: Vec<String>,
        verifying_key: VerifyingKey,
        cache_path: Option<PathBuf>,
        resolve_to: Option<SocketAddr>,
    ) -> Result<Self, IntegrityFailure> {
        Self::injected_with_floor(
            base_url,
            roots,
            initial_pins,
            verifying_key,
            cache_path,
            resolve_to,
            EMBEDDED_PINSET_VERSION_FLOOR,
        )
    }

    /// Explicit dependency-injected construction for local integration tests.
    /// Production callers must use [`Self::production`].
    pub fn injected_with_floor(
        base_url: impl AsRef<str>,
        roots: RootCertStore,
        initial_pins: Vec<String>,
        verifying_key: VerifyingKey,
        cache_path: Option<PathBuf>,
        resolve_to: Option<SocketAddr>,
        embedded_version_floor: u64,
    ) -> Result<Self, IntegrityFailure> {
        if embedded_version_floor == 0 {
            return Err(IntegrityFailure::InvalidPinset);
        }
        let base_url = validate_api_base_url(base_url.as_ref())?;
        let initial_pins = decode_pinset(initial_pins.iter().map(String::as_str))?;
        Ok(Self {
            base_url,
            roots,
            initial_pins,
            verifying_key,
            cache_path,
            resolve_to,
            embedded_version_floor,
        })
    }
}

#[derive(Clone)]
pub struct PinnedApiClient {
    base_url: Url,
    http: Client,
    pinsets: Arc<PinsetController>,
}

impl Debug for PinnedApiClient {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedApiClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl PinnedApiClient {
    pub fn new(policy: ApiTlsPolicy) -> CloudflareResult<Self> {
        let pinsets = Arc::new(PinsetController::new(
            policy.initial_pins,
            policy.verifying_key,
            policy.cache_path,
            policy.embedded_version_floor,
        ));
        pinsets
            .load_cached(unix_now())
            .map_err(CloudflareError::Integrity)?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(policy.roots),
            provider.clone(),
        )
        .build()
        .map_err(|_| CloudflareError::Integrity(IntegrityFailure::TlsConfiguration))?;
        let verifier = Arc::new(SpkiServerVerifier {
            webpki,
            pins: pinsets.state.clone(),
        });
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| CloudflareError::Integrity(IntegrityFailure::TlsConfiguration))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        config.key_log = Arc::new(rustls::NoKeyLog);
        debug_assert!(!config.key_log.will_log("CLIENT_RANDOM"));

        let mut builder = Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .https_only(true)
            .use_preconfigured_tls(config);
        if let Some(address) = policy.resolve_to {
            builder = builder.resolve(API_HOST, address);
        }
        let http = builder
            .build()
            .map_err(|_| CloudflareError::Integrity(IntegrityFailure::TlsConfiguration))?;

        Ok(Self {
            base_url: policy.base_url,
            http,
            pinsets,
        })
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str().trim_end_matches('/')
    }

    pub async fn get_text(&self, relative_path: &str) -> CloudflareResult<String> {
        let response = self.get(relative_path).await?;
        response.text().await.map_err(classify_reqwest_error)
    }

    pub async fn get(&self, relative_path: &str) -> CloudflareResult<Response> {
        let url = self.url(relative_path)?;
        self.http
            .get(url)
            .send()
            .await
            .map_err(classify_reqwest_error)
    }

    pub async fn refresh_pinset_at(&self, now: i64) -> CloudflareResult<PinsetClaims> {
        let body = self.get_text("/api/security/pins").await?;
        let envelope = serde_json::from_str::<SignedPinsetEnvelope>(&body)
            .map_err(|_| CloudflareError::Integrity(IntegrityFailure::PinsetEnvelope))?;
        self.install_pinset_at(envelope, now)
            .map_err(CloudflareError::Integrity)
    }

    pub async fn refresh_pinset(&self) -> CloudflareResult<PinsetClaims> {
        self.refresh_pinset_at(unix_now()).await
    }

    pub fn install_pinset_at(
        &self,
        envelope: SignedPinsetEnvelope,
        now: i64,
    ) -> Result<PinsetClaims, IntegrityFailure> {
        self.pinsets.install(envelope, now, true)
    }

    pub fn reload_cached_pinset_at(&self, now: i64) -> Result<(), IntegrityFailure> {
        self.pinsets.load_cached(now)
    }

    pub(crate) fn http_client(&self) -> Client {
        self.http.clone()
    }

    fn url(&self, relative_path: &str) -> CloudflareResult<Url> {
        if !relative_path.starts_with('/') || relative_path.starts_with("//") {
            return Err(CloudflareError::Integrity(
                IntegrityFailure::InvalidApiEndpoint,
            ));
        }
        let mut url = self.base_url.clone();
        url.set_path(relative_path);
        url.set_query(None);
        url.set_fragment(None);
        if url.scheme() != "https" || url.host_str() != Some(API_HOST) {
            return Err(CloudflareError::Integrity(
                IntegrityFailure::InvalidApiEndpoint,
            ));
        }
        Ok(url)
    }
}

#[derive(Debug)]
struct PinState {
    version: u64,
    payload: Option<String>,
    pins: Vec<SpkiDigest>,
}

#[derive(Debug)]
struct PinsetController {
    state: Arc<RwLock<PinState>>,
    verifying_key: VerifyingKey,
    cache_path: Option<PathBuf>,
    update_lock: Mutex<()>,
}

impl PinsetController {
    fn new(
        initial_pins: Vec<SpkiDigest>,
        verifying_key: VerifyingKey,
        cache_path: Option<PathBuf>,
        embedded_version_floor: u64,
    ) -> Self {
        // The embedded floor and this in-process high-water mark reject cache
        // rollback at startup and during refresh. Because the only persisted
        // state is the signed public envelope, replacing it before process
        // startup with another valid envelope at or above the embedded floor
        // is inherently indistinguishable; release maintenance raises the
        // embedded floor when stronger rollback exclusion is required.
        Self {
            state: Arc::new(RwLock::new(PinState {
                version: embedded_version_floor,
                payload: None,
                pins: initial_pins,
            })),
            verifying_key,
            cache_path,
            update_lock: Mutex::new(()),
        }
    }

    fn load_cached(&self, now: i64) -> Result<(), IntegrityFailure> {
        let Some(path) = self.cache_path.as_deref() else {
            return Ok(());
        };
        match fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_PINSET_CACHE_BYTES => {
                return Err(IntegrityFailure::PinsetCache)
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(IntegrityFailure::PinsetCache),
        }
        let bytes = fs::read(path).map_err(|_| IntegrityFailure::PinsetCache)?;
        let envelope = serde_json::from_slice::<SignedPinsetEnvelope>(&bytes)
            .map_err(|_| IntegrityFailure::PinsetEnvelope)?;
        self.install(envelope, now, false).map(|_| ())
    }

    fn install(
        &self,
        envelope: SignedPinsetEnvelope,
        now: i64,
        persist: bool,
    ) -> Result<PinsetClaims, IntegrityFailure> {
        let _update = self
            .update_lock
            .lock()
            .map_err(|_| IntegrityFailure::PinsetCache)?;
        let (claims, pins) = verify_pinset(&envelope, &self.verifying_key, now)?;
        {
            let state = self
                .state
                .read()
                .map_err(|_| IntegrityFailure::PinsetCache)?;
            if claims.version < state.version
                || (claims.version == state.version
                    && state
                        .payload
                        .as_deref()
                        .is_some_and(|payload| payload != envelope.pinset_payload))
            {
                return Err(IntegrityFailure::PinsetRollback);
            }
        }

        if persist {
            if let Some(path) = self.cache_path.as_deref() {
                persist_envelope(path, &envelope)?;
            }
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| IntegrityFailure::PinsetCache)?;
        state.version = claims.version;
        state.payload = Some(envelope.pinset_payload);
        state.pins = pins;
        Ok(claims)
    }
}

#[derive(Debug)]
struct SpkiServerVerifier {
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
    pins: Arc<RwLock<PinState>>,
}

impl ServerCertVerifier for SpkiServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let verified = self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;

        if !matches!(server_name, ServerName::DnsName(name) if name.as_ref() == API_HOST) {
            return Err(RustlsError::General(
                "API server name violates the endpoint policy".to_string(),
            ));
        }

        let pins = self
            .pins
            .read()
            .map_err(|_| RustlsError::General("API pin state unavailable".to_string()))?;
        for certificate in std::iter::once(end_entity).chain(intermediates) {
            let (remainder, parsed) =
                parse_x509_certificate(certificate.as_ref()).map_err(|_| {
                    RustlsError::InvalidCertificate(rustls::CertificateError::BadEncoding)
                })?;
            if !remainder.is_empty() {
                return Err(RustlsError::InvalidCertificate(
                    rustls::CertificateError::BadEncoding,
                ));
            }
            let digest: SpkiDigest = Sha256::digest(parsed.public_key().raw).into();
            if pins.pins.iter().any(|pin| pin == &digest) {
                return Ok(verified);
            }
        }

        Err(RustlsError::Other(OtherError(Arc::new(SpkiMismatchMarker))))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.webpki.requires_raw_public_keys()
    }
}

#[derive(Debug)]
struct SpkiMismatchMarker;

impl fmt::Display for SpkiMismatchMarker {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("API SPKI policy rejected the certificate chain")
    }
}

impl StdError for SpkiMismatchMarker {}

pub(crate) fn classify_reqwest_error(error: reqwest::Error) -> CloudflareError {
    if error_is_spki_mismatch(&error) {
        CloudflareError::Integrity(IntegrityFailure::SpkiMismatch)
    } else {
        CloudflareError::Transport(error.to_string())
    }
}

fn error_is_spki_mismatch(error: &(dyn StdError + 'static)) -> bool {
    error_contains_spki_mismatch(error, 0)
}

fn error_contains_spki_mismatch(error: &(dyn StdError + 'static), depth: usize) -> bool {
    if depth > 12 {
        return false;
    }
    if error.downcast_ref::<SpkiMismatchMarker>().is_some() {
        return true;
    }
    if let Some(RustlsError::Other(other)) = error.downcast_ref::<RustlsError>() {
        if other.0.downcast_ref::<SpkiMismatchMarker>().is_some() {
            return true;
        }
    }
    if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
        if io_error
            .get_ref()
            .is_some_and(|inner| error_contains_spki_mismatch(inner, depth + 1))
        {
            return true;
        }
    }
    error
        .source()
        .is_some_and(|source| error_contains_spki_mismatch(source, depth + 1))
}

fn verify_pinset(
    envelope: &SignedPinsetEnvelope,
    verifying_key: &VerifyingKey,
    now: i64,
) -> Result<(PinsetClaims, Vec<SpkiDigest>), IntegrityFailure> {
    if !envelope.pinset_payload.is_ascii() {
        return Err(IntegrityFailure::PinsetSignature);
    }
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(&envelope.pinset_signature)
        .map_err(|_| IntegrityFailure::PinsetSignature)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| IntegrityFailure::PinsetSignature)?;
    verifying_key
        .verify_strict(envelope.pinset_payload.as_bytes(), &signature)
        .map_err(|_| IntegrityFailure::PinsetSignature)?;

    let payload = URL_SAFE_NO_PAD
        .decode(&envelope.pinset_payload)
        .map_err(|_| IntegrityFailure::PinsetEnvelope)?;
    let claims = serde_json::from_slice::<PinsetClaims>(&payload)
        .map_err(|_| IntegrityFailure::PinsetEnvelope)?;
    if claims.version == 0 {
        return Err(IntegrityFailure::PinsetRollback);
    }
    if claims.host != API_HOST {
        return Err(IntegrityFailure::PinsetHost);
    }
    if claims.not_before >= claims.expires_at || now < claims.not_before || now >= claims.expires_at
    {
        return Err(IntegrityFailure::PinsetTime);
    }
    let pins = decode_pinset([claims.primary_pin.as_str(), claims.backup_pin.as_str()])?;
    if pins[0] == pins[1] {
        return Err(IntegrityFailure::InvalidPinset);
    }
    Ok((claims, pins))
}

fn decode_pinset<'a>(
    encoded_pins: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<SpkiDigest>, IntegrityFailure> {
    let pins = encoded_pins
        .into_iter()
        .map(|encoded| {
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| IntegrityFailure::InvalidPinset)?;
            bytes
                .try_into()
                .map_err(|_| IntegrityFailure::InvalidPinset)
        })
        .collect::<Result<Vec<SpkiDigest>, _>>()?;
    if pins.is_empty() {
        return Err(IntegrityFailure::InvalidPinset);
    }
    Ok(pins)
}

fn decode_verifying_key(encoded: &str) -> Result<VerifyingKey, IntegrityFailure> {
    let bytes = STANDARD
        .decode(encoded.trim())
        .or_else(|_| STANDARD_NO_PAD.decode(encoded.trim()))
        .map_err(|_| IntegrityFailure::InvalidVerificationKey)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| IntegrityFailure::InvalidVerificationKey)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| IntegrityFailure::InvalidVerificationKey)
}

fn validate_api_base_url(base_url: &str) -> Result<Url, IntegrityFailure> {
    let url = Url::parse(base_url).map_err(|_| IntegrityFailure::InvalidApiEndpoint)?;
    if url.scheme() != "https"
        || url.host_str() != Some(API_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(IntegrityFailure::InvalidApiEndpoint);
    }
    Ok(url)
}

fn persist_envelope(
    cache_path: &Path,
    envelope: &SignedPinsetEnvelope,
) -> Result<(), IntegrityFailure> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).map_err(|_| IntegrityFailure::PinsetCache)?;
    }
    let bytes = serde_json::to_vec(envelope).map_err(|_| IntegrityFailure::PinsetEnvelope)?;
    fs::write(cache_path, bytes).map_err(|_| IntegrityFailure::PinsetCache)
}

fn default_cache_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("VivoKsu")
        .join(PINSET_CACHE_FILE)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_key_decoder_fails_closed_for_malformed_values() {
        assert_eq!(
            decode_verifying_key("not-base64"),
            Err(IntegrityFailure::InvalidVerificationKey)
        );
        assert_eq!(
            decode_verifying_key(&STANDARD.encode([0_u8; 31])),
            Err(IntegrityFailure::InvalidVerificationKey)
        );
    }
}
