use std::hint::black_box;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_protection::{
    accept_signed_login_lease, admit_local_operation, build_identity_matches,
    classify_signed_heartbeat_lease, trace_credential_sentinel, verify_image_integrity,
    LeaseBinding, LeaseClaims, LeaseKind, SignedEnvelope, TokenDigest, TraceCredentialScanner,
    VmpIntegrityProbe,
};

const NOW: i64 = 1_725_000_000;

fn main() {
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let verifying_key = signing_key.verifying_key();
    let login = signed_lease(&signing_key, LeaseKind::Login, 1);
    let session = accept_signed_login_lease(&login, &verifying_key, &binding(), NOW)
        .expect("probe login must verify and bind");
    let heartbeat = signed_lease(&signing_key, LeaseKind::Heartbeat, 2);
    let heartbeat = classify_signed_heartbeat_lease(&heartbeat, &verifying_key, &binding(), 1, NOW)
        .expect("probe heartbeat must verify");
    let admission = admit_local_operation(&session, "layout-build", "layout-nonce", NOW);
    let integrity = verify_image_integrity(&VmpIntegrityProbe);
    let identity = build_identity_matches("layout-build", "layout-build");
    let mut trace_scanner = TraceCredentialScanner::new();
    trace_scanner.push(b"trace layout probe");
    let redacted_trace = trace_scanner.finish().expect("static probe text must seal");
    let trace_credential = trace_credential_sentinel(&redacted_trace);

    black_box((
        session,
        heartbeat,
        admission,
        integrity,
        identity,
        trace_credential,
    ));
}

fn binding() -> LeaseBinding {
    LeaseBinding::new(
        "layout-user",
        TokenDigest::from_bytes([9_u8; 32]),
        "1.0.1",
        "layout-build",
        "layout-nonce",
        "layout-session",
    )
}

fn signed_lease(signing_key: &SigningKey, kind: LeaseKind, sequence: u64) -> SignedEnvelope {
    let claims = LeaseClaims {
        version: 1,
        kind,
        username: "layout-user".into(),
        token_sha256: TokenDigest::from_bytes([9_u8; 32]),
        client_version: "1.0.1".into(),
        build_id: "layout-build".into(),
        process_nonce: "layout-nonce".into(),
        session_id: "layout-session".into(),
        sequence,
        issued_at: NOW - 1,
        expires_at: NOW + 60,
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims serialize"));
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
    SignedEnvelope {
        lease_payload: payload,
        lease_signature: signature,
    }
}
