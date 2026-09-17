use std::hint::black_box;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_protection::{
    accept_signed_login_lease, admit_local_operation, build_identity_matches,
    classify_signed_heartbeat_lease, requires_protected_recheck, trace_credential_sentinel,
    verify_image_integrity, LeaseBinding, LeaseClaims, LeaseKind, ProtectedOperationKind,
    SignedEnvelope, TokenDigest, TraceOutputSession, VmpIntegrityProbe,
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
    // 分类叶子必须被链接进 MAP：每个 wire 索引都触达它（值本身不进 black_box
    // 决策，只要调用存在即保证符号出现且恰好一次）。
    let classification =
        requires_protected_recheck(ProtectedOperationKind::Flashing as u32);
    let mut trace_reader = std::io::Cursor::new(b"trace layout probe".as_slice());
    let trace_session = TraceOutputSession::from_reader(
        nwflash_domain::TraceId::try_new_v7().expect("probe event id"),
        nwflash_domain::TraceOutputStreamV2::Stdout,
        &mut trace_reader,
        &nwflash_protection::ExactSecretSet::empty(),
    )
    .expect("static probe text must seal");
    let trace_input = trace_session.credential_sentinel_input();
    let trace_credential = trace_credential_sentinel(&trace_input);
    // 终结器叶子不能在布局探针里执行;取函数指针即可强制把符号保留
    // 进 MAP(每个叶子符号必须恰好出现一次)。
    let terminator = nwflash_protection::terminate_protected_process as unsafe fn(i32) -> !;

    black_box((
        session,
        heartbeat,
        admission,
        integrity,
        identity,
        classification,
        trace_credential,
        terminator,
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
