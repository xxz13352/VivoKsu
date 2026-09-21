//! 本地工件（配置文件 / 固件包）的签名校验。
//!
//! ## 为什么本地文件也需要验签
//!
//! 刷机工具的输入来自本机磁盘：用户选的固件包、工具自己的 `settings.json`。
//! 这两处都是**可被替换的本地输入**：
//!
//! - 固件包被替换成恶意镜像 -> 直接写进设备 -> **变砖或植入**。
//! - `settings.json` 被改（例如把 `scrcpy_path` 指向任意可执行文件）-> 工具
//!   会拉起该进程 -> 本地提权。
//!
//! ## 与既有公钥体系的关系
//!
//! 复用**同一个** Ed25519 公钥（编译期由 `NWFLASH_SESSION_VERIFY_KEY_B64`
//! 嵌入，见 `nwflash-infrastructure::pinned_tls`），不引入第二套信任根。
//! 签名格式与租约信封一致：**base64url 无填充编码的 64 字节签名**。
//!
//! ## fail-closed 原则
//!
//! 校验失败**绝不**"退回未校验内容"：
//! - `verify_artifact_bytes` 返回 `Err`，调用方必须拒绝使用该数据。
//! - `select_verified_or_default` 用于配置类场景：验签失败时回退到**编译期
//!   默认值**（安全的那一份），而不是回退到磁盘上那份未验证的内容。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use sha2::{Digest as _, Sha256};

/// 本地工件验签失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactVerificationError {
    /// 附带的签名不是合法的 base64url 或无填充长度不对。
    MalformedSignature,
    /// 签名长度不是 64 字节。
    InvalidSignatureLength,
    /// Ed25519 验签不通过（内容被改、或签名不属于本工具的公钥）。
    SignatureMismatch,
}

/// 一份工件与其 detached 签名的校验关系。
///
/// `signature_b64` 是 `.sig` 文件的**全部内容**（base64url 无填充，可带尾部
/// 空白）。签名覆盖的是**工件字节的 SHA-256**，而不是工件原文——这样大文件
/// 不需要把全文喂进验签函数，也避免 Ed25519 的 64 字节签名超限问题。
pub fn verify_artifact_bytes(
    artifact: &[u8],
    signature_b64: &str,
    verifying_key: &VerifyingKey,
) -> Result<(), ArtifactVerificationError> {
    let digest = Sha256::digest(artifact);
    verify_sha256_digest(digest.as_slice(), signature_b64, verifying_key)
}

/// 已经算好摘要时的验签入口。
///
/// 用于流式读取的大固件包：边读边喂 `Sha256`，最后只把 32 字节摘要交给
/// 本函数，避免把整个包读进内存。
pub fn verify_sha256_digest(
    digest: &[u8],
    signature_b64: &str,
    verifying_key: &VerifyingKey,
) -> Result<(), ArtifactVerificationError> {
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64.trim())
        .map_err(|_| ArtifactVerificationError::MalformedSignature)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ArtifactVerificationError::InvalidSignatureLength)?;
    verifying_key
        .verify(digest, &signature)
        .map_err(|_| ArtifactVerificationError::SignatureMismatch)
}

/// 配置类工件的选择结果：告知调用方"用的是哪一份"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedConfigSource {
    /// 磁盘上的配置通过了验签，可以使用。
    VerifiedFile,
    /// 验签失败（或文件缺失），调用方必须使用**编译期默认值**。
    ///
    /// 这个分支必须让调用方**可见**（而不是静默替换），因为它意味着本地
    /// 配置被改过——是需要留痕的安全事件。
    FellBackToDefault,
}

