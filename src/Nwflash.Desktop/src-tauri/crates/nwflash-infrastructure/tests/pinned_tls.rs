use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use nwflash_infrastructure::{
    ApiTlsPolicy, CloudflareClient, CloudflareError, IntegrityFailure, PinnedApiClient,
    SignedPinsetEnvelope, API_HOST, DEFAULT_APP_VERSION,
};
use rand_core::{OsRng, RngCore};
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PublicKeyData,
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    RootCertStore, ServerConfig,
};
use serde_json::json;
use serial_test::serial;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Barrier,
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

const OK_BODY: &str = r#"{"ok":true}"#;

#[derive(Clone)]
struct TestResponse {
    status: &'static str,
    headers: Vec<(String, String)>,
    body: String,
    keep_alive: bool,
}

impl TestResponse {
    fn ok_json(body: String) -> Self {
        Self {
            status: "200 OK",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body,
            keep_alive: false,
        }
    }

    fn keep_alive_json(body: String) -> Self {
        Self {
            keep_alive: true,
            ..Self::ok_json(body)
        }
    }

    fn redirect(location: &str) -> Self {
        Self {
            status: "302 Found",
            headers: vec![("Location".to_string(), location.to_string())],
            body: String::new(),
            keep_alive: false,
        }
    }
}

#[derive(Clone)]
struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    fn new(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }

    fn callback(&self) -> Arc<dyn Fn() -> i64 + Send + Sync> {
        let now = self.0.clone();
        Arc::new(move || now.load(Ordering::SeqCst))
    }
}

struct TestChain {
    root_der: CertificateDer<'static>,
    certificates: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    leaf_pin: String,
    intermediate_pin: String,
}

struct TestTlsServer {
    addr: SocketAddr,
    root_der: CertificateDer<'static>,
    leaf_pin: String,
    intermediate_pin: String,
    task: JoinHandle<()>,
}

impl Drop for TestTlsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestTlsServer {
    async fn start(chain: TestChain, response_body: String) -> Self {
        Self::start_with_response(chain, TestResponse::ok_json(response_body)).await
    }

    async fn start_with_response(chain: TestChain, response: TestResponse) -> Self {
        Self::start_with_versions(
            chain,
            response,
            &[&rustls::version::TLS13, &rustls::version::TLS12],
        )
        .await
    }

    async fn start_tls12(chain: TestChain, response: TestResponse) -> Self {
        Self::start_with_versions(chain, response, &[&rustls::version::TLS12]).await
    }

    async fn start_with_versions(
        chain: TestChain,
        response: TestResponse,
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .expect("test TLS versions should be supported")
            .with_no_client_auth()
            .with_single_cert(chain.certificates, chain.private_key)
            .expect("test TLS certificate chain should be valid");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test TLS server should bind");
        let addr = listener.local_addr().expect("server address should exist");
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let response = response.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    loop {
                        let mut request = Vec::new();
                        let mut chunk = [0_u8; 1024];
                        loop {
                            let Ok(read) = stream.read(&mut chunk).await else {
                                return;
                            };
                            if read == 0 {
                                return;
                            }
                            request.extend_from_slice(&chunk[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let headers = response
                            .headers
                            .iter()
                            .map(|(name, value)| format!("{name}: {value}\r\n"))
                            .collect::<String>();
                        let connection = if response.keep_alive {
                            "keep-alive"
                        } else {
                            "close"
                        };
                        let wire_response = format!(
                            "HTTP/1.1 {}\r\n{}Content-Length: {}\r\nConnection: {}\r\n\r\n{}",
                            response.status,
                            headers,
                            response.body.len(),
                            connection,
                            response.body
                        );
                        if stream.write_all(wire_response.as_bytes()).await.is_err() {
                            return;
                        }
                        if !response.keep_alive {
                            let _ = stream.shutdown().await;
                            return;
                        }
                    }
                });
            }
        });

        Self {
            addr,
            root_der: chain.root_der,
            leaf_pin: chain.leaf_pin,
            intermediate_pin: chain.intermediate_pin,
            task,
        }
    }
}

