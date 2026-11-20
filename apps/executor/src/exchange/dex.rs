//! DEX 签名工具：Hyperliquid 的 EIP-712 + msgpack 签名，Lighter 的 Schnorr 交易签名。
//!
//! 两家 DEX 都要求由钱包或 API Key 私钥签名后才能下单，签名过程完全在本机完成，
//! 私钥不会发往任何服务器；提交给交易所的只有已签名的订单。

use anyhow::{anyhow, bail, Context};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;

use lighters::{
    constants::{
        CROSS_MARGIN, GOOD_TILL_TIME, LIMIT_ORDER, MAX_ORDER_EXPIRY, TX_TYPE_L2_CREATE_ORDER,
    },
    signer::KeyManager,
    transactions::{sign_tx, CreateOrder, OrderInfo, TxInfo, UpdateLeverage},
};

/// Lighter 主网 chain id；测试网为 300。
pub const LIGHTER_MAINNET_CHAIN_ID: u32 = 304;

/// 以太坊 EIP-712 里 `Exchange` 域的固定 chain id，与 Hyperliquid 官方 SDK 一致。
const HYPERLIQUID_EIP712_CHAIN_ID: u64 = 1337;

/// 从 16 进制私钥解析出 32 字节，兼容 `0x` 前缀。
fn private_key_bytes(raw: &str) -> anyhow::Result<[u8; 32]> {
    let text = raw.trim();
    let text = text.strip_prefix("0x").unwrap_or(text);
    let bytes = hex::decode(text).context("私钥不是合法的十六进制")?;
    if bytes.len() != 32 {
        bail!("私钥必须是 32 字节（64 位十六进制）");
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// 导入时校验签名私钥格式：Hyperliquid 为 32 字节，Lighter 的 API Key 为 40 字节，
/// Aster 的 API 钱包私钥为 32 字节。
pub fn validate_signer_key(venue: &str, key: &str) -> anyhow::Result<()> {
    match venue {
        "hyperliquid" => private_key_bytes(key).map(|_| ()),
        "aster" => crate::exchange::aster::AsterWallet::from_private_key(key).map(|_| ()),
        "lighter" => KeyManager::from_hex(key)
            .map(|_| ())
            .map_err(|error| anyhow!("Lighter 私钥不合法: {error}")),
        _ => Ok(()),
    }
}

/// 去掉多余的 0，得到交易所要求的「最短十进制」表示。
fn plain(value: Decimal) -> String {
    value.normalize().to_string()
}

// ---------------------------------------------------------------------------
// Hyperliquid
// ---------------------------------------------------------------------------

/// Hyperliquid 限价单参数，字段顺序与官方 SDK 的 wire 结构一致。
pub struct HyperliquidOrder<'a> {
    pub coin: &'a str,
    pub asset: u32,
    /// 每张合约的最小价格精度。
    pub size_decimals: u32,
    pub is_buy: bool,
    pub price: Decimal,
    pub size: Decimal,
    pub reduce_only: bool,
    pub cloid: Option<String>,
}

impl HyperliquidOrder<'_> {
    /// 价格最多 5 位有效数字，且小数位不超过 `6 - size_decimals`。
    fn wire_price(&self) -> Result<String, String> {
        let max_decimals = 6u32.saturating_sub(self.size_decimals);
        let text = format!("{:.*}", max_decimals as usize, self.price);
        let (int_part, frac_part) = text.split_once('.').unwrap_or((text.as_str(), ""));
        let int_digits = int_part
            .trim_start_matches('-')
            .trim_start_matches('0')
            .len();
        let allowed = if int_digits > 0 {
            (5i64 - int_digits as i64).max(0) as u32
        } else {
            match frac_part.find(|ch: char| ch != '0') {
                Some(position) => position as u32 + 4,
                None => 0,
            }
        };
        let decimals = max_decimals.min(allowed);
        let rounded = self
            .price
            .round_dp_with_strategy(decimals, RoundingStrategy::MidpointAwayFromZero);
        if rounded <= Decimal::ZERO {
            return Err(format!("{} 价格精度不足，四舍五入后为 0", self.coin));
        }
        Ok(plain(rounded))
    }

    fn wire_size(&self) -> Result<String, String> {
        let rounded = self.size.round_dp(self.size_decimals);
        if rounded <= Decimal::ZERO {
            return Err(format!(
                "{} 数量小于最小精度 1e-{}",
                self.coin, self.size_decimals
            ));
        }
        Ok(plain(rounded))
    }

    /// 序列化 order action：字段顺序必须与官方 SDK 完全一致，否则签名不通过。
    pub(crate) fn action(&self) -> Result<Vec<u8>, String> {
        let price = self.wire_price()?;
        let size = self.wire_size()?;
        let mut out = Vec::with_capacity(160);
        // {"type": "order", "orders": [...], "grouping": "na"}
        msgpack_map(&mut out, 3);
        msgpack_str(&mut out, "type");
        msgpack_str(&mut out, "order");
        msgpack_str(&mut out, "orders");
        msgpack_array(&mut out, 1);
        msgpack_map(&mut out, if self.cloid.is_some() { 7 } else { 6 });
        msgpack_str(&mut out, "a");
        msgpack_uint(&mut out, u64::from(self.asset));
        msgpack_str(&mut out, "b");
        msgpack_bool(&mut out, self.is_buy);
        msgpack_str(&mut out, "p");
        msgpack_str(&mut out, &price);
        msgpack_str(&mut out, "s");
        msgpack_str(&mut out, &size);
        msgpack_str(&mut out, "r");
        msgpack_bool(&mut out, self.reduce_only);
        msgpack_str(&mut out, "t");
        // {"limit": {"tif": "Gtc"}}
        msgpack_map(&mut out, 1);
        msgpack_str(&mut out, "limit");
        msgpack_map(&mut out, 1);
        msgpack_str(&mut out, "tif");
        msgpack_str(&mut out, "Gtc");
        if let Some(cloid) = &self.cloid {
            msgpack_str(&mut out, "c");
            msgpack_str(&mut out, cloid);
        }
        msgpack_str(&mut out, "grouping");
        msgpack_str(&mut out, "na");
        Ok(out)
    }

    /// 与 msgpack 版本等价的 JSON action，提交给 `/exchange`。
    ///
    /// 服务端按字段名解析，顺序无关；价格与数量复用同一份字符串，保证签名与提交内容一致。
    pub fn action_value(&self) -> Result<Value, String> {
        let price = self.wire_price()?;
        let size = self.wire_size()?;
        let mut order = Map::new();
        order.insert("a".to_string(), json!(self.asset));
        order.insert("b".to_string(), json!(self.is_buy));
        order.insert("p".to_string(), json!(price));
        order.insert("s".to_string(), json!(size));
        order.insert("r".to_string(), json!(self.reduce_only));
        order.insert("t".to_string(), json!({ "limit": { "tif": "Gtc" } }));
        if let Some(cloid) = &self.cloid {
            order.insert("c".to_string(), json!(cloid));
        }
        Ok(json!({
            "type": "order",
            "orders": [Value::Object(order)],
            "grouping": "na",
        }))
    }
}

