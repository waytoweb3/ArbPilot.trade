//! Aster Pro API (V3) 的 API 钱包签名。
//!
//! V3 不再使用 API Key + HMAC，而是「主账户 + 授权的 API 钱包」：
//! 请求带上 `signer` 与微秒 `nonce`，参数串经 EIP-712 结构签名后
//! 用 API 钱包私钥做 secp256k1 签名。签名全部在本机完成，私钥不出本机。

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context};
use chrono::Utc;
use serde_json::Value;
use sha3::{Digest, Keccak256};

use crate::http_client::HttpClient;

/// Aster L1 的 chain id，签名域固定为 `AsterSignTransaction`。
const CHAIN_ID: u64 = 1666;

/// nonce 必须严格递增，服务端只保留每个 API 钱包最近 100 个 nonce。
static LAST_NONCE: AtomicU64 = AtomicU64::new(0);

/// 一个已授权的 Aster API 钱包，只持有私钥。
pub struct AsterWallet {
    signing_key: k256::ecdsa::SigningKey,
}

impl AsterWallet {
    /// 从 16 进制私钥构造，兼容 `0x` 前缀。
    pub fn from_private_key(raw: &str) -> anyhow::Result<Self> {
        let text = raw.trim();
        let text = text.strip_prefix("0x").unwrap_or(text);
        let bytes =
            hex::decode(text).map_err(|_| anyhow!("Aster API 钱包私钥不是合法的十六进制"))?;
        if bytes.len() != 32 {
            bail!("Aster API 钱包私钥必须是 32 字节（64 位十六进制）");
        }
        let signing_key = k256::ecdsa::SigningKey::from_slice(&bytes)
            .map_err(|_| anyhow!("Aster API 钱包私钥不合法"))?;
        Ok(Self { signing_key })
    }

    /// API 钱包地址（`0x` + 40 位十六进制），由私钥推导得到。
    pub fn address(&self) -> String {
        let point = self.signing_key.verifying_key().to_encoded_point(false);
        let hash = keccak(&point.as_bytes()[1..]);
        format!("0x{}", hex::encode(&hash[12..]))
    }

    /// 对参数串签名，返回 `0x` + r + s + v 的 65 字节十六进制。
    pub fn sign(&self, payload: &str) -> anyhow::Result<String> {
        let digest = eip712_digest(payload);
        let (signature, recovery) = self
            .signing_key
            .sign_prehash_recoverable(&digest)
            .map_err(|error| anyhow!("Aster 签名失败: {error}"))?;
        let v = 27 + recovery.to_byte();
        Ok(format!("0x{}{:02x}", hex::encode(signature.to_bytes()), v))
    }
}

/// 一次 V3 请求所需的凭证：主账户地址 + 已授权的 API 钱包私钥。
pub struct AsterSigner {
    user: String,
    wallet: AsterWallet,
}

impl AsterSigner {
    /// `user` 是主账户钱包地址，`signer_key` 是 API 钱包私钥，signer 地址由其推导。
    pub fn new(user: &str, signer_key: &str) -> anyhow::Result<Self> {
        let user = user.trim();
        let address = user
            .strip_prefix("0x")
            .or_else(|| user.strip_prefix("0X"))
            .unwrap_or(user);
        if address.len() != 40 || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("Aster 主账户地址格式不正确（应为 0x 开头的 40 位十六进制）");
        }
        Ok(Self {
            user: format!("0x{}", address.to_ascii_lowercase()),
            wallet: AsterWallet::from_private_key(signer_key)?,
        })
    }

    /// API 钱包地址，即授权给主账户的 `signer`。
    pub fn signer_address(&self) -> String {
        self.wallet.address()
    }

    /// 组装并签名完整参数串，含 `user` / `signer` / `nonce` / `signature`。
    pub fn signed_form(&self, params: &[(&str, String)]) -> anyhow::Result<String> {
        let mut all: Vec<(&str, String)> = Vec::with_capacity(params.len() + 4);
        all.extend(params.iter().cloned());
        all.push(("user", self.user.clone()));
        all.push(("signer", self.signer_address()));
        all.push(("nonce", next_nonce().to_string()));
        let payload = form_encode(&all);
        let signature = self.wallet.sign(&payload)?;
        Ok(format!("{payload}&signature={signature}"))
    }
}