fn test_chain(host: &str, expired: bool) -> TestChain {
    let root_params = ca_params("NWflash test root");
    let root_key = KeyPair::generate().expect("root key should generate");
    let root_cert = root_params
        .self_signed(&root_key)
        .expect("root certificate should sign");
    let root_issuer = Issuer::new(root_params.clone(), root_key);

    let intermediate_params = ca_params("NWflash test intermediate");
    let intermediate_key = KeyPair::generate().expect("intermediate key should generate");
    let intermediate_pin = spki_pin(intermediate_key.subject_public_key_info());
    let intermediate_cert = intermediate_params
        .signed_by(&intermediate_key, &root_issuer)
        .expect("intermediate certificate should sign");
    let intermediate_issuer = Issuer::new(intermediate_params, intermediate_key);

    let leaf_key = KeyPair::generate().expect("leaf key should generate");
    let leaf_pin = spki_pin(leaf_key.subject_public_key_info());
    let mut leaf_params =
        CertificateParams::new(vec![host.to_string()]).expect("test DNS name should be valid");
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, host);
    leaf_params
        .key_usages
        .push(KeyUsagePurpose::DigitalSignature);
    leaf_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    if expired {
        leaf_params.not_before = date_time_ymd(2019, 1, 1);
        leaf_params.not_after = date_time_ymd(2020, 1, 1);
    } else {
        leaf_params.not_before = date_time_ymd(2025, 1, 1);
        leaf_params.not_after = date_time_ymd(2045, 1, 1);
    }
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &intermediate_issuer)
        .expect("leaf certificate should sign");

    let private_key = PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into();
    TestChain {
        root_der: root_cert.der().clone(),
        certificates: vec![leaf_cert.der().clone(), intermediate_cert.der().clone()],
        private_key,
        leaf_pin,
        intermediate_pin,
    }
}

fn ca_params(common_name: &str) -> CertificateParams {
    let mut params = CertificateParams::new(Vec::<String>::new())
        .expect("empty CA subject alternative names should be valid");
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params.not_before = date_time_ymd(2025, 1, 1);
    params.not_after = date_time_ymd(2045, 1, 1);
    params
}

fn spki_pin(spki_der: Vec<u8>) -> String {
    STANDARD.encode(Sha256::digest(spki_der))
}

fn random_signing_key() -> SigningKey {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    SigningKey::from_bytes(&bytes)
}

fn roots(certificates: impl IntoIterator<Item = CertificateDer<'static>>) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate)
            .expect("test root certificate should be accepted");
    }
    roots
}

fn policy_for(
    server: &TestTlsServer,
    trusted_roots: RootCertStore,
    pins: Vec<String>,
    verifying_key: VerifyingKey,
    cache_path: Option<&Path>,
) -> ApiTlsPolicy {
    ApiTlsPolicy::injected(
        format!("https://{API_HOST}:{}", server.addr.port()),
        trusted_roots,
        pins,
        verifying_key,
        cache_path.map(Path::to_path_buf),
        Some(server.addr),
    )
    .expect("test API policy should be valid")
}

fn policy_for_clock(
    server: &TestTlsServer,
    trusted_roots: RootCertStore,
    pins: Vec<String>,
    verifying_key: VerifyingKey,
    cache_path: Option<&Path>,
    embedded_floor: u64,
    clock: &ManualClock,
) -> ApiTlsPolicy {
    ApiTlsPolicy::injected_with_floor(
        format!("https://{API_HOST}:{}", server.addr.port()),
        trusted_roots,
        pins,
        verifying_key,
        cache_path.map(Path::to_path_buf),
        Some(server.addr),
        embedded_floor,
    )
    .expect("clocked test API policy should be valid")
    .with_clock_for_test(clock.callback())
}

fn signed_pinset(
    signing_key: &SigningKey,
    version: u64,
    now: i64,
    primary_pin: &str,
    backup_pin: &str,
) -> SignedPinsetEnvelope {
    signed_pinset_for_host(
        signing_key,
        version,
        API_HOST,
        now - 60,
        now + 3_600,
        primary_pin,
        backup_pin,
    )
}

fn signed_pinset_for_host(
    signing_key: &SigningKey,
    version: u64,
    host: &str,
    not_before: i64,
    expires_at: i64,
    primary_pin: &str,
    backup_pin: &str,
) -> SignedPinsetEnvelope {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "version": version,
            "host": host,
            "not_before": not_before,
            "expires_at": expires_at,
            "primary_pin": primary_pin,
            "backup_pin": backup_pin,
        }))
        .expect("pinset payload should serialize"),
    );
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
    SignedPinsetEnvelope {
        pinset_payload: payload,
        pinset_signature: signature,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock should be after the epoch")
        .as_secs() as i64
}

