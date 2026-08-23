use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use nwflash_protection::{
    accept_login_lease, classify_heartbeat_lease, verify_signed_lease, HeartbeatDecision,
    LeaseBinding, LeaseClaims, LeaseKind, LeaseRejection, SignedEnvelope, TokenDigest,
    MAX_CLOCK_SKEW_SECONDS,
};

const NOW: i64 = 1_725_000_000;
type ClaimMutation = Box<dyn Fn(&mut LeaseClaims)>;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn verification_key() -> VerifyingKey {
    signing_key().verifying_key()
}

fn binding() -> LeaseBinding {
    LeaseBinding::new(
        "alice",
        TokenDigest::from_bytes([9_u8; 32]),
        "1.0.1",
        "build-123",
        "nonce-abc",
        "session-xyz",
    )
}

fn claims(kind: LeaseKind) -> LeaseClaims {
    LeaseClaims {
        version: 1,
        kind,
        username: "alice".into(),
        token_sha256: URL_SAFE_NO_PAD.encode([9_u8; 32]),
        client_version: "1.0.1".into(),
        build_id: "build-123".into(),
        process_nonce: "nonce-abc".into(),
        session_id: "session-xyz".into(),
        sequence: 11,
        issued_at: NOW - 5,
        expires_at: NOW + 60,
    }
}

fn signed_envelope(claims: &LeaseClaims) -> SignedEnvelope {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let signature = signing_key().sign(payload.as_bytes());
    SignedEnvelope {
        lease_payload: payload,
        lease_signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    }
}

fn verified_login() -> nwflash_protection::VerifiedLease {
    verify_signed_lease(
        &signed_envelope(&claims(LeaseKind::Login)),
        &verification_key(),
    )
    .unwrap()
}

#[test]
fn verifies_a_signature_over_the_original_base64url_payload_ascii_bytes() {
    let verified = verify_signed_lease(
        &signed_envelope(&claims(LeaseKind::Login)),
        &verification_key(),
    )
    .unwrap();

    assert_eq!(verified.claims().username, "alice");
    assert_eq!(verified.claims().sequence, 11);
}

#[test]
fn rejects_a_wrong_signature_before_parsing_claims() {
    let mut envelope = signed_envelope(&claims(LeaseKind::Login));
    envelope.lease_signature = URL_SAFE_NO_PAD.encode([0_u8; 64]);

    assert!(matches!(
        verify_signed_lease(&envelope, &verification_key()),
        Err(nwflash_protection::LeaseVerificationError::InvalidSignature)
    ));
}

#[test]
fn rejects_a_modified_payload_with_the_original_signature() {
    let mut envelope = signed_envelope(&claims(LeaseKind::Login));
    envelope.lease_payload.push('A');

    assert!(matches!(
        verify_signed_lease(&envelope, &verification_key()),
        Err(nwflash_protection::LeaseVerificationError::InvalidSignature)
    ));
}

#[test]
fn rejects_malformed_base64url_and_signed_non_json_payloads() {
    let malformed = SignedEnvelope {
        lease_payload: "%%%".into(),
        lease_signature: URL_SAFE_NO_PAD.encode(signing_key().sign(b"%%%").to_bytes()),
    };
    assert!(matches!(
        verify_signed_lease(&malformed, &verification_key()),
        Err(nwflash_protection::LeaseVerificationError::MalformedEnvelope)
    ));

    let payload = URL_SAFE_NO_PAD.encode(b"not-json");
    let signature = signing_key().sign(payload.as_bytes());
    let non_json = SignedEnvelope {
        lease_payload: payload,
        lease_signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    };
    assert!(matches!(
        verify_signed_lease(&non_json, &verification_key()),
        Err(nwflash_protection::LeaseVerificationError::MalformedClaims)
    ));
}

#[test]
fn rejects_expired_and_excessively_future_login_leases() {
    let mut expired = claims(LeaseKind::Login);
    expired.expires_at = NOW - 1;
    let expired = verify_signed_lease(&signed_envelope(&expired), &verification_key()).unwrap();
    assert_eq!(
        accept_login_lease(&expired, &binding(), NOW),
        Err(LeaseRejection::Expired)
    );

    let mut future = claims(LeaseKind::Login);
    future.issued_at = NOW + MAX_CLOCK_SKEW_SECONDS + 1;
    future.expires_at = future.issued_at + 60;
    let future = verify_signed_lease(&signed_envelope(&future), &verification_key()).unwrap();
    assert_eq!(
        accept_login_lease(&future, &binding(), NOW),
        Err(LeaseRejection::IssuedInFuture)
    );
}

