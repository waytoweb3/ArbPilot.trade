//! 交易所凭证的本地加密存储。
//!
//! 主密钥优先存放在系统钥匙串（macOS Keychain / Windows Credential Manager）；
//! Linux 自托管环境没有稳定的钥匙串服务，因此退回到数据目录下仅本人可读的密钥文件。
//! 凭证文件本身用 ChaCha20-Poly1305 加密，每次写入都使用新的随机 nonce。

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const ENVELOPE_VERSION: u32 = 1;
const KEY_FILE_NAME: &str = "accounts.key";
#[cfg(all(not(test), any(target_os = "macos", target_os = "windows")))]
const KEYCHAIN_SERVICE: &str = "ArbPilot";
#[cfg(all(not(test), any(target_os = "macos", target_os = "windows")))]
const KEYCHAIN_ACCOUNT: &str = "accounts-master-key";

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// 主密钥来源；`Unavailable` 只用于初始化失败时向界面如实展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    Keychain,
    KeyFile,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageInfo {
    pub encrypted: bool,
    pub backend: StorageBackend,
    /// 凭证文件路径；自托管时是服务器上的路径。
    pub path: String,
    /// 凭证文件是否已创建。
    pub exists: bool,
    /// Unix 权限位（如 `rw-------`）；Windows 或文件不存在时为 null。
    pub permissions: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    alg: String,
    nonce: String,
    data: String,
}

/// 凭证加解密器：持有一个进程内的主密钥副本，不把密钥写进凭证文件。
pub struct CredentialCipher {
    key: LessSafeKey,
    backend: StorageBackend,
}

impl CredentialCipher {
    /// 打开（必要时创建）本机主密钥。初始化失败时调用方应停止写入凭证，避免静默退回明文。
    pub fn open(data_file: &Path) -> anyhow::Result<Self> {
        let key_file = key_file_path(data_file);
        let (key, backend) = load_or_create_key(&key_file, data_file)?;
        let unbound = UnboundKey::new(&CHACHA20_POLY1305, &key)
            .map_err(|_| anyhow!("主密钥长度无效"))?;
        Ok(Self {
            key: LessSafeKey::new(unbound),
            backend,
        })
    }

    pub fn info(&self, data_file: &Path) -> StorageInfo {
        StorageInfo {
            encrypted: true,
            backend: self.backend,
            path: data_file.display().to_string(),
            exists: data_file.exists(),
            permissions: file_permissions(data_file),
        }
    }

    /// 判断文件是不是本模块写出的加密信封。
    pub fn is_encrypted(raw: &[u8]) -> bool {
        envelope_of(raw).is_some()
    }

    /// 加密任意 JSON 明文；格式为带版本号的信封，便于以后轮换算法。
    pub fn seal(&self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| anyhow!("生成随机 nonce 失败"))?;

        let mut ciphertext = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::empty(),
                &mut ciphertext,
            )
            .map_err(|_| anyhow!("加密凭证失败"))?;

        let envelope = Envelope {
            version: ENVELOPE_VERSION,
            alg: "chacha20_poly1305".to_string(),
            nonce: BASE64.encode(nonce_bytes),
            data: BASE64.encode(ciphertext),
        };
        serde_json::to_vec_pretty(&envelope).context("序列化加密信封失败")
    }

    /// 解密信封；非信封内容按历史明文原样返回，供首次升级时迁移。
    pub fn unseal(&self, raw: &[u8]) -> anyhow::Result<Vec<u8>> {
        let Some(envelope) = envelope_of(raw) else {
            return Ok(raw.to_vec());
        };
        if envelope.version != ENVELOPE_VERSION {
            bail!("不支持的凭证文件版本: {}", envelope.version);
        }

        let nonce = BASE64
            .decode(envelope.nonce.as_bytes())
            .context("解析 nonce 失败")?;
        let nonce: [u8; NONCE_LEN] = nonce
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("nonce 长度无效"))?;

        let mut data = BASE64
            .decode(envelope.data.as_bytes())
            .context("解析密文失败")?;
        let plaintext = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut data,
            )
            .map_err(|_| anyhow!("解密凭证失败：主密钥不匹配或文件已损坏"))?;
        Ok(plaintext.to_vec())
    }
}

fn envelope_of(raw: &[u8]) -> Option<Envelope> {
    let value: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let object = value.as_object()?;
    if !object.contains_key("data") || !object.contains_key("nonce") {
        return None;
    }
    serde_json::from_value(value).ok()
}

fn key_file_path(data_file: &Path) -> PathBuf {
    data_file.with_file_name(KEY_FILE_NAME)
}