#[tokio::test]
#[serial]
async fn valid_webpki_chain_accepts_a_transmitted_intermediate_spki_pin() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.intermediate_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    let client = PinnedApiClient::new(policy).expect("pinned client should build");
    assert_eq!(
        client
            .get_text("/health")
            .await
            .expect("valid WebPKI chain and intermediate SPKI pin should connect"),
        OK_BODY
    );
}

#[tokio::test]
#[serial]
async fn tls12_handshake_uses_the_delegating_webpki_signature_path() {
    let server = TestTlsServer::start_tls12(
        test_chain(API_HOST, false),
        TestResponse::ok_json(OK_BODY.to_string()),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    assert_eq!(
        PinnedApiClient::new(policy)
            .expect("TLS 1.2 pinned client should build")
            .get_text("/health")
            .await
            .expect("TLS 1.2 WebPKI and SPKI checks should complete"),
        OK_BODY
    );
}

#[tokio::test]
#[serial]
async fn dynamic_pinset_expiry_is_checked_before_each_pooled_api_operation() {
    let now = 1_800_000_000;
    let clock = ManualClock::new(now);
    let server = TestTlsServer::start_with_response(
        test_chain(API_HOST, false),
        TestResponse::keep_alive_json(OK_BODY.to_string()),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for_clock(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
        1,
        &clock,
    );
    let client = PinnedApiClient::new(policy).expect("clocked pinned client should build");
    client
        .install_pinset_at(
            signed_pinset_for_host(
                &signing_key,
                2,
                API_HOST,
                now - 1,
                now + 10,
                &server.leaf_pin,
                &server.intermediate_pin,
            ),
            now,
        )
        .expect("currently valid dynamic pinset should install");
    client
        .get_text("/health")
        .await
        .expect("first request should establish a pooled TLS connection");

    clock.set(now + 10);
    assert_eq!(
        client
            .get_text("/health")
            .await
            .expect_err("expired dynamic pinset must block a pooled request"),
        CloudflareError::Integrity(IntegrityFailure::PinsetTime)
    );
}

#[tokio::test]
#[serial]
async fn dynamic_pinset_not_before_is_rechecked_after_a_clock_rollback() {
    let now = 1_800_000_000;
    let clock = ManualClock::new(now);
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for_clock(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
        1,
        &clock,
    );
    let client = PinnedApiClient::new(policy).expect("clocked pinned client should build");
    client
        .install_pinset_at(
            signed_pinset_for_host(
                &signing_key,
                2,
                API_HOST,
                now - 1,
                now + 3_600,
                &server.leaf_pin,
                &server.intermediate_pin,
            ),
            now,
        )
        .expect("currently valid dynamic pinset should install");

    clock.set(now - 2);
    assert_eq!(
        client
            .get_text("/health")
            .await
            .expect_err("not-yet-valid dynamic state must block the request"),
        CloudflareError::Integrity(IntegrityFailure::PinsetTime)
    );
}

#[tokio::test]
#[serial]
async fn expired_dynamic_pinset_can_refresh_only_through_embedded_bootstrap_pins() {
    let now = 1_800_000_000;
    let clock = ManualClock::new(now);
    let signing_key = random_signing_key();
    let chain = test_chain(API_HOST, false);
    let fresh = signed_pinset_for_host(
        &signing_key,
        3,
        API_HOST,
        now + 10,
        now + 3_600,
        &chain.leaf_pin,
        &chain.intermediate_pin,
    );
    let server = TestTlsServer::start_with_response(
        chain,
        TestResponse::keep_alive_json(
            serde_json::to_string(&fresh).expect("fresh pinset should serialize"),
        ),
    )
    .await;
    let policy = policy_for_clock(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
        1,
        &clock,
    );
    let client = PinnedApiClient::new(policy).expect("clocked pinned client should build");
    let expired_primary = STANDARD.encode([0x11_u8; 32]);
    let expired_backup = STANDARD.encode([0x22_u8; 32]);
    client
        .install_pinset_at(
            signed_pinset_for_host(
                &signing_key,
                2,
                API_HOST,
                now - 1,
                now + 5,
                &expired_primary,
                &expired_backup,
            ),
            now,
        )
        .expect("short-lived dynamic pinset should install");

    clock.set(now + 10);
    assert_eq!(
        client
            .get_text("/health")
            .await
            .expect_err("expired dynamic pinset should block ordinary API requests"),
        CloudflareError::Integrity(IntegrityFailure::PinsetTime)
    );
    assert_eq!(
        client
            .refresh_pinset()
            .await
            .expect("embedded bootstrap pins should recover a fresh signed pinset")
            .version,
        3
    );
    client
        .get_text("/health")
        .await
        .expect("ordinary requests should resume after a fresh pinset installs");
}

#[tokio::test]
#[serial]
async fn injected_pinned_transport_drives_the_cloudflare_api_adapter() {
    let server = TestTlsServer::start(
        test_chain(API_HOST, false),
        serde_json::to_string(&json!({
            "latest": "1.0.1",
            "min": "1.0.0",
            "download_url": null,
            "update_required": false,
            "force_update": false
        }))
        .expect("version response should serialize"),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    let client = CloudflareClient::new_pinned(policy, DEFAULT_APP_VERSION)
        .expect("Cloudflare adapter should accept an injected pinned policy");
    let version = client
        .check_version_policy()
        .await
        .expect("normal API methods should use the pinned transport");
    assert_eq!(version.latest.as_deref(), Some("1.0.1"));
}

#[tokio::test]
#[serial]
async fn cloudflare_adapter_classifies_redirect_as_endpoint_integrity_failure() {
    let server = TestTlsServer::start_with_response(
        test_chain(API_HOST, false),
        TestResponse::redirect("https://redirect.invalid/version"),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );
    let client = CloudflareClient::new_pinned(policy, DEFAULT_APP_VERSION)
        .expect("Cloudflare adapter should accept a pinned policy");

    assert_eq!(
        client
            .check_version_policy()
            .await
            .expect_err("redirect response must be an endpoint integrity failure"),
        CloudflareError::Integrity(IntegrityFailure::InvalidApiEndpoint)
    );
}

#[tokio::test]
#[serial]
async fn valid_webpki_chain_with_the_wrong_pin_is_an_integrity_failure() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![STANDARD.encode([0xA5_u8; 32])],
        signing_key.verifying_key(),
        None,
    );

    let error = PinnedApiClient::new(policy)
        .expect("client construction should succeed")
        .get_text("/health")
        .await
        .expect_err("wrong SPKI pin must fail");
    assert_eq!(
        error,
        CloudflareError::Integrity(IntegrityFailure::SpkiMismatch)
    );
    assert!(!format!("{error}").contains(&server.leaf_pin));
}

#[tokio::test]
#[serial]
async fn a_webpki_trusted_private_proxy_ca_cannot_bypass_the_pin() {
    let proxy = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &proxy,
        roots([proxy.root_der.clone()]),
        vec![STANDARD.encode([0x5A_u8; 32])],
        signing_key.verifying_key(),
        None,
    );

    let error = PinnedApiClient::new(policy)
        .expect("client construction should succeed")
        .get_text("/health")
        .await
        .expect_err("a trusted private proxy chain must still satisfy the SPKI pin");
    assert_eq!(
        error,
        CloudflareError::Integrity(IntegrityFailure::SpkiMismatch)
    );
}

