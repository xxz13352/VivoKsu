use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_protection::{
    accept_signed_login_lease, admit_local_operation, dispatch_protection_decision,
    encoded_selector, DecisionInput, LeaseBinding, LeaseClaims, LeaseKind, OperationDecision,
    ProtectionDecision, ProtectionFailure, ProtectionSelector, SignedEnvelope, TokenDigest,
};
use rand_core::OsRng;

fn accepted_login(expires_at: i64) -> nwflash_protection::SessionLease {
    let mut rng = OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let claims = LeaseClaims {
        version: 1,
        kind: LeaseKind::Login,
        username: "alice".into(),
        token_sha256: TokenDigest::from_bytes([9_u8; 32]),
        client_version: "1.0.1".into(),
        build_id: "build-123".into(),
        process_nonce: "nonce-abc".into(),
        session_id: "session-xyz".into(),
        sequence: 1,
        issued_at: 10,
        expires_at,
    };
    let lease_payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let lease_signature =
        URL_SAFE_NO_PAD.encode(signing_key.sign(lease_payload.as_bytes()).to_bytes());
    let envelope = SignedEnvelope {
        lease_payload,
        lease_signature,
    };
    accept_signed_login_lease(
        &envelope,
        &signing_key.verifying_key(),
        &LeaseBinding::new(
            "alice",
            TokenDigest::from_bytes([9_u8; 32]),
            "1.0.1",
            "build-123",
            "nonce-abc",
            "session-xyz",
        ),
        20,
    )
    .unwrap()
}

#[test]
fn dispatcher_allows_each_encoded_selector_only_with_its_validated_input() {
    let cases = [
        (
            ProtectionSelector::Login,
            DecisionInput::Login {
                signature_valid: true,
                claims_bound: true,
            },
        ),
        (
            ProtectionSelector::Heartbeat,
            DecisionInput::Heartbeat {
                signature_valid: true,
                claims_bound: true,
                sequence_advanced: true,
            },
        ),
        (
            ProtectionSelector::LocalOperation,
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: true,
                build_id_matches: true,
                process_nonce_matches: true,
                sequence_current: true,
            },
        ),
    ];

    for (selector, input) in cases {
        assert_eq!(
            dispatch_protection_decision(encoded_selector(selector), input),
            ProtectionDecision::Allow
        );
    }
}

#[test]
fn dispatcher_denies_each_known_selector_when_its_required_flag_is_false() {
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::Login),
            DecisionInput::Login {
                signature_valid: false,
                claims_bound: true,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::InvalidLease)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::Heartbeat),
            DecisionInput::Heartbeat {
                signature_valid: true,
                claims_bound: true,
                sequence_advanced: false,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::SequenceRollback)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: false,
                build_id_matches: true,
                process_nonce_matches: true,
                sequence_current: true,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::LeaseExpired)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: true,
                build_id_matches: false,
                process_nonce_matches: true,
                sequence_current: true,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::BuildIdentityMismatch)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: true,
                build_id_matches: true,
                process_nonce_matches: false,
                sequence_current: true,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::ProcessNonceMismatch)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: true,
                build_id_matches: true,
                process_nonce_matches: true,
                sequence_current: false,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::SequenceMismatch)
    );
}

#[test]
fn dispatcher_fails_closed_for_an_illegal_selector_or_mismatched_input() {
    assert_eq!(
        dispatch_protection_decision(
            0,
            DecisionInput::Login {
                signature_valid: true,
                claims_bound: true,
            }
        ),
        ProtectionDecision::Deny(ProtectionFailure::IllegalSelector)
    );
    assert_eq!(
        dispatch_protection_decision(
            encoded_selector(ProtectionSelector::Login),
            DecisionInput::LocalOperation {
                session_active: true,
                lease_current: true,
                build_id_matches: true,
                process_nonce_matches: true,
                sequence_current: true,
            },
        ),
        ProtectionDecision::Deny(ProtectionFailure::InvalidInput)
    );
}

#[test]
fn local_operation_admission_rechecks_expiry_and_signed_process_binding() {
    let expired = accepted_login(30);
    assert_eq!(
        admit_local_operation(&expired, "build-123", "nonce-abc", 31),
        OperationDecision::DenyExpired
    );

    let current = accepted_login(60);
    assert_eq!(current.build_id(), "build-123");
    assert_eq!(current.process_nonce(), "nonce-abc");
    assert_eq!(
        admit_local_operation(&current, "other-build", "nonce-abc", 20),
        OperationDecision::DenyBuildIdMismatch
    );
    assert_eq!(
        admit_local_operation(&current, "build-123", "other-nonce", 20),
        OperationDecision::DenyProcessNonceMismatch
    );
    assert_eq!(
        admit_local_operation(&current, "build-123", "nonce-abc", 20),
        OperationDecision::Allow
    );
}