/// Hyperliquid `updateLeverage` action 的 msgpack 序列化，字段顺序与官方 SDK 一致。
pub fn hyperliquid_update_leverage_action(asset: u32, is_cross: bool, leverage: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    msgpack_map(&mut out, 4);
    msgpack_str(&mut out, "type");
    msgpack_str(&mut out, "updateLeverage");
    msgpack_str(&mut out, "asset");
    msgpack_uint(&mut out, u64::from(asset));
    msgpack_str(&mut out, "isCross");
    msgpack_bool(&mut out, is_cross);
    msgpack_str(&mut out, "leverage");
    msgpack_uint(&mut out, u64::from(leverage));
    out
}

/// Hyperliquid `cancel` action 的 msgpack 与 JSON 序列化。
///
/// 撤单必须带上合约的 asset index；有数字 oid 时按 oid 撤，否则按 cloid 撤。
pub fn hyperliquid_cancel_action(
    asset: u32,
    oid: Option<u64>,
    cloid: Option<&str>,
) -> (Vec<u8>, Value) {
    let mut out = Vec::new();
    msgpack_map(&mut out, 2);
    msgpack_str(&mut out, "type");
    msgpack_str(&mut out, "cancel");
    msgpack_str(&mut out, "cancels");
    msgpack_array(&mut out, 1);
    msgpack_map(&mut out, 2);
    msgpack_str(&mut out, "a");
    msgpack_uint(&mut out, u64::from(asset));
    let cancel = match (oid, cloid) {
        (Some(oid), _) => {
            msgpack_str(&mut out, "o");
            msgpack_uint(&mut out, oid);
            json!({ "a": asset, "o": oid })
        }
        (None, Some(cloid)) => {
            msgpack_str(&mut out, "c");
            msgpack_str(&mut out, cloid);
            json!({ "a": asset, "c": cloid })
        }
        (None, None) => {
            msgpack_str(&mut out, "o");
            msgpack_uint(&mut out, 0);
            json!({ "a": asset, "o": 0 })
        }
    };
    (out, json!({ "type": "cancel", "cancels": [cancel] }))
}

