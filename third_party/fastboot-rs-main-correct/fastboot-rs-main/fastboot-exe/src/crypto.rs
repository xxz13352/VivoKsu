use base64::Engine;
use byteorder::{LittleEndian, WriteBytesExt};
use num_bigint::BigUint;
use rand_core::OsRng;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const RSA_KEY_SIZE: usize = 2048;

fn calculate_n0inv(n0: u32) -> u32 {
    let mut r = n0;
    let mut i = 0;
    while i < 4 {
        r = r.wrapping_mul(2u32.wrapping_sub(n0.wrapping_mul(r)));
        i += 1;
    }
    (!r).wrapping_add(1)
}

pub fn generate_adb_public_key(priv_key: &RsaPrivateKey) -> String {
    let n_bytes_be = priv_key.n().to_bytes_be();
    let n_biguint = BigUint::from_bytes_be(&n_bytes_be);

    let r = BigUint::from(1u32) << 2048;
    let r_squared: BigUint = (&r * &r) % &n_biguint;

    let n_bytes_le = n_biguint.to_bytes_le();
    let rr_bytes_le = r_squared.to_bytes_le();

    let mut n_256 = [0u8; 256];
    for (i, &b) in n_bytes_le.iter().enumerate().take(256) {
        n_256[i] = b;
    }

    let mut rr_256 = [0u8; 256];
    for (i, &b) in rr_bytes_le.iter().enumerate().take(256) {
        rr_256[i] = b;
    }

    let n0 = u32::from_le_bytes(n_256[0..4].try_into().unwrap());
    let n0inv = calculate_n0inv(n0);

    let mut buffer = Vec::with_capacity(524);
    buffer.write_u32::<LittleEndian>(64).unwrap();
    buffer.write_u32::<LittleEndian>(n0inv).unwrap();
    buffer.extend_from_slice(&n_256);
    buffer.extend_from_slice(&rr_256);
    buffer.write_u32::<LittleEndian>(65537).unwrap();

    let base64_str = base64::engine::general_purpose::STANDARD.encode(&buffer);
    format!("{} user@rustyadb\0", base64_str)
}

/// 返回 `~/.android` 目录（不存在则尝试创建）。
fn android_dir() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".android");
    if !dir.exists() {
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("警告: 无法创建 .android 目录: {}", e);
        }
    }
    dir
}

/// 同时兼容官方 adb 的 PKCS#8（`-----BEGIN PRIVATE KEY-----`）与
/// 旧式 PKCS#1（`-----BEGIN RSA PRIVATE KEY-----`）私钥，二者皆可读取。
fn parse_private_key_any(pem: &str) -> Option<RsaPrivateKey> {
    RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
        .ok()
}

/// 把可疑/无法解析的密钥文件备份一份再处理，绝不静默丢弃用户原有密钥。
fn backup_file(path: &Path) {
    if !path.exists() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("adbkey");
    let backup = path.with_file_name(format!("{}.corrupt-{}.bak", name, ts));
    match fs::copy(path, &backup) {
        Ok(_) => eprintln!("提示: 已将原密钥备份为 {}", backup.display()),
        Err(e) => eprintln!("警告: 备份 {} 失败: {}", path.display(), e),
    }
}

/// 原子写：先写 `*.tmp` 并 fsync，再替换目标文件，避免半截写坏密钥。
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)
}