#[tokio::test]
#[serial]
async fn wrong_dns_name_is_rejected_by_webpki_before_pin_matching() {
    let server = TestTlsServer::start(
        test_chain("not-api.nwflash.invalid", false),
        OK_BODY.to_string(),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    let error = PinnedApiClient::new(policy)
        .expect("client construction should succeed")
        .get_text("/health")
        .await
        .expect_err("wrong DNS certificate must fail WebPKI");
    assert!(matches!(error, CloudflareError::Transport(_)));
}

#[tokio::test]
#[serial]
async fn expired_certificate_is_rejected_by_webpki_before_pin_matching() {
    let server = TestTlsServer::start(test_chain(API_HOST, true), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    let error = PinnedApiClient::new(policy)
        .expect("client construction should succeed")
        .get_text("/health")
        .await
        .expect_err("expired certificate must fail WebPKI");
    assert!(matches!(error, CloudflareError::Transport(_)));
}

#[tokio::test]
#[serial]
async fn pinned_client_ignores_https_proxy_environment_variables() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );
    let unused_proxy = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("unused proxy port should bind")
        .local_addr()
        .expect("unused proxy address should exist");
    let proxy_url = format!("http://{unused_proxy}");
    let _environment = ProxyEnvironmentGuard::set(&proxy_url);

    let body = PinnedApiClient::new(policy)
        .expect("client construction should succeed")
        .get_text("/health")
        .await
        .expect("pinned API client must connect directly despite proxy environment variables");
    assert_eq!(body, OK_BODY);
}