/// 配置类工件的 fail-closed 选择器。
///
/// - 验签通过 -> 返回磁盘内容 + [`VerifiedConfigSource::VerifiedFile`]
/// - 验签失败 / 签名缺失 -> 返回 `None` + [`VerifiedConfigSource::FellBackToDefault`]
///
/// 返回 `None` 是刻意的：它强迫调用方**显式**写出"用默认值"这一步，
/// 而不是拿到一份来源不明的数据继续用。
pub fn select_verified_config<'a>(
    config_bytes: Option<&'a [u8]>,
    signature_b64: Option<&str>,
    verifying_key: &VerifyingKey,
) -> (Option<&'a [u8]>, VerifiedConfigSource) {
    match (config_bytes, signature_b64) {
        (Some(bytes), Some(signature)) => match verify_artifact_bytes(bytes, signature, verifying_key)
        {
            Ok(()) => (Some(bytes), VerifiedConfigSource::VerifiedFile),
            Err(_) => (None, VerifiedConfigSource::FellBackToDefault),
        },
        // 配置或签名缺失同样按 fail-closed 处理。
        _ => (None, VerifiedConfigSource::FellBackToDefault),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn keypair() -> (SigningKey, VerifyingKey) {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let verifying = signing.verifying_key();
        (signing, verifying)
    }

    fn sign_digest(signing: &SigningKey, artifact: &[u8]) -> String {
        let digest = Sha256::digest(artifact);
        URL_SAFE_NO_PAD.encode(signing.sign(digest.as_slice()).to_bytes())
    }

    #[test]
    fn correctly_signed_artifact_is_accepted() {
        let (signing, verifying) = keypair();
        let artifact = b"firmware-package-bytes";
        let signature = sign_digest(&signing, artifact);
        assert_eq!(
            verify_artifact_bytes(artifact, &signature, &verifying),
            Ok(())
        );
    }

    #[test]
    fn tampered_artifact_is_rejected() {
        let (signing, verifying) = keypair();
        let signature = sign_digest(&signing, b"original-firmware");
        // 内容被替换，签名不变。
        assert_eq!(
            verify_artifact_bytes(b"totally-different-firmware", &signature, &verifying),
            Err(ArtifactVerificationError::SignatureMismatch)
        );
    }

    #[test]
    fn signature_from_another_key_is_rejected() {
        let (signing, _) = keypair();
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        let artifact = b"firmware";
        let signature = sign_digest(&signing, artifact);
        assert_eq!(
            verify_artifact_bytes(artifact, &signature, &other),
            Err(ArtifactVerificationError::SignatureMismatch)
        );
    }

    #[test]
    fn malformed_signature_is_rejected() {
        let (_, verifying) = keypair();
        assert_eq!(
            verify_artifact_bytes(b"x", "not-base64!!", &verifying),
            Err(ArtifactVerificationError::MalformedSignature)
        );
    }

    #[test]
    fn wrong_length_signature_is_rejected() {
        let (_, verifying) = keypair();
        let short = URL_SAFE_NO_PAD.encode([1u8; 32]);
        assert_eq!(
            verify_artifact_bytes(b"x", &short, &verifying),
            Err(ArtifactVerificationError::InvalidSignatureLength)
        );
    }

    #[test]
    fn trailing_whitespace_in_signature_file_is_tolerated() {
        let (signing, verifying) = keypair();
        let artifact = b"config-json";
        let signature = format!("{}\n", sign_digest(&signing, artifact));
        assert_eq!(
            verify_artifact_bytes(artifact, &signature, &verifying),
            Ok(())
        );
    }

    #[test]
    fn config_with_valid_signature_is_used() {
        let (signing, verifying) = keypair();
        let config = b"{\"ScrcpyPath\":\"C:/tools/scrcpy.exe\"}";
        let signature = sign_digest(&signing, config);
        let (selected, source) =
            select_verified_config(Some(config), Some(&signature), &verifying);
        assert_eq!(selected, Some(config.as_slice()));
        assert_eq!(source, VerifiedConfigSource::VerifiedFile);
    }

    /// fail-closed 的核心断言：签名不对时**必须**回退到默认值，
    /// 绝不能返回磁盘上那份未验证的内容。
    #[test]
    fn config_with_invalid_signature_falls_back_to_default() {
        let (signing, verifying) = keypair();
        let config = b"{\"ScrcpyPath\":\"C:/evil.exe\"}";
        let stale = sign_digest(&signing, b"some-other-content");
        let (selected, source) = select_verified_config(Some(config), Some(&stale), &verifying);
        assert_eq!(selected, None, "验签失败绝不能返回磁盘内容");
        assert_eq!(source, VerifiedConfigSource::FellBackToDefault);
    }

    #[test]
    fn missing_signature_falls_back_to_default() {
        let (_, verifying) = keypair();
        let (selected, source) = select_verified_config(Some(b"{}"), None, &verifying);
        assert_eq!(selected, None);
        assert_eq!(source, VerifiedConfigSource::FellBackToDefault);
    }

    #[test]
    fn missing_config_falls_back_to_default() {
        let (signing, verifying) = keypair();
        let signature = sign_digest(&signing, b"{}");
        let (selected, source) = select_verified_config(None, Some(&signature), &verifying);
        assert_eq!(selected, None);
        assert_eq!(source, VerifiedConfigSource::FellBackToDefault);
    }

    /// 流式摘要入口与整体入口必须给出**完全一致**的结论。
    #[test]
    fn streaming_digest_entry_matches_whole_buffer_entry() {
        let (signing, verifying) = keypair();
        let artifact = b"large-firmware-package";
        let signature = sign_digest(&signing, artifact);
        let digest = Sha256::digest(artifact);

        assert_eq!(
            verify_sha256_digest(digest.as_slice(), &signature, &verifying),
            verify_artifact_bytes(artifact, &signature, &verifying)
        );
        assert_eq!(
            verify_sha256_digest(digest.as_slice(), &signature, &verifying),
            Ok(())
        );
    }
}