#[test]
fn rejects_login_claims_that_are_not_bound_to_the_normalized_context() {
    let cases: Vec<(&str, ClaimMutation)> = vec![
        ("version", Box::new(|claims| claims.version = 2)),
        (
            "username",
            Box::new(|claims| claims.username = "mallory".into()),
        ),
        (
            "token",
            Box::new(|claims| claims.token_sha256 = URL_SAFE_NO_PAD.encode([8_u8; 32])),
        ),
        (
            "client",
            Box::new(|claims| claims.client_version = "1.0.2".into()),
        ),
        ("build", Box::new(|claims| claims.build_id = "other".into())),
        (
            "nonce",
            Box::new(|claims| claims.process_nonce = "other".into()),
        ),
        (
            "session",
            Box::new(|claims| claims.session_id = "other".into()),
        ),
        (
            "kind",
            Box::new(|claims| claims.kind = LeaseKind::Heartbeat),
        ),
    ];

    for (name, mutate) in cases {
        let mut candidate = claims(LeaseKind::Login);
        mutate(&mut candidate);
        let verified =
            verify_signed_lease(&signed_envelope(&candidate), &verification_key()).unwrap();

        assert!(
            accept_login_lease(&verified, &binding(), NOW).is_err(),
            "{name} mismatch must fail closed"
        );
    }
}

#[test]
fn classifies_heartbeat_sequence_rollback_as_exit_pending() {
    let mut candidate = claims(LeaseKind::Heartbeat);
    candidate.sequence = 11;
    let verified = verify_signed_lease(&signed_envelope(&candidate), &verification_key()).unwrap();

    assert_eq!(
        classify_heartbeat_lease(&verified, &binding(), 11, NOW),
        HeartbeatDecision::ExitPending(LeaseRejection::SequenceRollback)
    );
}

#[test]
fn accepts_a_bound_heartbeat_with_a_strictly_increasing_sequence() {
    let verified = verify_signed_lease(
        &signed_envelope(&claims(LeaseKind::Heartbeat)),
        &verification_key(),
    )
    .unwrap();

    assert!(matches!(
        classify_heartbeat_lease(&verified, &binding(), 10, NOW),
        HeartbeatDecision::Continue(_)
    ));
}

#[test]
fn token_digest_debug_is_redacted_and_replacement_clears_the_previous_value() {
    let mut digest = TokenDigest::from_bytes([9_u8; 32]);
    assert!(!format!("{digest:?}").contains('9'));

    digest.replace([4_u8; 32]);
    assert_eq!(format!("{digest:?}"), "TokenDigest([REDACTED])");
}

#[test]
fn derives_the_sha256_token_digest_before_entering_the_lease_decision_boundary() {
    let expected_sha256 = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];
    let mut candidate = claims(LeaseKind::Login);
    candidate.token_sha256 = URL_SAFE_NO_PAD.encode(expected_sha256);
    let verified = verify_signed_lease(&signed_envelope(&candidate), &verification_key()).unwrap();
    let binding = LeaseBinding::new(
        "alice",
        TokenDigest::sha256(b"abc"),
        "1.0.1",
        "build-123",
        "nonce-abc",
        "session-xyz",
    );

    assert!(accept_login_lease(&verified, &binding, NOW).is_ok());
    assert_eq!(
        format!("{:?}", TokenDigest::sha256(b"abc")),
        "TokenDigest([REDACTED])"
    );
}

#[test]
fn claim_debug_output_redacts_the_token_digest() {
    let candidate = claims(LeaseKind::Login);
    let token_digest = candidate.token_sha256.clone();

    assert!(!format!("{candidate:?}").contains(&token_digest));
}

#[test]
fn signed_envelope_debug_output_redacts_the_wire_values() {
    let envelope = signed_envelope(&claims(LeaseKind::Login));

    let debug = format!("{envelope:?}");
    assert!(!debug.contains(&envelope.lease_payload));
    assert!(!debug.contains(&envelope.lease_signature));
}

#[test]
fn verified_login_fixture_is_accepted() {
    assert!(accept_login_lease(&verified_login(), &binding(), NOW).is_ok());
}