/// Hyperliquid 的签名结果，`v` 为 27/28。
pub struct HyperliquidSignature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

/// 对 L1 action 签名：`keccak(msgpack(action) || nonce || 0x00)`，
/// 再按 Phantom Agent 的 EIP-712 结构签名。
pub fn sign_hyperliquid_action(
    action: &[u8],
    nonce: u64,
    private_key: &str,
    mainnet: bool,
) -> anyhow::Result<HyperliquidSignature> {
    let mut payload = action.to_vec();
    payload.extend_from_slice(&nonce.to_be_bytes());
    // vaultAddress 为空时追加一个 0x00 标记。
    payload.push(0);
    let action_hash = keccak(&payload);

    let domain_separator = {
        let mut encoded = keccak(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        )
        .to_vec();
        encoded.extend_from_slice(&keccak(b"Exchange"));
        encoded.extend_from_slice(&keccak(b"1"));
        let mut chain_id = [0u8; 32];
        chain_id[24..].copy_from_slice(&HYPERLIQUID_EIP712_CHAIN_ID.to_be_bytes());
        encoded.extend_from_slice(&chain_id);
        encoded.extend_from_slice(&[0u8; 32]);
        keccak(&encoded)
    };

    let struct_hash = {
        let mut encoded = keccak(b"Agent(string source,bytes32 connectionId)").to_vec();
        encoded.extend_from_slice(&keccak(if mainnet { b"a" } else { b"b" }));
        encoded.extend_from_slice(&action_hash);
        keccak(&encoded)
    };

    let mut preimage = vec![0x19, 0x01];
    preimage.extend_from_slice(&domain_separator);
    preimage.extend_from_slice(&struct_hash);
    let digest = keccak(&preimage);

    let key = private_key_bytes(private_key)?;
    let signing_key = k256::ecdsa::SigningKey::from_slice(&key)
        .map_err(|_| anyhow!("Hyperliquid 签名私钥不合法"))?;
    let (signature, recovery) = signing_key
        .sign_prehash_recoverable(&digest)
        .map_err(|error| anyhow!("Hyperliquid 签名失败: {error}"))?;
    let bytes = signature.to_bytes();
    let (r, s) = bytes.split_at(32);

    Ok(HyperliquidSignature {
        r: format!("0x{}", trim_hex(r)),
        s: format!("0x{}", trim_hex(s)),
        v: 27 + recovery.to_byte(),
    })
}

