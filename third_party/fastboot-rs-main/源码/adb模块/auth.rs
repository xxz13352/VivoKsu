
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use rsa::{RsaPrivateKey, RsaPublicKey};
use rsa::pkcs1::{EncodeRsaPrivateKey, DecodeRsaPrivateKey};
use rsa::traits::PublicKeyParts;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use num_bigint::{BigUint, BigInt};
use num_traits::{Zero, One, Signed, ToPrimitive};

const RSA_KEY_BITS: usize = 2048;
const ANDROID_PUBKEY_MODULUS_SIZE: usize = 256;
const ANDROID_PUBKEY_MODULUS_SIZE_WORDS: u32 = 64;

pub struct AdbKeyManager {
    private_key: RsaPrivateKey,
    public_key: RsaPublicKey,
    key_path: PathBuf,
    host_id: String,
}

impl AdbKeyManager {
    pub fn load_or_generate() -> io::Result<Self> {
        let key_path = Self::default_key_path()?;

        if key_path.exists() {
            match Self::load_from_file(&key_path) {
                Ok(manager) => Ok(manager),
                Err(e) => {
                    panic!(
                        "[ADB] 致命错误: 密钥文件存在但无法解析!\n\
                         路径: {}\n\
                         错误: {}\n\
                         请检查文件权限或删除损坏的密钥文件后重试。",
                        key_path.display(), e
                    );
                }
            }
        } else {
            Self::generate_and_save(&key_path)
        }
    }

    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let pem = fs::read_to_string(path)?;
        let private_key = RsaPrivateKey::from_pkcs1_pem(&pem)
            .or_else(|_| {
                use rsa::pkcs8::DecodePrivateKey;
                RsaPrivateKey::from_pkcs8_pem(&pem)
            })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData,
                format!("无法解析密钥文件: {}", e)))?;
        let public_key = RsaPublicKey::from(&private_key);

        let host_id = Self::get_fixed_host_id();

        Ok(Self {
            private_key,
            public_key,
            key_path: path.to_path_buf(),
            host_id,
        })
    }

    pub fn generate_and_save(path: &Path) -> io::Result<Self> {
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, RSA_KEY_BITS)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let public_key = RsaPublicKey::from(&private_key);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let pem = private_key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, pem.as_bytes())?;

        let host_id = Self::get_fixed_host_id();

        let temp = Self {
            private_key: private_key.clone(),
            public_key: public_key.clone(),
            key_path: path.to_path_buf(),
            host_id: host_id.clone(),
        };

        let pub_path = path.with_extension("pub");
        let android_pubkey = temp.get_adb_public_key()?;
        let pubkey_str = String::from_utf8_lossy(&android_pubkey);
        fs::write(&pub_path, pubkey_str.trim_end_matches('\0'))?;

        Ok(Self {
            private_key,
            public_key,
            key_path: path.to_path_buf(),
            host_id,
        })
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

    fn default_key_path() -> io::Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法找到用户目录"))?;
        Ok(home.join(".android").join("adbkey"))
    }

    pub fn sign(&self, token: &[u8]) -> io::Result<Vec<u8>> {
        use rsa::traits::PrivateKeyParts;

        let k = (self.private_key.n().bits() + 7) / 8;
        if token.len() > k - 11 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "token 太长"));
        }

        let padding_len = k - 3 - token.len();
        let mut padded = Vec::with_capacity(k);
        padded.push(0x00);
        padded.push(0x01);
        padded.extend(std::iter::repeat(0xFF).take(padding_len));
        padded.push(0x00);
        padded.extend_from_slice(token);

        let m = BigUint::from_bytes_be(&padded);
        let n = BigUint::from_bytes_be(&self.private_key.n().to_bytes_be());
        let d = BigUint::from_bytes_be(&self.private_key.d().to_bytes_be());
        let sig_int = m.modpow(&d, &n);

        let sig_bytes = sig_int.to_bytes_be();
        let mut signature = vec![0u8; k - sig_bytes.len()];
        signature.extend_from_slice(&sig_bytes);

        Ok(signature)
    }

    pub fn get_adb_public_key(&self) -> io::Result<Vec<u8>> {
        let n = self.public_key.n();
        let e = self.public_key.e();
        let n_bytes_le = n.to_bytes_le();

        let mut modulus = vec![0u8; ANDROID_PUBKEY_MODULUS_SIZE];
        let copy_len = std::cmp::min(n_bytes_le.len(), ANDROID_PUBKEY_MODULUS_SIZE);
        modulus[..copy_len].copy_from_slice(&n_bytes_le[..copy_len]);

        let n_biguint = BigUint::from_bytes_le(&n_bytes_le);
        let r32 = BigUint::from(1u64 << 32);
        let n0 = &n_biguint % &r32;

        let n0inv = Self::mod_inverse(&n0, &r32)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "无法计算 n0inv"))?;
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

        Ok(format!("{} {}\0", b64, self.host_id).into_bytes())
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

        if old_r != BigInt::one() { return None; }
        if old_s.is_negative() { old_s = old_s + &m; }
        Some(old_s.to_biguint().unwrap())
    }
}