#[test]
fn api_tls_policy_requires_https_and_the_exact_production_host() {
    let signing_key = random_signing_key();
    let no_roots = RootCertStore::empty();
    let pins = vec![STANDARD.encode([7_u8; 32])];

    let http = ApiTlsPolicy::injected(
        format!("http://{API_HOST}"),
        no_roots.clone(),
        pins.clone(),
        signing_key.verifying_key(),
        None,
        None,
    )
    .expect_err("HTTP endpoint must be rejected");
    assert_eq!(http, IntegrityFailure::InvalidApiEndpoint);

    let wrong_host = ApiTlsPolicy::injected(
        "https://api.nwflash.cc.cd.evil.invalid",
        no_roots,
        pins,
        signing_key.verifying_key(),
        None,
        None,
    )
    .expect_err("lookalike host must be rejected");
    assert_eq!(wrong_host, IntegrityFailure::InvalidApiEndpoint);
}

#[tokio::test]
#[serial]
async fn signed_pin_rotation_refreshes_over_the_pinned_channel_and_only_caches_the_envelope() {
    let signing_key = random_signing_key();
    let next_chain = test_chain(API_HOST, false);
    let next_leaf_pin = next_chain.leaf_pin.clone();
    let next_intermediate_pin = next_chain.intermediate_pin.clone();
    let next_root = next_chain.root_der.clone();
    let next_server = TestTlsServer::start(next_chain, OK_BODY.to_string()).await;
    let now = unix_now();
    let envelope = signed_pinset(&signing_key, 2, now, &next_leaf_pin, &next_intermediate_pin);
    let bootstrap_chain = test_chain(API_HOST, false);
    let bootstrap_root = bootstrap_chain.root_der.clone();
    let bootstrap_pin = bootstrap_chain.leaf_pin.clone();
    let bootstrap_server = TestTlsServer::start(
        bootstrap_chain,
        serde_json::to_string(&envelope).expect("envelope should serialize"),
    )
    .await;
    let cache = TempDir::new().expect("cache directory should be created");
    let cache_path = cache.path().join("pinset.json");
    let root_store = roots([bootstrap_root, next_root]);
    let bootstrap_policy = policy_for(
        &bootstrap_server,
        root_store.clone(),
        vec![bootstrap_pin.clone()],
        signing_key.verifying_key(),
        Some(&cache_path),
    );
    let bootstrap_client =
        PinnedApiClient::new(bootstrap_policy).expect("bootstrap client should build");

    let installed = bootstrap_client
        .refresh_pinset_at(now)
        .await
        .expect("pinset refresh should use the bootstrapped pinned channel");
    assert_eq!(installed.version, 2);
    let cached: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&cache_path).expect("signed envelope should be cached"),
    )
    .expect("cache should contain JSON");
    assert_eq!(
        cached
            .as_object()
            .expect("cache should be an object")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["pinset_payload".to_string(), "pinset_signature".to_string()]
            .into_iter()
            .collect()
    );

    let rotated_policy = policy_for(
        &next_server,
        root_store,
        vec![bootstrap_pin],
        signing_key.verifying_key(),
        Some(&cache_path),
    );
    let rotated_client =
        PinnedApiClient::new(rotated_policy).expect("signed cached pinset should reload");
    assert_eq!(
        rotated_client
            .get_text("/health")
            .await
            .expect("rotated leaf pin loaded from the signed cache should connect"),
        OK_BODY
    );
}