/// 仅在内存中生成 RSA 私钥（不落盘）。
fn generate_private_key_in_memory() -> RsaPrivateKey {
    let mut rng = OsRng;
    loop {
        match RsaPrivateKey::new(&mut rng, RSA_KEY_SIZE) {
            Ok(key) => return key,
            Err(e) => {
                eprintln!("警告: 密钥生成失败: {}, 重试...", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// 判断磁盘上的 adbkey.pub 是否与给定私钥同属一把钥匙（按 RSA 模数 n 比对，
/// 不受公钥文本编码/注释差异影响）。这样既能修复「公私钥不配对导致永远弹授权」，
/// 又不会误改与设备已配对的官方公钥。
fn pub_matches_key(pub_path: &Path, key: &RsaPrivateKey) -> bool {
    let content = match fs::read_to_string(pub_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let b64 = match content.split_whitespace().next() {
        Some(s) => s,
        None => return false,
    };
    let blob = match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
        Ok(b) => b,
        Err(_) => return false,
    };
    // Android 公钥结构: [words:4][n0inv:4][modulus(LE):256][rr:256][e:4]
    if blob.len() < 8 + 256 {
        return false;
    }
    let modulus = &blob[8..8 + 256];
    let n_le = key.n().to_bytes_le();
    let mut n_256 = [0u8; 256];
    for (i, &b) in n_le.iter().enumerate().take(256) {
        n_256[i] = b;
    }
    modulus == n_256
}

/// 确保 adbkey.pub 与私钥配对：缺失或不匹配才重写（重写也只在确属不同密钥时发生）。
fn ensure_pub_consistent(pub_path: &Path, key: &RsaPrivateKey) {
    if pub_matches_key(pub_path, key) {
        return;
    }
    let expected = generate_adb_public_key(key);
    if let Err(e) = write_atomic(pub_path, expected.as_bytes()) {
        eprintln!("警告: 修复 adbkey.pub 失败: {}", e);
    }
}

/// 落盘一对密钥（私钥 PKCS#8，与官方 adb 一致；公钥为 Android 二进制格式）。
fn persist_keypair(priv_path: &Path, pub_path: &Path, key: &RsaPrivateKey) {
    match key.to_pkcs8_pem(LineEnding::LF) {
        Ok(pem) => {
            if let Err(e) = write_atomic(priv_path, pem.as_bytes()) {
                eprintln!("警告: 写入私钥失败: {}", e);
            }
        }
        Err(e) => eprintln!("警告: 私钥格式化失败: {}", e),
    }
    let pub_str = generate_adb_public_key(key);
    if let Err(e) = write_atomic(pub_path, pub_str.as_bytes()) {
        eprintln!("警告: 写入公钥失败: {}", e);
    }
}

/// 加载或（在确实缺失/损坏时）生成 adb 私钥。
///
/// 持久化策略（修复原作者未完成处）：
/// 1. 已存在且可解析 → 直接复用（同时校正 adbkey.pub），**不重新生成**；
/// 2. 文件可读但格式不识别（PKCS#8/#1 均失败）→ 先备份原文件再重建，绝不静默丢弃；
/// 3. 文件存在却暂时读不出（被占用等）→ 本次使用临时内存密钥，**绝不改动磁盘原密钥**；
/// 4. 真正缺失 → 生成并原子落盘。
pub fn load_or_generate_private_key() -> RsaPrivateKey {
    let dir = android_dir();
    let priv_path = dir.join("adbkey");
    let pub_path = dir.join("adbkey.pub");

    if priv_path.exists() {
        match fs::read_to_string(&priv_path) {
            Ok(content) => {
                if let Some(key) = parse_private_key_any(&content) {
                    ensure_pub_consistent(&pub_path, &key);
                    return key;
                }
                eprintln!(
                    "警告: 现有 adbkey 无法解析(PKCS#8/#1 均失败)，已备份并重新生成，设备需重新授权一次"
                );
                backup_file(&priv_path);
                backup_file(&pub_path);
            }
            Err(e) => {
                eprintln!(
                    "警告: 暂时无法读取 adbkey({})，本次使用临时密钥且不改动磁盘文件",
                    e
                );
                return generate_private_key_in_memory();
            }
        }
    }

    let priv_key = generate_private_key_in_memory();
    persist_keypair(&priv_path, &pub_path, &priv_key);
    priv_key
}

pub fn sign_token(priv_pem: &str, token: &[u8]) -> Result<Vec<u8>, &'static str> {
    let priv_key = RsaPrivateKey::from_pkcs8_pem(priv_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(priv_pem))
        .map_err(|_| "私钥解析失败")?;

    let mut digest_info = Vec::with_capacity(35);
    digest_info.extend_from_slice(&[
        0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14,
    ]);
    digest_info.extend_from_slice(token);

    let padding = rsa::pkcs1v15::Pkcs1v15Sign::new_raw();
    let signature = priv_key
        .sign(padding, &digest_info)
        .map_err(|_| "裸签计算失败")?;

    Ok(signature)
}

pub fn get_public_key() -> Vec<u8> {
    let priv_key = load_or_generate_private_key();
    let pub_path = android_dir().join("adbkey.pub");
    match fs::read(&pub_path) {
        Ok(mut data) => {
            if !data.ends_with(&[0]) {
                data.push(0);
            }
            data
        }
        Err(_) => generate_adb_public_key(&priv_key).into_bytes(),
    }
}

pub fn get_or_create_keys() -> Result<(String, Vec<u8>), &'static str> {
    let priv_key = load_or_generate_private_key();

    let priv_pem = priv_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|_| "无法生成私钥 PEM")?
        .to_string();

    // load_or_generate_private_key 已确保 adbkey.pub 与私钥配对，这里直接读文件即可，
    // 避免再次进入 load_or_generate 造成重复工作。
    let pub_path = android_dir().join("adbkey.pub");
    let pub_key = match fs::read(&pub_path) {
        Ok(mut d) => {
            if !d.ends_with(&[0]) {
                d.push(0);
            }
            d
        }
        Err(_) => generate_adb_public_key(&priv_key).into_bytes(),
    };

    Ok((priv_pem, pub_key))
}
