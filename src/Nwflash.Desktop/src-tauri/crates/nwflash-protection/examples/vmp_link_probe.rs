use std::hint::black_box;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_protection::{
    accept_login_lease, admit_local_operation, build_identity_matches, classify_heartbeat_lease,
    verify_image_integrity, verify_signed_lease, LeaseBinding, LeaseClaims, LeaseKind,
    SignedEnvelope, TokenDigest, VmpIntegrityProbe,
};

const NOW: i64 = 1_725_000_000;

fn main() {
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let login = verified_lease(&signing_key, LeaseKind::Login, 1);
    let session = accept_login_lease(&login, &binding(), NOW).expect("probe login must bind");
    let heartbeat = verified_lease(&signing_key, LeaseKind::Heartbeat, 2);
    let heartbeat = classify_heartbeat_lease(&heartbeat, &binding(), 1, NOW);
    let admission = admit_local_operation(&session, NOW);
    let integrity = verify_image_integrity(&VmpIntegrityProbe);
    let identity = build_identity_matches("layout-build", "layout-build");

    black_box((session, heartbeat, admission, integrity, identity));
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

fn verified_lease(
    signing_key: &SigningKey,
    kind: LeaseKind,
    sequence: u64,
) -> nwflash_protection::VerifiedLease {
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
    verify_signed_lease(
        &SignedEnvelope {
            lease_payload: payload,
            lease_signature: signature,
        },
        &signing_key.verifying_key(),
    )
    .expect("probe lease verifies")
}