/// Hyperliquid 要求 cloid 是 128 位十六进制，用客户端订单号哈希得到稳定值。
pub fn hyperliquid_cloid(client_order_id: &str) -> String {
    let digest = Sha256::digest(client_order_id.as_bytes());
    format!("0x{}", hex::encode(&digest[..16]))
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let digest = Keccak256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// 十六进制去掉前导 0（与官方 SDK 的 `to_hex` 一致）。
fn trim_hex(bytes: &[u8]) -> String {
    let text = hex::encode(bytes);
    let trimmed = text.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// MessagePack：只覆盖 Hyperliquid 订单所需的类型
// ---------------------------------------------------------------------------

fn msgpack_map(out: &mut Vec<u8>, len: usize) {
    if len < 16 {
        out.push(0x80 | len as u8);
    } else {
        out.push(0xde);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    }
}

fn msgpack_array(out: &mut Vec<u8>, len: usize) {
    if len < 16 {
        out.push(0x90 | len as u8);
    } else {
        out.push(0xdc);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    }
}

fn msgpack_str(out: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    match bytes.len() {
        len if len < 32 => out.push(0xa0 | len as u8),
        len if len < 256 => {
            out.push(0xd9);
            out.push(len as u8);
        }
        len if len < 65_536 => {
            out.push(0xda);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(0xdb);
            out.extend_from_slice(&(len as u32).to_be_bytes());
        }
    }
    out.extend_from_slice(bytes);
}

fn msgpack_bool(out: &mut Vec<u8>, value: bool) {
    out.push(if value { 0xc3 } else { 0xc2 });
}

fn msgpack_uint(out: &mut Vec<u8>, value: u64) {
    if value < 128 {
        out.push(value as u8);
    } else if value <= u64::from(u8::MAX) {
        out.push(0xcc);
        out.push(value as u8);
    } else if value <= u64::from(u16::MAX) {
        out.push(0xcd);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u64::from(u32::MAX) {
        out.push(0xce);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

// ---------------------------------------------------------------------------
// Lighter
// ---------------------------------------------------------------------------

/// Lighter 下单参数，金额与价格都需要先换算成整数刻度。
pub struct LighterOrder {
    pub account_index: i64,
    pub api_key_index: u8,
    pub market_index: i16,
    pub size_decimals: u32,
    pub price_decimals: u32,
    pub is_ask: bool,
    pub price: Decimal,
    pub size: Decimal,
    pub reduce_only: bool,
    pub nonce: i64,
    pub expired_at: i64,
}

/// Lighter 已签名订单，可直接提交到 `/api/v1/sendTx`。
pub struct LighterSigned {
    pub tx_type: u8,
    pub tx_info: String,
}

pub fn sign_lighter_order(
    order: &LighterOrder,
    private_key: &str,
    chain_id: u32,
) -> anyhow::Result<LighterSigned> {
    let scale = |decimals: u32| Decimal::from(10u64.pow(decimals));
    let base_scale = scale(order.size_decimals);
    let price_scale = scale(order.price_decimals);

    let base_amount = (order.size * base_scale)
        .round_dp(0)
        .to_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("Lighter 数量小于最小精度 1e-{}", order.size_decimals))?;
    let price = (order.price * price_scale)
        .round_dp(0)
        .to_u64()
        .filter(|value| *value <= u64::from(u32::MAX) && *value > 0)
        .ok_or_else(|| anyhow!("Lighter 价格超出可表示范围"))? as u32;

    let key = KeyManager::from_hex(private_key)
        .map_err(|error| anyhow!("Lighter 私钥不合法: {error}"))?;

    let mut tx = CreateOrder {
        account_index: order.account_index,
        api_key_index: order.api_key_index,
        order: OrderInfo {
            market_index: order.market_index,
            client_order_index: 0,
            base_amount,
            price,
            is_ask: u8::from(order.is_ask),
            order_type: LIMIT_ORDER,
            time_in_force: GOOD_TILL_TIME,
            reduce_only: u8::from(order.reduce_only),
            trigger_price: 0,
            order_expiry: order.expired_at.min(MAX_ORDER_EXPIRY),
        },
        expired_at: order.expired_at,
        nonce: order.nonce,
        sig: Vec::new(),
        signed_hash: String::new(),
        attributes: None,
    };

    sign_tx(&mut tx, &key, chain_id).map_err(|error| anyhow!("Lighter 签名失败: {error}"))?;

    Ok(LighterSigned {
        tx_type: TX_TYPE_L2_CREATE_ORDER,
        tx_info: tx
            .to_json()
            .map_err(|error| anyhow!("Lighter 订单序列化失败: {error}"))?,
    })
}

/// Lighter 杠杆设置参数，`initial_margin_fraction` 以 1e-4 为单位。
pub struct LighterLeverage {
    pub account_index: i64,
    pub api_key_index: u8,
    pub market_index: i16,
    pub initial_margin_fraction: u16,
    pub nonce: i64,
    pub expired_at: i64,
}

/// 签名一笔 `L2UpdateLeverage` 交易，按全仓模式提交。
pub fn sign_lighter_leverage(
    leverage: &LighterLeverage,
    private_key: &str,
    chain_id: u32,
) -> anyhow::Result<LighterSigned> {
    let key = KeyManager::from_hex(private_key)
        .map_err(|error| anyhow!("Lighter 私钥不合法: {error}"))?;
    let mut tx = UpdateLeverage {
        account_index: leverage.account_index,
        api_key_index: leverage.api_key_index,
        market_index: leverage.market_index,
        initial_margin_fraction: leverage.initial_margin_fraction,
        margin_mode: CROSS_MARGIN,
        expired_at: leverage.expired_at,
        nonce: leverage.nonce,
        sig: Vec::new(),
        signed_hash: String::new(),
        attributes: None,
    };
    sign_tx(&mut tx, &key, chain_id).map_err(|error| anyhow!("Lighter 签名失败: {error}"))?;
    Ok(LighterSigned {
        tx_type: tx.tx_type(),
        tx_info: tx
            .to_json()
            .map_err(|error| anyhow!("Lighter 杠杆交易序列化失败: {error}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn encodes_hyperliquid_order_action() {
        let order = HyperliquidOrder {
            coin: "BTC",
            asset: 3,
            size_decimals: 5,
            is_buy: true,
            price: dec!(78000.5),
            size: dec!(0.01),
            reduce_only: false,
            cloid: None,
        };
        let action = order.action().expect("订单应可序列化");
        // {"type":"order","orders":[{"a":3,"b":true,"p":"78001","s":"0.01",...}],"grouping":"na"}
        // 78000.5 有 6 位有效数字，按交易所规则四舍五入到 5 位。
        assert_eq!(action[0], 0x83);
        assert_eq!(action[1], 0xa4);
        assert_eq!(&action[2..6], b"type");
        assert_eq!(action[6], 0xa5);
        assert_eq!(&action[7..12], b"order");
        assert!(action.windows(5).any(|window| window == b"78001"));
        assert!(action.windows(4).any(|window| window == b"0.01"));
    }

    #[test]
    fn rounds_hyperliquid_price_to_five_significant_figures() {
        let order = HyperliquidOrder {
            coin: "BTC",
            asset: 0,
            size_decimals: 5,
            is_buy: false,
            price: dec!(78123.456789),
            size: dec!(0.001),
            reduce_only: true,
            cloid: None,
        };
        assert_eq!(order.wire_price().unwrap(), "78123");
        assert_eq!(order.wire_size().unwrap(), "0.001");
    }

    #[test]
    fn rounds_small_price_by_significant_figures() {
        let order = HyperliquidOrder {
            coin: "kPEPE",
            asset: 42,
            size_decimals: 0,
            is_buy: true,
            price: dec!(0.00001234567),
            size: dec!(1000),
            reduce_only: false,
            cloid: None,
        };
        // 6 位小数上限优先于 5 位有效数字。
        assert_eq!(order.wire_price().unwrap(), "0.000012");
    }
}
