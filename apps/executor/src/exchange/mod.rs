pub mod aster;
pub mod binance;
pub mod bingx;
pub mod bitget;
pub mod bybit;
pub mod dex;
pub mod gate;
pub mod htx;
pub mod hyperliquid;
pub mod lighter;
pub mod live;
pub mod mexc;
pub mod okx;

use anyhow::{anyhow, bail};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

pub use binance::{BinanceFundingClient, BinanceMarketClient, VENUE};
pub use bingx::{BingxFundingClient, VENUE as BINGX_VENUE};
pub use bitget::{BitgetFundingClient, VENUE as BITGET_VENUE};
pub use bybit::{BybitFundingClient, VENUE as BYBIT_VENUE};
pub use gate::{GateFundingClient, VENUE as GATE_VENUE};
pub use htx::{HtxFundingClient, VENUE as HTX_VENUE};
pub use hyperliquid::{HyperliquidFundingClient, VENUE as HYPERLIQUID_VENUE};
pub use lighter::{LighterFundingClient, VENUE as LIGHTER_VENUE};
pub use live::{LiveEndpoints, LiveExchange};
pub use mexc::{MexcFundingClient, VENUE as MEXC_VENUE};
pub use okx::{ContractSpec, OkxFundingClient, OkxMarketClient, VENUE as OKX_VENUE};

/// Aster 的合约接口与 Binance 完全兼容，只换域名与交易所标识。
pub const ASTER_VENUE: &str = "aster";

/// 现货 / USDT 本位永续 / 交割合约三类行情。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Spot,
    Swap,
    /// 有固定到期日的交割合约（季度合约）。
    Delivery,
}

/// 一个在交易的交割合约，用于「现货多 + 交割空」的基差机会。
#[derive(Debug, Clone)]
pub struct DeliveryContract {
    /// 交易所标识，与永续 venue 相同（`binance` / `okx` / `bybit`）。
    pub venue: String,
    /// 内部交易对，如 `BTCUSDT`。
    pub symbol: String,
    /// 交易所合约名，如 `BTCUSDT_251226` / `BTC-USDT-251226` / `BTCUSDT-26DEC25`。
    pub contract: String,
    /// 到期时间。
    pub expiry: DateTime<Utc>,
}

/// 已结算的一期资金费率，`venue` 区分 Binance / OKX。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRate {
    pub venue: String,
    pub symbol: String,
    pub funding_time: DateTime<Utc>,
    pub funding_rate: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_price: Option<Decimal>,
}

impl FundingRate {
    /// 数据库唯一键：同一交易所、同一交易对、同一结算时间。
    pub(crate) fn same_key(&self, other: &Self) -> bool {
        self.venue == other.venue
            && self.symbol == other.symbol
            && self.funding_time == other.funding_time
    }
}

/// 当期未结算的费率快照，仅用于展示与判读。
#[derive(Debug, Clone)]
pub struct FundingSnapshot {
    pub funding_rate: Decimal,
    pub mark_price: Option<Decimal>,
    pub next_funding_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

/// 某交易所某交易对的买一/卖一档快照与对应名义价值。
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub bid: Decimal,
    pub ask: Decimal,
    pub bid_notional_usd: Decimal,
    pub ask_notional_usd: Decimal,
}

/// 盘口单档：价格与数量，数量统一为币本位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthLevel {
    pub price: Decimal,
    pub quantity: Decimal,
}

/// 多档盘口快照，供桌面端展示盘口深度。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthSnapshot {
    pub venue: String,
    pub symbol: String,
    /// 买盘，价格由高到低。
    pub bids: Vec<DepthLevel>,
    /// 卖盘，价格由低到高。
    pub asks: Vec<DepthLevel>,
    pub observed_at: DateTime<Utc>,
}

/// 交易所对单个交易对的最小下单限制。
///
/// 多数交易所同时限制最小数量与最小名义额，二者取较大值才是实际门槛；
/// 字段缺失表示交易所未公开该限制，由下单接口做最终校验。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinOrderSpec {
    pub venue: String,
    pub symbol: String,
    /// 最小下单数量（基础币）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_quantity: Option<Decimal>,
    /// 最小下单名义额（USDT）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_notional_usd: Option<Decimal>,
}

impl MinOrderSpec {
    pub fn new(venue: &str, symbol: &str) -> Self {
        Self {
            venue: venue.to_string(),
            symbol: symbol.trim().to_ascii_uppercase(),
            min_quantity: None,
            min_notional_usd: None,
        }
    }
}