/// 发起一次 V3 私有请求：GET 走查询串，其余方法把表单放在请求体。
pub fn request(
    http: &HttpClient,
    method: &str,
    base_url: &str,
    path: &str,
    signer: &AsterSigner,
    params: &[(&str, String)],
) -> anyhow::Result<Value> {
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let form = signer.signed_form(params)?;
    let raw = if method.eq_ignore_ascii_case("GET") {
        http.request("GET", &format!("{url}?{form}"), &[], None)?
    } else {
        http.request(
            method,
            &url,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            Some(&form),
        )?
    };
    serde_json::from_slice(&raw).with_context(|| format!("解析 Aster 响应失败: {url}"))
}

/// Aster 的错误响应统一是 `{"code": -xxxx, "msg": "..."}`。
pub fn error_of(value: &Value) -> String {
    match value["code"].as_i64() {
        Some(code) => format!(
            "Aster 返回错误 {code}: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        ),
        None => format!("Aster 响应格式异常: {value}"),
    }
}

/// 生成微秒级 nonce，并保证在本进程内严格递增。
pub fn next_nonce() -> u64 {
    let now = Utc::now().timestamp_micros().max(0) as u64;
    loop {
        let last = LAST_NONCE.load(Ordering::Relaxed);
        let next = if now > last { now } else { last + 1 };
        if LAST_NONCE
            .compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

/// 表单编码：保留 RFC 3986 非保留字符，其余按 `%XX` 编码，空格用 `+`。
pub fn form_encode(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// EIP-712 摘要：域名固定为 Aster L1，消息体是参数串。
fn eip712_digest(payload: &str) -> [u8; 32] {
    let domain_type = keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut domain = Vec::with_capacity(32 * 5);
    domain.extend_from_slice(&domain_type);
    domain.extend_from_slice(&keccak(b"AsterSignTransaction"));
    domain.extend_from_slice(&keccak(b"1"));
    let mut chain_id = [0u8; 32];
    chain_id[24..].copy_from_slice(&CHAIN_ID.to_be_bytes());
    domain.extend_from_slice(&chain_id);
    domain.extend_from_slice(&[0u8; 32]);

    let struct_hash = {
        let mut encoded = keccak(b"Message(string msg)").to_vec();
        encoded.extend_from_slice(&keccak(payload.as_bytes()));
        keccak(&encoded)
    };

    let mut preimage = vec![0x19, 0x01];
    preimage.extend_from_slice(&keccak(&domain));
    preimage.extend_from_slice(&struct_hash);
    keccak(&preimage)
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let digest = Keccak256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aster 官方文档里的 API 钱包示例私钥与地址。
    #[test]
    fn derives_signer_address_from_private_key() {
        let wallet = AsterWallet::from_private_key(
            "0x4fd0a42218f3eae43a6ce26d22544e986139a01e5b34a62db53757ffca81bae1",
        )
        .expect("私钥应可解析");
        assert_eq!(
            wallet.address().to_ascii_lowercase(),
            "0x21cf8ae13bb72632562c6fff438652ba1a151bb0"
        );
    }

    /// 与 eth_account 的 EIP-712 实现对齐：同一参数串必须得到相同签名。
    #[test]
    fn signs_payload_like_eth_account() {
        let wallet = AsterWallet::from_private_key(
            "0x4fd0a42218f3eae43a6ce26d22544e986139a01e5b34a62db53757ffca81bae1",
        )
        .expect("私钥应可解析");
        let payload = "symbol=BTCUSDT&side=BUY&type=LIMIT&timeInForce=GTC&quantity=0.01&price=50000&nonce=1748310859508867&signer=0x21cF8Ae13Bb72632562c6Fff438652Ba1a151bb0";
        assert_eq!(
            wallet.sign(payload).expect("签名应成功"),
            "0x13cdf86eb65bcd0bb1ccb9c604390e3dd5e6b271360f0c5b65b2a322f29b5ef61ec8d4cba49b9f0ebebcf4200bd0f960bd23ccdc694b48d53db1b6cf6942746f1c"
        );
    }

    #[test]
    fn encodes_form_parameters() {
        assert_eq!(
            form_encode(&[
                ("nonce", "1748310859508867".to_string()),
                ("signer", "0xAbC".to_string())
            ]),
            "nonce=1748310859508867&signer=0xAbC"
        );
    }
}
