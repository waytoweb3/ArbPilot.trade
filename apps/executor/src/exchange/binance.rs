use std::{collections::HashMap, str::FromStr, sync::Arc};

use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    candles_from_rows, depth_rows, make_book, make_depth, BookSnapshot, Candle, DeliveryContract,
    DepthSnapshot, FundingRate, FundingSnapshot, FundingVenueClient, MinOrderSpec, Segment,
};
use crate::http_client::HttpClient;

/// Binance 永续合约的交易所标识。
pub const VENUE: &str = "binance";

const HISTORY_LIMIT: u32 = 1_000;
const KLINE_LIMIT: u32 = 1_000;

/// Binance USDT 本位合约公开行情客户端（只读，无需 API Key）。
///
/// Aster 的合约接口与 Binance 完全兼容，用 [`BinanceFundingClient::with_venue`]
/// 换个 base_url 与交易所标识即可复用。
pub struct BinanceFundingClient {
    venue: &'static str,
    base_url: String,
    http: Arc<HttpClient>,
}

impl BinanceFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self::with_venue(VENUE, base_url, http)
    }

    pub fn with_venue(venue: &'static str, base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            venue,
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// 当期费率、标记价格与下次结算时间。
    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let url = format!("{}/fapi/v1/premiumIndex?symbol={symbol}", self.base_url);
        let value = self.http.get_json(&url)?;
        let payload: PremiumIndex =
            serde_json::from_value(value).context("解析 premiumIndex 失败")?;
        snapshot_from(&payload).ok_or_else(|| anyhow!("premiumIndex 缺少合法的 lastFundingRate"))
    }

    /// 一次请求返回全部合约（USDT/USDC 本位）的当期费率。
    ///
    /// Binance 的 `premiumIndex` 不带 `symbol` 时返回数组，合约数量再多也只是一次请求。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let url = format!("{}/fapi/v1/premiumIndex", self.base_url);
        let rows: Vec<PremiumIndex> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 premiumIndex 列表失败")?;

        let mut snapshots = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(symbol) = row.symbol.as_deref() else {
                continue;
            };
            let Some(snapshot) = snapshot_from(&row) else {
                continue;
            };
            snapshots.push((symbol.to_ascii_uppercase(), snapshot));
        }
        Ok(snapshots)
    }

    /// 各合约的官方结算间隔（秒）；`fundingInfo` 是 Binance 唯一公开该字段的接口。
    ///
    /// Aster 的接口与 Binance 兼容，同一份实现可以直接复用。
    pub fn funding_intervals(&self) -> anyhow::Result<HashMap<String, i64>> {
        let url = format!("{}/fapi/v1/fundingInfo", self.base_url);
        let rows: Vec<FundingInfoRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 fundingInfo 失败")?;

        let mut intervals = HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(hours) = row.funding_interval_hours.filter(|value| (1..=24).contains(value))
            else {
                continue;
            };
            intervals.insert(row.symbol.to_ascii_uppercase(), hours * 3600);
        }
        Ok(intervals)
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/fapi/v1/exchangeInfo", self.base_url);
        let payload: ExchangeInfo =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 exchangeInfo 失败")?;

        let mut symbols: Vec<String> = payload
            .symbols
            .into_iter()
            .filter(|item| item.status.eq_ignore_ascii_case("TRADING"))
            .filter(|item| item.contract_type.eq_ignore_ascii_case("PERPETUAL"))
            .filter(|item| item.quote_asset.eq_ignore_ascii_case("USDT"))
            .map(|item| item.symbol.to_ascii_uppercase())
            .collect();
        symbols.sort();
        symbols.dedup();
        Ok(symbols)
    }

    /// 历史已结算费率，按结算时间升序返回；`start` 为已入库的最后一条。
    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let mut url = format!(
            "{}/fapi/v1/fundingRate?symbol={symbol}&limit={HISTORY_LIMIT}",
            self.base_url
        );
        if let Some(start) = start {
            // Binance 的 startTime 为闭区间，加 1 毫秒避免重复拉取已入库记录。
            url.push_str(&format!("&startTime={}", start.timestamp_millis() + 1));
        }

        let value = self.http.get_json(&url)?;
        let rows: Vec<BinanceFundingRow> =
            serde_json::from_value(value).context("解析 fundingRate 失败")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let funding_time = millis_to_time(row.funding_time)
                .ok_or_else(|| anyhow!("fundingTime 不合法: {}", row.funding_time))?;
            let funding_rate = parse_decimal(&row.funding_rate)
                .ok_or_else(|| anyhow!("fundingRate 不是合法数字: {}", row.funding_rate))?;
            rates.push(FundingRate {
                venue: self.venue.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate,
                mark_price: row.mark_price.as_deref().and_then(parse_decimal),
            });
        }
        Ok(rates)
    }
}

