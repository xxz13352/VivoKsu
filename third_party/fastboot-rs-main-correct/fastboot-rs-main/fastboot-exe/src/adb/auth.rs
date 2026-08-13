use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use num_bigint::{BigInt, BigUint};
use num_traits::{One, Signed, ToPrimitive, Zero};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const RSA_KEY_BITS: usize = 2048;
const ANDROID_PUBKEY_MODULUS_SIZE: usize = 256;
const ANDROID_PUBKEY_MODULUS_SIZE_WORDS: u32 = 64;

const ADB_KEY_PERSIST_FAIL: &str = "密钥持久化失败";

const ADB_MEM_KEY_DISK_FAIL: &str = "密钥写入失败";

const ADB_KEY_CORRUPT: &str = "密钥损坏";

const ADB_KEY_FORMAT: &str = "密钥格式无效";

const ADB_KEY_LEN_MISMATCH: &str = "密钥长度不匹配";

const ADB_PHYSICAL_KEY_MISSING: &str = "私钥未找到";

const ADB_PHYSICAL_PUB_MISSING: &str = "公钥未找到";

const ADB_KEYPAIR_INCOMPLETE: &str = "密钥对不完整";

const ADB_PUB_PAYLOAD_INVALID: &str = "公钥载荷无效";

const ADB_AUTH_TOKEN_LEN_BAD: &str = "认证令牌长度无效";

const ADB_SIGNATURE_LEN_BAD: &str = "签名长度无效";

const ADB_RSA_MODULUS_NOT_2048: &str = "RSA 模数不是 2048 位";

const ADB_SIGN_CRYPTO_FAIL: &str = "签名加密失败";

const ADB_KEY_PEM_BODY_INVALID: &str = "PEM 正文无效";
pub struct AdbKeyManager {
    private_key: RsaPrivateKey,
    public_key: RsaPublicKey,
    key_path: PathBuf,
    host_id: String,
}

