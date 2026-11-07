use std::sync::Arc;

use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    make_book, parse_decimal, parse_json_decimal, BookSnapshot, FundingRate, FundingSnapshot,
    FundingVenueClient,
};
use crate::http_client::HttpClient;

/// Hyperliquid 的交易所标识。
pub const VENUE: &str = "hyperliquid";

/// Hyperliquid 的 `fundingHistory` 单次最多返回 500 条（每小时一条）。
const HISTORY_LIMIT: u32 = 500;

/// Hyperliquid 永续按小时结算，取下个小时整点作为下次结算时间。
fn next_hour(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt((now.timestamp() / 3600 + 1) * 3600, 0)
        .single()
}

/// Hyperliquid 的币名（如 `BTC`、`kPEPE`）补齐内部统一后缀；币名大小写敏感，保持原样。
fn internal_symbol(coin: &str) -> String {
    let upper = coin.trim().to_ascii_uppercase();
    if upper.ends_with("USDT") || upper.ends_with("USDC") {
        coin.trim().to_string()
    } else {
        format!("{}USDT", coin.trim())
    }
}

/// 内部 `BTCUSDT` → Hyperliquid 的 `BTC`。
fn venue_coin(symbol: &str) -> anyhow::Result<&str> {
    let upper = symbol.to_ascii_uppercase();
    for suffix in ["USDT", "USDC"] {
        if upper.ends_with(suffix) && symbol.len() > suffix.len() {
            return Ok(&symbol[..symbol.len() - suffix.len()]);
        }
    }
    Err(anyhow!("Hyperliquid 无法映射交易对: {symbol}"))
}

/// 从 `l2Book` 解析买一/卖一；`levels[0]` 为买盘、`levels[1]` 为卖盘。
fn book_from_l2(value: &Value) -> anyhow::Result<BookSnapshot> {
    let levels = value
        .get("levels")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Hyperliquid l2Book 响应缺少 levels"))?;
    let level = |index: usize, label: &str| -> anyhow::Result<(Decimal, Decimal)> {
        let row = levels
            .get(index)
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .ok_or_else(|| anyhow!("Hyperliquid l2Book 缺少{label}"))?;
        let price = row
            .get("px")
            .ok_or_else(|| anyhow!("Hyperliquid l2Book {label}缺少 px"))?;
        let size = row
            .get("sz")
            .ok_or_else(|| anyhow!("Hyperliquid l2Book {label}缺少 sz"))?;
        Ok((parse_json_decimal(price)?, parse_json_decimal(size)?))
    };
    let (bid, bid_size) = level(0, "买一")?;
    let (ask, ask_size) = level(1, "卖一")?;
    make_book(bid, bid_size, ask, ask_size)
}

#[derive(Debug, Deserialize)]
struct UniverseEntry {
    name: String,
    #[serde(rename = "isDelisted", default)]
    is_delisted: bool,
}

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default)]
    universe: Vec<UniverseEntry>,
}

#[derive(Debug, Deserialize)]
struct AssetCtx {
    #[serde(default)]
    funding: Option<String>,
    #[serde(rename = "markPx", default)]
    mark_px: Option<String>,
    #[serde(rename = "oraclePx", default)]
    oracle_px: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    #[serde(rename = "fundingRate")]
    funding_rate: String,
    /// 结算时间（毫秒）。
    time: i64,
}