#[tokio::test]
#[serial]
async fn tampered_signed_pinset_cache_is_rejected_when_loaded() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let now = unix_now();
    let mut envelope = signed_pinset(
        &signing_key,
        1,
        now,
        &server.leaf_pin,
        &server.intermediate_pin,
    );
    envelope.pinset_payload.push('A');
    let cache = TempDir::new().expect("cache directory should be created");
    let cache_path = cache.path().join("pinset.json");
    std::fs::write(
        &cache_path,
        serde_json::to_vec(&envelope).expect("tampered envelope should serialize"),
    )
    .expect("tampered cache should be written");
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        Some(&cache_path),
    );

    let error = PinnedApiClient::new(policy).expect_err("tampered cache must fail closed");
    assert_eq!(
        error,
        CloudflareError::Integrity(IntegrityFailure::PinsetSignature)
    );
    assert!(!format!("{error}").contains(&envelope.pinset_payload));
}

#[tokio::test]
#[serial]
async fn signed_pinset_version_rollback_is_rejected() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let now = unix_now();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );
    let client = PinnedApiClient::new(policy).expect("client should build");
    client
        .install_pinset_at(
            signed_pinset(
                &signing_key,
                3,
                now,
                &server.leaf_pin,
                &server.intermediate_pin,
            ),
            now,
        )
        .expect("newer pinset should install");

    let error = client
        .install_pinset_at(
            signed_pinset(
                &signing_key,
                2,
                now,
                &server.leaf_pin,
                &server.intermediate_pin,
            ),
            now,
        )
        .expect_err("older signed pinset must not roll back the active version");
    assert_eq!(error, IntegrityFailure::PinsetRollback);
}

#[tokio::test]
#[serial]
async fn concurrent_valid_pinset_updates_do_not_break_requests_using_shared_state() {
    let now = 1_800_000_000;
    let clock = ManualClock::new(now);
    let server = TestTlsServer::start_with_response(
        test_chain(API_HOST, false),
        TestResponse::keep_alive_json(OK_BODY.to_string()),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for_clock(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
        1,
        &clock,
    );
    let client = Arc::new(PinnedApiClient::new(policy).expect("shared client should build"));
    let barrier = Arc::new(Barrier::new(3));

    let updater = {
        let client = client.clone();
        let barrier = barrier.clone();
        let signing_key = signing_key.clone();
        let leaf_pin = server.leaf_pin.clone();
        let intermediate_pin = server.intermediate_pin.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            for version in 2..=25 {
                client
                    .install_pinset_at(
                        signed_pinset_for_host(
                            &signing_key,
                            version,
                            API_HOST,
                            now - 1,
                            now + 3_600,
                            &leaf_pin,
                            &intermediate_pin,
                        ),
                        now,
                    )
                    .expect("monotonic concurrent update should install");
                tokio::task::yield_now().await;
            }
        })
    };
    let requester = {
        let client = client.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..24 {
                assert_eq!(
                    client
                        .get_text("/health")
                        .await
                        .expect("request should observe a complete valid pin state"),
                    OK_BODY
                );
                tokio::task::yield_now().await;
            }
        })
    };

    barrier.wait().await;
    updater.await.expect("updater task should join");
    requester.await.expect("requester task should join");
}

#[tokio::test]
#[serial]
async fn startup_rejects_a_valid_signed_cache_below_the_embedded_version_floor() {
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let now = unix_now();
    let envelope = signed_pinset(
        &signing_key,
        1,
        now,
        &server.leaf_pin,
        &server.intermediate_pin,
    );
    let cache = TempDir::new().expect("cache directory should be created");
    let cache_path = cache.path().join("pinset.json");
    std::fs::write(
        &cache_path,
        serde_json::to_vec(&envelope).expect("signed envelope should serialize"),
    )
    .expect("signed cache should be written");
    let policy = ApiTlsPolicy::injected_with_floor(
        format!("https://{API_HOST}:{}", server.addr.port()),
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        Some(cache_path),
        Some(server.addr),
        2,
    )
    .expect("test policy should build");

    assert_eq!(
        PinnedApiClient::new(policy).expect_err("cache below embedded floor must fail closed"),
        CloudflareError::Integrity(IntegrityFailure::PinsetRollback)
    );
}