/// 一根 K 线，两个交易所统一为同一结构。
#[derive(Debug, Clone, Serialize)]
pub struct Candle {
    pub open_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// 资金费率交易所适配器：内部 symbol（如 `BTCUSDT`）由各实现自行映射为交易所合约名。
pub trait FundingVenueClient: Send + Sync {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot>;
    fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>>;

    /// 该交易所当前可交易的 USDT 本位永续清单；不支持自动发现时返回空。
    fn listed_symbols(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// 一次请求取回全部合约的当期费率；没有批量接口时返回空，由调用方逐币兜底。
    fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        Ok(Vec::new())
    }

    /// 该交易所固定的结算间隔；返回 `None` 时由历史结算时间推断。
    fn funding_interval_secs(&self) -> Option<i64> {
        None
    }

    /// 交易所公开的逐合约结算间隔（秒）；不支持时返回空，由历史结算时间推断。
    fn funding_intervals(&self) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        Ok(std::collections::HashMap::new())
    }
}

pub(crate) fn parse_decimal(value: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(value.trim()).map_err(|_| anyhow!("不是合法数字: {value}"))
}

pub(crate) fn parse_json_decimal(value: &Value) -> anyhow::Result<Decimal> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    parse_decimal(&text)
}

pub(crate) fn value_to_millis(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

pub(crate) fn millis_to_time(millis: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(millis).single()
}

pub(crate) fn make_book(
    bid: Decimal,
    bid_qty: Decimal,
    ask: Decimal,
    ask_qty: Decimal,
) -> anyhow::Result<BookSnapshot> {
    if bid <= Decimal::ZERO || ask <= Decimal::ZERO || ask < bid {
        bail!("盘口价格异常: bid={bid} ask={ask}");
    }
    Ok(BookSnapshot {
        bid,
        ask,
        bid_notional_usd: bid * bid_qty,
        ask_notional_usd: ask * ask_qty,
    })
}

/// 解析 `[[价格, 数量], ...]` 形式的盘口数组。
pub(crate) fn depth_rows(value: &Value, key: &str) -> anyhow::Result<Vec<(Decimal, Decimal)>> {
    let rows = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("盘口缺少 {key}"))?;
    let mut levels = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| anyhow!("盘口 {key} 档位格式不合法: {row:?}"))?;
        if row.len() < 2 {
            bail!("盘口 {key} 档位字段不足: {row:?}");
        }
        levels.push((parse_json_decimal(&row[0])?, parse_json_decimal(&row[1])?));
    }
    Ok(levels)
}

/// 组装多档盘口：数量按 `multiplier` 折算成币本位，并校验价格与数量合法。
pub(crate) fn make_depth(
    venue: &str,
    symbol: &str,
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
    multiplier: Decimal,
) -> anyhow::Result<DepthSnapshot> {
    if bids.is_empty() || asks.is_empty() {
        bail!("盘口档位为空: {venue} {symbol}");
    }
    let levels = |rows: Vec<(Decimal, Decimal)>| -> anyhow::Result<Vec<DepthLevel>> {
        rows.into_iter()
            .map(|(price, quantity)| {
                if price <= Decimal::ZERO || quantity < Decimal::ZERO {
                    bail!("盘口价格或数量异常: price={price} quantity={quantity}");
                }
                Ok(DepthLevel {
                    price,
                    quantity: quantity * multiplier,
                })
            })
            .collect()
    };
    Ok(DepthSnapshot {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        bids: levels(bids)?,
        asks: levels(asks)?,
        observed_at: Utc::now(),
    })
}

/// 按 `[时间, 开, 高, 低, 收, 量, ...]` 解析两个交易所的 K 线行。
pub(crate) fn candles_from_rows(
    rows: Vec<Vec<Value>>,
    newest_first: bool,
) -> anyhow::Result<Vec<Candle>> {
    let mut candles = Vec::with_capacity(rows.len());
    for row in rows {
        if row.len() < 6 {
            bail!("K 线字段不足: {row:?}");
        }
        let millis =
            value_to_millis(&row[0]).ok_or_else(|| anyhow!("K 线时间不合法: {}", row[0]))?;
        candles.push(Candle {
            open_time: millis_to_time(millis).ok_or_else(|| anyhow!("K 线时间不合法: {millis}"))?,
            open: parse_json_decimal(&row[1])?,
            high: parse_json_decimal(&row[2])?,
            low: parse_json_decimal(&row[3])?,
            close: parse_json_decimal(&row[4])?,
            volume: parse_json_decimal(&row[5])?,
        });
    }
    if newest_first {
        candles.reverse();
    }
    Ok(candles)
}