/// Hyperliquid 永续公开资金费率客户端。
pub struct HyperliquidFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl HyperliquidFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    fn info(&self, payload: Value) -> anyhow::Result<Value> {
        self.http
            .post_json(&format!("{}/info", self.base_url), &payload)
    }

    /// 永续的买一/卖一档；`l2Book` 的数量单位是币。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let coin = venue_coin(symbol)?;
        let value = self.info(json!({ "type": "l2Book", "coin": coin }))?;
        book_from_l2(&value)
    }

    /// 全部永续的当期费率、标记价与下次结算时间，一次 `metaAndAssetCtxs` 请求即可。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let value = self.info(json!({ "type": "metaAndAssetCtxs" }))?;
        let mut items = value
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow!("metaAndAssetCtxs 响应不是数组"))?
            .into_iter();

        let meta: Meta = serde_json::from_value(
            items
                .next()
                .ok_or_else(|| anyhow!("metaAndAssetCtxs 缺少 meta"))?,
        )
        .context("解析 Hyperliquid meta 失败")?;
        let contexts: Vec<AssetCtx> = serde_json::from_value(
            items
                .next()
                .ok_or_else(|| anyhow!("metaAndAssetCtxs 缺少 assetCtxs"))?,
        )
        .context("解析 Hyperliquid assetCtxs 失败")?;

        let now = Utc::now();
        let next_funding_at = next_hour(now);
        let mut snapshots = Vec::with_capacity(contexts.len());
        for (entry, ctx) in meta.universe.into_iter().zip(contexts) {
            if entry.is_delisted {
                continue;
            }
            let Some(raw_rate) = ctx.funding.as_deref() else {
                continue;
            };
            let Ok(funding_rate) = parse_decimal(raw_rate) else {
                continue;
            };
            snapshots.push((
                internal_symbol(&entry.name),
                FundingSnapshot {
                    funding_rate,
                    mark_price: ctx
                        .mark_px
                        .as_deref()
                        .and_then(|value| parse_decimal(value).ok())
                        .or_else(|| {
                            ctx.oracle_px
                                .as_deref()
                                .and_then(|value| parse_decimal(value).ok())
                        }),
                    next_funding_at,
                    observed_at: now,
                },
            ));
        }
        Ok(snapshots)
    }

    /// 当前可交易的永续币种清单。
    pub fn listed_symbols(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .premium_index_all()?
            .into_iter()
            .map(|(symbol, _)| symbol)
            .collect())
    }

    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let coin = venue_coin(symbol)?;
        let now = Utc::now();
        let start_time = start
            .map(|time| time.timestamp_millis() + 1)
            .unwrap_or_else(|| (now - chrono::Duration::days(30)).timestamp_millis());

        let value = self.info(json!({
            "type": "fundingHistory",
            "coin": coin,
            "startTime": start_time,
            "endTime": now.timestamp_millis(),
        }))?;
        let rows: Vec<HistoryRow> =
            serde_json::from_value(value).context("解析 Hyperliquid fundingHistory 失败")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows.into_iter().take(HISTORY_LIMIT as usize) {
            let Some(funding_time) = Utc.timestamp_millis_opt(row.time).single() else {
                continue;
            };
            let Ok(funding_rate) = parse_decimal(&row.funding_rate) else {
                continue;
            };
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate,
                mark_price: None,
            });
        }
        Ok(rates)
    }
}

impl FundingVenueClient for HyperliquidFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let mut rows = self.premium_index_all()?;
        rows.iter()
            .position(|(item, _)| item == symbol)
            .map(|index| rows.swap_remove(index).1)
            .ok_or_else(|| anyhow!("Hyperliquid 未找到交易对: {symbol}"))
    }

    fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        Self::funding_history(self, symbol, start)
    }

    fn listed_symbols(&self) -> anyhow::Result<Vec<String>> {
        Self::listed_symbols(self)
    }

    /// Hyperliquid 的永续合约每小时结算一次。
    fn funding_interval_secs(&self) -> Option<i64> {
        Some(3600)
    }

    fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        Self::premium_index_all(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn maps_coin_names() {
        assert_eq!(internal_symbol("BTC"), "BTCUSDT");
        assert_eq!(internal_symbol("kPEPE"), "kPEPEUSDT");
        assert_eq!(internal_symbol("ETHUSDC"), "ETHUSDC");
        assert_eq!(venue_coin("BTCUSDT").unwrap(), "BTC");
        assert_eq!(venue_coin("kPEPEUSDT").unwrap(), "kPEPE");
        assert!(venue_coin("BTC").is_err());
    }

    #[test]
    fn parses_meta_and_contexts() {
        let value = json!([
            { "universe": [{ "name": "BTC" }, { "name": "OLD", "isDelisted": true }] },
            [
                { "funding": "0.0000125", "markPx": "78415.0", "oraclePx": "78437.5" },
                { "funding": "0.0001", "markPx": "1.0" }
            ]
        ]);
        let mut items = value.as_array().cloned().unwrap().into_iter();
        let meta: Meta = serde_json::from_value(items.next().unwrap()).unwrap();
        let ctxs: Vec<AssetCtx> = serde_json::from_value(items.next().unwrap()).unwrap();
        assert_eq!(meta.universe.len(), 2);
        assert_eq!(meta.universe[1].is_delisted, true);
        assert_eq!(
            ctxs[0]
                .funding
                .as_deref()
                .and_then(|v| parse_decimal(v).ok()),
            Some(dec!(0.0000125))
        );
    }

    #[test]
    fn rounds_up_to_next_hour() {
        let now = Utc.timestamp_opt(1_700_000_123, 0).unwrap();
        let next = next_hour(now).unwrap();
        assert_eq!(next.timestamp(), 1_700_002_800);
    }

    #[test]
    fn builds_book_from_l2() {
        let value = json!({
            "coin": "BTC",
            "levels": [
                [{ "px": "75722.0", "sz": "1.76879" }],
                [{ "px": "75723.0", "sz": "6.47643" }]
            ]
        });
        let book = book_from_l2(&value).unwrap();
        assert_eq!(book.bid, dec!(75722.0));
        assert_eq!(book.ask, dec!(75723.0));
        assert_eq!(book.bid_notional_usd, dec!(133_936.31638));
        assert_eq!(book.ask_notional_usd, dec!(490_414.70889));
    }
}