impl FundingVenueClient for BinanceFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        Self::premium_index(self, symbol)
    }

    fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        Self::funding_history(self, symbol, start)
    }

    fn listed_symbols(&self) -> anyhow::Result<Vec<String>> {
        self.perpetual_symbols()
    }

    fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        Self::premium_index_all(self)
    }

    fn funding_intervals(&self) -> anyhow::Result<HashMap<String, i64>> {
        Self::funding_intervals(self)
    }
}

/// Binance 现货与 USDT 本位合约的盘口 / K 线客户端。
pub struct BinanceMarketClient {
    spot_base_url: String,
    futures_base_url: String,
    http: Arc<HttpClient>,
}

impl BinanceMarketClient {
    pub fn new(spot_base_url: &str, futures_base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            spot_base_url: spot_base_url.trim_end_matches('/').to_string(),
            futures_base_url: futures_base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    pub fn book(&self, symbol: &str, segment: Segment) -> anyhow::Result<BookSnapshot> {
        let url = match segment {
            Segment::Spot => format!(
                "{}/api/v3/ticker/bookTicker?symbol={symbol}",
                self.spot_base_url
            ),
            Segment::Swap | Segment::Delivery => format!(
                "{}/fapi/v1/ticker/bookTicker?symbol={symbol}",
                self.futures_base_url
            ),
        };
        let payload: BookTicker =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Binance 盘口失败")?;
        make_book(
            required_decimal(&payload.bid_price, "bidPrice")?,
            required_decimal(&payload.bid_qty, "bidQty")?,
            required_decimal(&payload.ask_price, "askPrice")?,
            required_decimal(&payload.ask_qty, "askQty")?,
        )
    }

    /// 多档盘口；现货与合约分别走各自的 depth 端点，数量单位是币。
    pub fn depth(
        &self,
        symbol: &str,
        segment: Segment,
        limit: u32,
    ) -> anyhow::Result<DepthSnapshot> {
        let (base, path) = match segment {
            Segment::Spot => (&self.spot_base_url, "/api/v3/depth"),
            Segment::Swap | Segment::Delivery => (&self.futures_base_url, "/fapi/v1/depth"),
        };
        // Binance 只接受固定的档位集合，向上取整到最近的合法值。
        let limit = match limit {
            0..=5 => 5,
            6..=10 => 10,
            11..=20 => 20,
            21..=50 => 50,
            51..=100 => 100,
            101..=500 => 500,
            _ => 1_000,
        };
        let url = format!("{base}{path}?symbol={symbol}&limit={limit}");
        let value = self.http.get_json(&url)?;
        make_depth(
            VENUE,
            symbol,
            depth_rows(&value, "bids")?,
            depth_rows(&value, "asks")?,
            Decimal::ONE,
        )
    }

    pub fn candles(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
        segment: Segment,
    ) -> anyhow::Result<Vec<Candle>> {
        let (base, path) = match segment {
            Segment::Spot => (&self.spot_base_url, "/api/v3/klines"),
            Segment::Swap | Segment::Delivery => (&self.futures_base_url, "/fapi/v1/klines"),
        };
        let url = format!(
            "{base}{path}?symbol={symbol}&interval={interval}&limit={}",
            limit.clamp(1, KLINE_LIMIT)
        );
        let rows: Vec<Vec<Value>> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Binance K 线失败")?;
        candles_from_rows(rows, false)
    }

    /// 当前在交易的 USDT 本位季度交割合约；每个交易对只保留最近到期的一张。
    pub fn delivery_contracts(&self, symbols: &[String]) -> anyhow::Result<Vec<DeliveryContract>> {
        let url = format!("{}/fapi/v1/exchangeInfo", self.futures_base_url);
        let payload: ExchangeInfo =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 exchangeInfo 失败")?;
        let now = Utc::now();

        let mut nearest: HashMap<String, DeliveryContract> = HashMap::new();
        for item in payload.symbols {
            if !item.status.eq_ignore_ascii_case("TRADING") {
                continue;
            }
            // 季度交割合约的 contractType 为 CURRENT_QUARTER / NEXT_QUARTER。
            if !item.contract_type.to_ascii_uppercase().contains("QUARTER") {
                continue;
            }
            let Some(expiry) = item
                .delivery_date
                .filter(|millis| *millis > 0)
                .and_then(millis_to_time)
            else {
                continue;
            };
            if expiry <= now {
                continue;
            }
            let underlying =
                format!("{}{}", item.base_asset, item.quote_asset).to_ascii_uppercase();
            if !symbols.is_empty()
                && !symbols
                    .iter()
                    .any(|symbol| symbol.eq_ignore_ascii_case(&underlying))
            {
                continue;
            }
            let contract = DeliveryContract {
                venue: VENUE.to_string(),
                symbol: underlying.clone(),
                contract: item.symbol.to_ascii_uppercase(),
                expiry,
            };
            match nearest.get(&underlying) {
                Some(existing) if existing.expiry <= expiry => {}
                _ => {
                    nearest.insert(underlying, contract);
                }
            }
        }

        let mut contracts: Vec<DeliveryContract> = nearest.into_values().collect();
        contracts.sort_by_key(|contract| contract.expiry);
        Ok(contracts)
    }

    /// 单个交易对的最小下单数量与最小名义额；交易所未公开时对应字段为 `None`。
    pub fn min_order(
        &self,
        venue: &str,
        symbol: &str,
        segment: Segment,
    ) -> anyhow::Result<Option<MinOrderSpec>> {
        let symbol = symbol.trim().to_ascii_uppercase();
        let (base_url, path) = match segment {
            Segment::Spot => (&self.spot_base_url, "/api/v3/exchangeInfo"),
            Segment::Swap | Segment::Delivery => (&self.futures_base_url, "/fapi/v1/exchangeInfo"),
        };
        let url = format!("{base_url}{path}?symbol={symbol}");
        let payload: ExchangeInfo = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 exchangeInfo 失败")?;
        let Some(row) = payload
            .symbols
            .into_iter()
            .find(|row| row.symbol.eq_ignore_ascii_case(&symbol))
        else {
            return Ok(None);
        };

        let mut spec = MinOrderSpec::new(venue, &symbol);
        for filter in &row.filters {
            match filter.filter_type.as_str() {
                "LOT_SIZE" => {
                    spec.min_quantity = filter.min_qty.as_deref().and_then(parse_decimal);
                }
                // 合约用 notional，现货用 minNotional；两者都存在时以较大值为准。
                "MIN_NOTIONAL" | "NOTIONAL" => {
                    let notional = filter
                        .notional
                        .as_deref()
                        .or(filter.min_notional.as_deref())
                        .and_then(parse_decimal);
                    if let Some(notional) = notional {
                        spec.min_notional_usd = Some(
                            spec.min_notional_usd
                                .map(|current| current.max(notional))
                                .unwrap_or(notional),
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(Some(spec))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FundingInfoRow {
    symbol: String,
    /// 官方结算间隔（小时）；缺省表示沿用 8 小时。
    #[serde(default)]
    funding_interval_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PremiumIndex {
    /// 批量接口（不带 symbol）返回的合约名；单币接口没有该字段。
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    #[serde(default)]
    last_funding_rate: Option<String>,
    #[serde(default)]
    next_funding_time: Option<i64>,
    #[serde(default)]
    time: Option<i64>,
}

/// 把 `premiumIndex` 的一行转换为内部快照；缺少合法费率时返回 `None`。
fn snapshot_from(payload: &PremiumIndex) -> Option<FundingSnapshot> {
    let raw_rate = payload.last_funding_rate.as_deref()?;
    Some(FundingSnapshot {
        funding_rate: parse_decimal(raw_rate)?,
        mark_price: payload.mark_price.as_deref().and_then(parse_decimal),
        next_funding_at: payload.next_funding_time.and_then(millis_to_time),
        observed_at: payload
            .time
            .and_then(millis_to_time)
            .unwrap_or_else(Utc::now),
    })
}

#[derive(Debug, Deserialize)]
struct ExchangeInfo {
    #[serde(default)]
    symbols: Vec<ExchangeSymbol>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeSymbol {
    symbol: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    contract_type: String,
    #[serde(default)]
    base_asset: String,
    #[serde(default)]
    quote_asset: String,
    /// 交割合约的到期时间（毫秒）；永续为 0。
    #[serde(default)]
    delivery_date: Option<i64>,
    /// 下单限制：LOT_SIZE / MIN_NOTIONAL 等。
    #[serde(default)]
    filters: Vec<ExchangeFilter>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeFilter {
    #[serde(default)]
    filter_type: String,
    #[serde(default)]
    min_qty: Option<String>,
    #[serde(default)]
    min_notional: Option<String>,
    #[serde(default)]
    notional: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BinanceFundingRow {
    #[serde(rename = "fundingTime")]
    funding_time: i64,
    #[serde(rename = "fundingRate")]
    funding_rate: String,
    #[serde(rename = "markPrice", default)]
    mark_price: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BookTicker {
    #[serde(rename = "bidPrice")]
    bid_price: String,
    #[serde(rename = "bidQty")]
    bid_qty: String,
    #[serde(rename = "askPrice")]
    ask_price: String,
    #[serde(rename = "askQty")]
    ask_qty: String,
}

fn parse_decimal(value: &str) -> Option<Decimal> {
    Decimal::from_str(value.trim()).ok()
}

fn required_decimal(value: &str, field: &str) -> anyhow::Result<Decimal> {
    parse_decimal(value).ok_or_else(|| anyhow!("{field} 不是合法数字: {value}"))
}

fn millis_to_time(millis: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(millis).single()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_funding_history_rows() {
        let rows: Vec<BinanceFundingRow> = serde_json::from_str(
            r#"[
                {"symbol":"BTCUSDT","fundingTime":1700006400000,"fundingRate":"0.00010000","markPrice":"35000.5"},
                {"symbol":"BTCUSDT","fundingTime":1700035200000,"fundingRate":"-0.00020000"}
            ]"#,
        )
        .expect("fixture 应可解析");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].funding_time, 1_700_006_400_000);
        assert_eq!(parse_decimal(&rows[0].funding_rate), Some(dec!(0.0001)));
        assert_eq!(parse_decimal(&rows[1].funding_rate), Some(dec!(-0.0002)));
        assert_eq!(
            rows[0].mark_price.as_deref().and_then(parse_decimal),
            Some(dec!(35000.5))
        );
    }

    #[test]
    fn parses_premium_index_payload() {
        let payload: PremiumIndex = serde_json::from_str(
            r#"{"symbol":"BTCUSDT","markPrice":"35000","lastFundingRate":"-0.0002","nextFundingTime":1700035200000,"time":1700031600000}"#,
        )
        .expect("fixture 应可解析");

        assert_eq!(payload.mark_price.as_deref(), Some("35000"));
        assert_eq!(payload.last_funding_rate.as_deref(), Some("-0.0002"));
        assert_eq!(payload.next_funding_time, Some(1_700_035_200_000));
    }

    #[test]
    fn parses_book_ticker_payload() {
        let payload: BookTicker = serde_json::from_str(
            r#"{"symbol":"BTCUSDT","bidPrice":"78000.10","bidQty":"2.5","askPrice":"78001.20","askQty":"1.5"}"#,
        )
        .expect("fixture 应可解析");

        assert_eq!(parse_decimal(&payload.bid_price), Some(dec!(78000.10)));
        assert_eq!(parse_decimal(&payload.ask_qty), Some(dec!(1.5)));
    }

    #[test]
    fn converts_millis_to_utc() {
        assert_eq!(
            millis_to_time(1_700_006_400_000).map(|time| time.timestamp()),
            Some(1_700_006_400)
        );
    }
}