impl AdbKeyManager {
    pub fn materialize_default_keypair_if_absent() -> io::Result<()> {
        let key_path = Self::default_key_path()?;
        let pub_path = key_path.with_extension("pub");
        let has_key = key_path.is_file();
        let has_pub = pub_path.is_file();
        if has_key && has_pub {
            return Ok(());
        }
        if has_key != has_pub {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ADB_KEYPAIR_INCOMPLETE,
            ));
        }
        Self::generate_keys_to_disk(&key_path)
    }

    pub fn ensure_physical_keypair_present() -> io::Result<()> {
        let key_path = Self::default_key_path()?;
        let pub_path = key_path.with_extension("pub");
        if !key_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                ADB_PHYSICAL_KEY_MISSING,
            ));
        }
        if !pub_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                ADB_PHYSICAL_PUB_MISSING,
            ));
        }
        Ok(())
    }

    pub fn load_existing() -> io::Result<Self> {
        Self::ensure_physical_keypair_present()?;
        let key_path = Self::default_key_path()?;
        Self::load_from_file(&key_path)
    }

    fn read_cleaned_pub_b64_string(pub_path: &Path) -> io::Result<String> {
        let raw = fs::read(pub_path)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "读取 adbkey.pub 失败"))?;
        let cleaned: String = String::from_utf8_lossy(&raw)
            .chars()
            .filter(|c| *c != '\n' && *c != '\r' && !c.is_whitespace())
            .collect();
        let b64_part = if let Some(i) = cleaned.find("==") {
            cleaned[..i + 2].to_string()
        } else if let Some(i) = cleaned.rfind('=') {
            cleaned[..i + 1].to_string()
        } else {
            String::new()
        };
        if b64_part.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ADB_PUB_PAYLOAD_INVALID,
            ));
        }
        BASE64
            .decode(b64_part.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, ADB_PUB_PAYLOAD_INVALID))?;
        Ok(b64_part)
    }

    fn pem_strip_decode_der(pem_text: &str) -> io::Result<Vec<u8>> {
        let mut body = String::new();
        let mut in_blob = false;
        for line in pem_text.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.starts_with("-----BEGIN") {
                in_blob = true;
                continue;
            }
            if t.starts_with("-----END") {
                break;
            }
            if in_blob {
                body.push_str(t);
            }
        }
        if body.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ADB_KEY_PEM_BODY_INVALID,
            ));
        }
        BASE64
            .decode(body.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, ADB_KEY_PEM_BODY_INVALID))
    }

    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let pem = fs::read_to_string(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, ADB_KEY_CORRUPT))?;
        let der = Self::pem_strip_decode_der(pem.trim())?;
        let private_key = RsaPrivateKey::from_pkcs1_der(&der)
            .or_else(|_| RsaPrivateKey::from_pkcs8_der(&der))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, ADB_KEY_FORMAT))?;
        let public_key = RsaPublicKey::from(&private_key);
        let host_id = Self::get_fixed_host_id();
        Ok(Self {
            private_key,
            public_key,
            key_path: path.to_path_buf(),
            host_id,
        })
    }

    fn generate_keys_to_disk(path: &Path) -> io::Result<()> {
        Self::ensure_android_dir_msg(path, ADB_MEM_KEY_DISK_FAIL)?;
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, RSA_KEY_BITS)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, ADB_MEM_KEY_DISK_FAIL))?;
        let pem = private_key
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, ADB_MEM_KEY_DISK_FAIL))?;
        let pem_bytes = pem.as_bytes();
        Self::write_all_sync_msg(path, pem_bytes, ADB_MEM_KEY_DISK_FAIL)?;
        Self::verify_file_len(path, pem_bytes.len() as u64, ADB_KEY_LEN_MISMATCH)?;
        #[cfg(unix)]
        Self::set_private_key_permissions_msg(path, ADB_MEM_KEY_DISK_FAIL)?;
        let host_id = Self::get_fixed_host_id();
        let android_pubkey = Self::android_pubkey_bytes(&private_key, &host_id)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, ADB_MEM_KEY_DISK_FAIL))?;
        let pub_path = path.with_extension("pub");
        let pubkey_str = String::from_utf8_lossy(&android_pubkey);
        let pub_slice = pubkey_str.trim_end_matches('\0').as_bytes();
        Self::write_all_sync_msg(&pub_path, pub_slice, ADB_MEM_KEY_DISK_FAIL)?;
        Self::verify_file_len(&pub_path, pub_slice.len() as u64, ADB_KEY_LEN_MISMATCH)?;
        drop(private_key);
        if !path.is_file() || !pub_path.is_file() {
            return Err(io::Error::new(io::ErrorKind::Other, ADB_MEM_KEY_DISK_FAIL));
        }
        Ok(())
    }

    fn map_key_write_io(path: &Path, e: io::Error, fallback: &'static str) -> io::Error {
        if e.kind() == io::ErrorKind::PermissionDenied {
            return io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("权限拒绝: {}", path.display()),
            );
        }
        io::Error::new(e.kind(), fallback)
    }

    fn write_all_sync_msg(path: &Path, data: &[u8], err_msg: &'static str) -> io::Result<()> {
        let mut f = fs::File::create(path).map_err(|e| Self::map_key_write_io(path, e, err_msg))?;
        f.write_all(data)
            .map_err(|e| Self::map_key_write_io(path, e, err_msg))?;
        f.sync_all()
            .map_err(|e| Self::map_key_write_io(path, e, err_msg))
    }

    fn verify_file_len(path: &Path, expected: u64, err_msg: &'static str) -> io::Result<()> {
        let len = fs::metadata(path)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, err_msg))?
            .len();
        if len != expected {
            return Err(io::Error::new(io::ErrorKind::Other, err_msg));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn set_private_key_permissions_msg(path: &Path, err_msg: &'static str) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, err_msg))?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms).map_err(|_| io::Error::new(io::ErrorKind::Other, err_msg))
    }

    fn ensure_android_dir_msg(key_path: &Path, err_msg: &'static str) -> io::Result<()> {
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("权限拒绝: {}", parent.display()),
                    )
                } else {
                    io::Error::new(io::ErrorKind::Other, err_msg)
                }
            })?;
        }
        Ok(())
    }

    fn get_fixed_host_id() -> String {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_string());
        #[cfg(windows)]
        let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_string());
        #[cfg(not(windows))]
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| {
                fs::read_to_string("/etc/hostname")
                    .map(|s| s.trim().to_string())
                    .ok()
                    .unwrap_or_else(|| "unknown".to_string())
            })
            .unwrap_or_else(|_| "unknown".to_string());
        format!("{}@{}", username, hostname)
    }

    fn resolve_home_dir() -> io::Result<PathBuf> {
        #[cfg(windows)]
        {
            let p = std::env::var("USERPROFILE")
                .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "USERPROFILE 未设置"))?;
            let t = p.trim();
            if t.is_empty() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "USERPROFILE 为空"));
            }
            return Ok(PathBuf::from(t));
        }
        #[cfg(not(windows))]
        {
            let p = std::env::var("HOME")
                .map_err(|_| io::Error::new(io::ErrorKind::NotFound, ADB_KEY_PERSIST_FAIL))?;
            let t = p.trim();
            if t.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    ADB_KEY_PERSIST_FAIL,
                ));
            }
            Ok(PathBuf::from(t))
        }
    }

    fn default_key_path() -> io::Result<PathBuf> {
        let home = Self::resolve_home_dir()?;
        Ok(home.join(".android").join("adbkey"))
    }

    fn android_pubkey_bytes(private_key: &RsaPrivateKey, host_id: &str) -> io::Result<Vec<u8>> {
        let public_key = RsaPublicKey::from(private_key.clone());
        let n = public_key.n();
        let e = public_key.e();
        let n_bytes_le = n.to_bytes_le();
        let mut modulus = vec![0u8; ANDROID_PUBKEY_MODULUS_SIZE];
        let copy_len = std::cmp::min(n_bytes_le.len(), ANDROID_PUBKEY_MODULUS_SIZE);
        modulus[..copy_len].copy_from_slice(&n_bytes_le[..copy_len]);
        let n_biguint = BigUint::from_bytes_le(&n_bytes_le);
        let r32 = BigUint::from(1u64 << 32);
        let n0 = &n_biguint % &r32;
        let n0inv = Self::mod_inverse(&n0, &r32)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "公钥参数派生失败"))?;
        let n0inv = (&r32 - &n0inv) % &r32;
        let n0inv_u32 = n0inv.to_u32().unwrap_or(0);
        let r = BigUint::from(1u8) << (ANDROID_PUBKEY_MODULUS_SIZE * 8);
        let rr = (&r * &r) % &n_biguint;
        let rr_bytes = rr.to_bytes_le();
        let mut rr_padded = vec![0u8; ANDROID_PUBKEY_MODULUS_SIZE];
        let copy_len = std::cmp::min(rr_bytes.len(), ANDROID_PUBKEY_MODULUS_SIZE);
        rr_padded[..copy_len].copy_from_slice(&rr_bytes[..copy_len]);
        let e_u32 = e.to_u32().unwrap_or(65537);
        let mut binary = Vec::with_capacity(524);
        binary.extend_from_slice(&ANDROID_PUBKEY_MODULUS_SIZE_WORDS.to_le_bytes());
        binary.extend_from_slice(&n0inv_u32.to_le_bytes());
        binary.extend_from_slice(&modulus);
        binary.extend_from_slice(&rr_padded);
        binary.extend_from_slice(&e_u32.to_le_bytes());
        let b64 = BASE64.encode(&binary);
        Ok(format!("{} {}\0", b64, host_id).into_bytes())
    }

    pub fn pubkey_bytes_for_auth(&self) -> io::Result<Vec<u8>> {
        let pub_path = self.key_path.with_extension("pub");
        if !pub_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                ADB_PHYSICAL_PUB_MISSING,
            ));
        }
        let b64_part = Self::read_cleaned_pub_b64_string(&pub_path)?;
        let mut out = b64_part.into_bytes();
        out.push(0);
        Ok(out)
    }

    pub fn sign(&self, token: &[u8]) -> io::Result<Vec<u8>> {
        const SIG_BYTES: usize = 256;
        if self.private_key.size() != SIG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ADB_RSA_MODULUS_NOT_2048,
            ));
        }
        if token.len() != 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ADB_AUTH_TOKEN_LEN_BAD,
            ));
        }
        let mut rng = rand::thread_rng();
        let sig = self
            .private_key
            .sign_with_rng(&mut rng, Pkcs1v15Sign::new::<Sha1>(), token)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, ADB_SIGN_CRYPTO_FAIL))?;
        if sig.len() != SIG_BYTES {
            return Err(io::Error::new(io::ErrorKind::Other, ADB_SIGNATURE_LEN_BAD));
        }
        Ok(sig)
    }

    pub fn get_adb_public_key(&self) -> io::Result<Vec<u8>> {
        Self::android_pubkey_bytes(&self.private_key, &self.host_id)
    }

    fn mod_inverse(a: &BigUint, m: &BigUint) -> Option<BigUint> {
        let a = BigInt::from(a.clone());
        let m = BigInt::from(m.clone());
        let mut old_r = a;
        let mut r = m.clone();
        let mut old_s = BigInt::one();
        let mut s = BigInt::zero();
        while !r.is_zero() {
            let quotient = &old_r / &r;
            let temp_r = r.clone();
            r = &old_r - &quotient * &r;
            old_r = temp_r;
            let temp_s = s.clone();
            s = &old_s - &quotient * &s;
            old_s = temp_s;
        }
        if old_r != BigInt::one() {
            return None;
        }
        if old_s.is_negative() {
            old_s = old_s + &m;
        }
        Some(old_s.to_biguint().unwrap())
    }
}