fn load_or_create_key(
    key_file: &Path,
    data_file: &Path,
) -> anyhow::Result<([u8; KEY_LEN], StorageBackend)> {
    // macOS 调试构建是 ad-hoc 签名，每次重新编译都会改变 cdhash，系统钥匙串随之拒绝
    // 旧条目（访问控制按 cdhash 绑定）。因此调试态优先读本机密钥文件，避免开发过程中
    // 每改一次代码就“凭证加密不可用”。
    #[cfg(all(debug_assertions, target_os = "macos"))]
    if let Some(key) = read_key_file(key_file) {
        return Ok((key, StorageBackend::KeyFile));
    }

    if let Some(key) = keychain_key() {
        // 首次从钥匙串读到主密钥时镜像一份到密钥文件；之后重建二进制也能直接解密，
        // 不必再让系统弹窗授权。
        #[cfg(all(debug_assertions, target_os = "macos"))]
        match write_key_file(key_file, &key) {
            Ok(()) => tracing::info!(path = %key_file.display(), "已将系统钥匙串主密钥镜像到本机密钥文件"),
            Err(error) => tracing::warn!(%error, path = %key_file.display(), "镜像主密钥到本机密钥文件失败，继续使用系统钥匙串"),
        }
        return Ok((key, StorageBackend::Keychain));
    }
    if let Some(key) = read_key_file(key_file) {
        tracing::info!(path = %key_file.display(), "使用本机密钥文件解密交易所凭证");
        return Ok((key, StorageBackend::KeyFile));
    }
    // 已有加密凭证但没有可用主密钥时，绝不静默生成新密钥：那会永久覆盖掉旧凭证。
    if fs::read(data_file)
        .map(|raw| envelope_of(&raw).is_some())
        .unwrap_or(false)
    {
        bail!("系统钥匙串与密钥文件均不可用，无法解密已有的凭证文件");
    }

    let key = random_key()?;
    write_key_file(key_file, &key)?;
    tracing::info!(path = %key_file.display(), "已生成本机主密钥");
    Ok((key, StorageBackend::KeyFile))
}

fn read_key_file(path: &Path) -> Option<[u8; KEY_LEN]> {
    let raw = fs::read_to_string(path).ok()?;
    match decode_key(raw.trim()) {
        Ok(key) => Some(key),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "读取主密钥文件失败");
            None
        }
    }
}

fn write_key_file(path: &Path, key: &[u8; KEY_LEN]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建密钥目录失败: {}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(KEY_FILE_NAME)
    ));
    fs::write(&tmp, hex::encode(key)).context("写入主密钥失败")?;
    restrict_permissions(&tmp)?;
    fs::rename(&tmp, path).context("保存主密钥失败")?;
    restrict_permissions(path)?;
    Ok(())
}

fn decode_key(value: &str) -> anyhow::Result<[u8; KEY_LEN]> {
    let bytes = if value.len() == KEY_LEN * 2 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        hex::decode(value).context("主密钥不是有效的十六进制")?
    } else {
        BASE64
            .decode(value.as_bytes())
            .context("主密钥不是有效的 base64")?
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("主密钥长度必须是 {KEY_LEN} 字节"))
}

fn random_key() -> anyhow::Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow!("生成主密钥失败"))?;
    Ok(key)
}

// 单元测试不触碰真实钥匙串，统一使用临时目录里的密钥文件。
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
fn keychain_key() -> Option<[u8; KEY_LEN]> {
    None
}

#[cfg(all(not(test), any(target_os = "macos", target_os = "windows")))]
fn keychain_key() -> Option<[u8; KEY_LEN]> {
    let entry = match keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(entry) => entry,
        Err(error) => {
            tracing::warn!(%error, "打开系统钥匙串失败");
            return None;
        }
    };

    match entry.get_password() {
        Ok(encoded) => match decode_key(&encoded) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::warn!(%error, "系统钥匙串中的主密钥格式无效");
                None
            }
        },
        Err(keyring::Error::NoEntry) => {
            let key = random_key().ok()?;
            match entry.set_password(&BASE64.encode(key)) {
                Ok(()) => {
                    tracing::info!("已在系统钥匙串中创建凭证主密钥");
                    Some(key)
                }
                Err(error) => {
                    tracing::warn!(%error, "写入系统钥匙串失败");
                    None
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "读取系统钥匙串失败");
            None
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn keychain_key() -> Option<[u8; KEY_LEN]> {
    None
}

/// 读取文件权限并转成 `rw-------` 这样的可读形式。
#[cfg(unix)]
pub fn file_permissions(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path).ok()?.permissions().mode() & 0o777;
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    Some(
        bits.iter()
            .map(|(bit, label)| if mode & bit != 0 { *label } else { '-' })
            .collect(),
    )
}

#[cfg(not(unix))]
pub fn file_permissions(_path: &Path) -> Option<String> {
    None
}

/// 把文件权限收紧为仅所有者可读写。
#[cfg(unix)]
pub fn restrict_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arbpilot-secret-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    #[test]
    fn round_trip_and_detects_envelope() {
        let dir = temp_dir();
        let data_file = dir.join("accounts.json");
        let cipher = CredentialCipher::open(&data_file).expect("初始化加密器");
        let plaintext = br#"[{"api_key":"secret"}]"#;

        let sealed = cipher.seal(plaintext).expect("加密");
        assert!(CredentialCipher::is_encrypted(&sealed));
        assert!(!String::from_utf8_lossy(&sealed).contains("secret"));
        assert_eq!(cipher.unseal(&sealed).expect("解密"), plaintext);
        assert_eq!(cipher.unseal(plaintext).expect("明文直通"), plaintext);
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let dir = temp_dir();
        let data_file = dir.join("accounts.json");
        let cipher = CredentialCipher::open(&data_file).expect("初始化加密器");
        let sealed = cipher.seal(b"[]").expect("加密");
        let mut envelope: Envelope = serde_json::from_slice(&sealed).expect("解析信封");
        let mut data = BASE64.decode(envelope.data.as_bytes()).expect("解码密文");
        data[0] ^= 0xff;
        envelope.data = BASE64.encode(data);
        let tampered = serde_json::to_vec(&envelope).expect("序列化");

        assert!(cipher.unseal(&tampered).is_err());
    }
}