#[tokio::test]
#[serial]
async fn startup_accepts_a_current_signed_cache_at_the_embedded_version_floor() {
    let now = 1_800_000_000;
    let clock = ManualClock::new(now);
    let server = TestTlsServer::start(test_chain(API_HOST, false), OK_BODY.to_string()).await;
    let signing_key = random_signing_key();
    let envelope = signed_pinset_for_host(
        &signing_key,
        2,
        API_HOST,
        now - 60,
        now + 3_600,
        &server.leaf_pin,
        &server.intermediate_pin,
    );
    let cache = TempDir::new().expect("cache directory should be created");
    let cache_path = cache.path().join("pinset.json");
    std::fs::write(
        &cache_path,
        serde_json::to_vec(&envelope).expect("signed envelope should serialize"),
    )
    .expect("signed cache should be written");
    let policy = policy_for_clock(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        Some(&cache_path),
        2,
        &clock,
    );

    assert_eq!(
        PinnedApiClient::new(policy)
            .expect("valid cache at the embedded floor should load")
            .get_text("/health")
            .await
            .expect("loaded cache should authorize the API request"),
        OK_BODY
    );
}

#[tokio::test]
#[serial]
async fn cross_host_https_redirect_is_an_endpoint_integrity_failure() {
    let server = TestTlsServer::start_with_response(
        test_chain(API_HOST, false),
        TestResponse::redirect("https://redirect.invalid/steal"),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    assert_eq!(
        PinnedApiClient::new(policy)
            .expect("pinned client should build")
            .get_text("/redirect")
            .await
            .expect_err("cross-host redirect must be rejected without following it"),
        CloudflareError::Integrity(IntegrityFailure::InvalidApiEndpoint)
    );
}

#[tokio::test]
#[serial]
async fn https_to_http_redirect_is_an_endpoint_integrity_failure() {
    let server = TestTlsServer::start_with_response(
        test_chain(API_HOST, false),
        TestResponse::redirect("http://127.0.0.1:9/downgrade"),
    )
    .await;
    let signing_key = random_signing_key();
    let policy = policy_for(
        &server,
        roots([server.root_der.clone()]),
        vec![server.leaf_pin.clone()],
        signing_key.verifying_key(),
        None,
    );

    assert_eq!(
        PinnedApiClient::new(policy)
            .expect("pinned client should build")
            .get_text("/redirect")
            .await
            .expect_err("HTTPS-to-HTTP redirect must be rejected without following it"),
        CloudflareError::Integrity(IntegrityFailure::InvalidApiEndpoint)
    );
}

#[test]
fn signed_pinset_rejects_wrong_host_future_and_expired_windows() {
    let signing_key = random_signing_key();
    let now = unix_now();
    let pin = STANDARD.encode([9_u8; 32]);
    let policy = ApiTlsPolicy::injected(
        format!("https://{API_HOST}"),
        RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        vec![pin.clone()],
        signing_key.verifying_key(),
        None,
        None,
    )
    .expect("policy should build");
    let client = PinnedApiClient::new(policy).expect("client should build without connecting");

    let wrong_host = signed_pinset_for_host(
        &signing_key,
        1,
        "api.nwflash.cc.cd.evil.invalid",
        now - 1,
        now + 60,
        &pin,
        &pin,
    );
    assert_eq!(
        client
            .install_pinset_at(wrong_host, now)
            .expect_err("pinset host must match exactly"),
        IntegrityFailure::PinsetHost
    );

    let future = signed_pinset_for_host(&signing_key, 1, API_HOST, now + 1, now + 60, &pin, &pin);
    assert_eq!(
        client
            .install_pinset_at(future, now)
            .expect_err("future pinset must fail"),
        IntegrityFailure::PinsetTime
    );

    let expired = signed_pinset_for_host(&signing_key, 1, API_HOST, now - 60, now - 1, &pin, &pin);
    assert_eq!(
        client
            .install_pinset_at(expired, now)
            .expect_err("expired pinset must fail"),
        IntegrityFailure::PinsetTime
    );
}

struct ProxyEnvironmentGuard {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ProxyEnvironmentGuard {
    fn set(proxy_url: &str) -> Self {
        let names = [
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ];
        let previous = names
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect::<Vec<_>>();
        for name in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
            std::env::set_var(name, proxy_url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
        Self { previous }
    }
}

impl Drop for ProxyEnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..) {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}
