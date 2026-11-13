use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    candles_from_rows, depth_rows, make_book, make_depth, millis_to_time, parse_decimal,
    BookSnapshot, Candle, DeliveryContract, DepthSnapshot, FundingRate, FundingSnapshot,
    FundingVenueClient, MinOrderSpec,
};
use crate::http_client::HttpClient;

/// Bybit 的交易所标识。
pub const VENUE: &str = "bybit";

/// Bybit 单次最多返回 200 条历史费率。
const HISTORY_LIMIT: u32 = 200;
/// `instruments-info` 每页最多 1000 条。
const PAGE_LIMIT: u32 = 1_000;
/// 防御性上限，避免游标异常时无限翻页。
const MAX_PAGES: usize = 5;

/// Bybit V5 统一的响应信封：`retCode` 为 0 才表示成功。
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Envelope<T> {
    #[serde(rename = "retCode")]
    ret_code: i64,
    #[serde(rename = "retMsg", default)]
    ret_msg: String,
    #[serde(default)]
    result: Option<T>,
}

fn into_result<T>(envelope: Envelope<T>, endpoint: &str) -> anyhow::Result<T> {
    if envelope.ret_code != 0 {
        bail!(
            "Bybit {endpoint} 返回错误 {}: {}",
            envelope.ret_code,
            envelope.ret_msg
        );
    }
    envelope
        .result
        .ok_or_else(|| anyhow!("Bybit {endpoint} 响应缺少 result"))
}

#[derive(Debug, Deserialize)]
struct TickerRow {
    symbol: String,
    #[serde(rename = "fundingRate", default)]
    funding_rate: Option<String>,
    #[serde(rename = "markPrice", default)]
    mark_price: Option<String>,
    #[serde(rename = "nextFundingTime", default)]
    next_funding_time: Option<String>,
    #[serde(rename = "bid1Price", default)]
    bid_price: Option<String>,
    #[serde(rename = "bid1Size", default)]
    bid_size: Option<String>,
    #[serde(rename = "ask1Price", default)]
    ask_price: Option<String>,
    #[serde(rename = "ask1Size", default)]
    ask_size: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TickerResult {
    #[serde(default)]
    list: Vec<TickerRow>,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    #[serde(rename = "fundingRate")]
    funding_rate: String,
    #[serde(rename = "fundingRateTimestamp")]
    funding_rate_timestamp: String,
}

#[derive(Debug, Deserialize)]
struct HistoryResult {
    #[serde(default)]
    list: Vec<HistoryRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstrumentRow {
    symbol: String,
    #[serde(default)]
    contract_type: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    base_coin: String,
    #[serde(default)]
    quote_coin: String,
    /// 交割合约到期时间（毫秒）。
    #[serde(default)]
    delivery_time: Option<String>,
    /// 下单限制：最小数量、数量步长与最小名义额。
    #[serde(default)]
    lot_size_filter: Option<LotSizeFilter>,
    /// 价格限制：价格步长（tickSize）。
    #[serde(default)]
    price_filter: Option<PriceFilter>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LotSizeFilter {
    #[serde(default)]
    min_order_qty: Option<String>,
    #[serde(default)]
    min_notional_value: Option<String>,
    /// 数量步长（基础币），下单数量必须是该值的整数倍。
    #[serde(default)]
    qty_step: Option<String>,
    /// 现货的最小下单金额（USDT）。
    #[serde(default)]
    min_order_amt: Option<String>,
    /// 现货的数量精度（基础币），相当于永续的 qtyStep。
    #[serde(default)]
    base_precision: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriceFilter {
    /// 价格步长（报价币），下单价格必须是该值的整数倍。
    #[serde(default)]
    tick_size: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstrumentResult {
    #[serde(default)]
    list: Vec<InstrumentRow>,
    #[serde(default)]
    next_page_cursor: Option<String>,
}

/// 当期费率、标记价与下次结算时间；字段缺失或为空时视为没有快照。
fn snapshot_from(row: &TickerRow) -> Option<FundingSnapshot> {
    let funding_rate = row
        .funding_rate
        .as_deref()
        .and_then(|value| parse_decimal(value).ok())?;
    Some(FundingSnapshot {
        funding_rate,
        mark_price: row
            .mark_price
            .as_deref()
            .and_then(|value| parse_decimal(value).ok()),
        next_funding_at: row
            .next_funding_time
            .as_deref()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .and_then(millis_to_time),
        observed_at: Utc::now(),
    })
}

/// Bybit 线性永续（USDT 本位）公开资金费率客户端。
pub struct BybitFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl BybitFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// 一次请求返回全部线性合约的当期费率，过滤出 USDT 本位的部分。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let url = format!("{}/v5/market/tickers?category=linear", self.base_url);
        let envelope: Envelope<TickerResult> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bybit tickers 失败")?;
        let rows = into_result(envelope, "market/tickers")?.list;

        let mut snapshots = Vec::with_capacity(rows.len());
        for row in &rows {
            if !row.symbol.ends_with("USDT") {
                continue;
            }
            if let Some(snapshot) = snapshot_from(row) {
                snapshots.push((row.symbol.to_ascii_uppercase(), snapshot));
            }
        }
        Ok(snapshots)
    }

    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let url = format!(
            "{}/v5/market/tickers?category=linear&symbol={symbol}",
            self.base_url
        );
        let envelope: Envelope<TickerResult> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bybit tickers 失败")?;
        let row = into_result(envelope, "market/tickers")?
            .list
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Bybit tickers 返回空数据: {symbol}"))?;
        snapshot_from(&row).ok_or_else(|| anyhow!("Bybit tickers 缺少合法的 fundingRate: {symbol}"))
    }

    /// 线性永续买一/卖一档；数量单位是币，可直接折算名义价值。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        self.category_book("linear", symbol)
    }

    /// 现货买一/卖一档；作为「现货多 + 交割空」基差机会的现货腿。
    pub fn spot_book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        self.category_book("spot", symbol)
    }

    /// 多档盘口；`spot` 为真取现货，否则取线性永续，数量单位是币。
    pub fn depth(&self, symbol: &str, spot: bool, limit: u32) -> anyhow::Result<DepthSnapshot> {
        let category = if spot { "spot" } else { "linear" };
        let limit = limit.clamp(1, 200);
        let url = format!(
            "{}/v5/market/orderbook?category={category}&symbol={symbol}&limit={limit}",
            self.base_url
        );
        let value = self.http.get_json(&url)?;
        let ret_code = value.get("retCode").and_then(Value::as_i64).unwrap_or(0);
        if ret_code != 0 {
            let message = value
                .get("retMsg")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bail!("Bybit orderbook 返回错误 {ret_code}: {message}");
        }
        let result = value
            .get("result")
            .ok_or_else(|| anyhow!("Bybit orderbook 响应缺少 result"))?;
        make_depth(
            VENUE,
            symbol,
            depth_rows(result, "b")?,
            depth_rows(result, "a")?,
            Decimal::ONE,
        )
    }

    fn category_book(&self, category: &str, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let url = format!(
            "{}/v5/market/tickers?category={category}&symbol={symbol}",
            self.base_url
        );
        let envelope: Envelope<TickerResult> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bybit tickers 失败")?;
        let row = into_result(envelope, "market/tickers")?
            .list
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Bybit tickers 返回空数据: {symbol}"))?;
        let field = |value: Option<&str>, name: &str| -> anyhow::Result<Decimal> {
            value
                .ok_or_else(|| anyhow!("Bybit ticker 缺少 {name}: {symbol}"))
                .and_then(parse_decimal)
        };
        make_book(
            field(row.bid_price.as_deref(), "bid1Price")?,
            field(row.bid_size.as_deref(), "bid1Size")?,
            field(row.ask_price.as_deref(), "ask1Price")?,
            field(row.ask_size.as_deref(), "ask1Size")?,
        )
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols: Vec<String> = self
            .linear_instruments()?
            .into_iter()
            .filter(|item| item.status.eq_ignore_ascii_case("Trading"))
            .filter(|item| item.contract_type.eq_ignore_ascii_case("LinearPerpetual"))
            .filter(|item| item.quote_coin.eq_ignore_ascii_case("USDT"))
            .map(|item| item.symbol.to_ascii_uppercase())
            .collect();
        symbols.sort();
        symbols.dedup();
        Ok(symbols)
    }

    /// 单个线性永续的最小下单数量与最小名义额。
    pub fn min_order(&self, symbol: &str) -> anyhow::Result<Option<MinOrderSpec>> {
        let url = format!(
            "{}/v5/market/instruments-info?category=linear&symbol={symbol}",
            self.base_url
        );
        let envelope: Envelope<InstrumentResult> =
            serde_json::from_value(self.http.get_json(&url)?)
                .context("解析 Bybit instruments 失败")?;
        let Some(row) = into_result(envelope, "market/instruments-info")?
            .list
            .into_iter()
            .next()
        else {
            return Ok(None);
        };

        let mut spec = MinOrderSpec::new(VENUE, symbol);
        if let Some(filter) = row.lot_size_filter {
            spec.min_quantity = filter
                .min_order_qty
                .as_deref()
                .and_then(|value| parse_decimal(value).ok());
            spec.min_notional_usd = filter
                .min_notional_value
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
            // 下单数量必须是 qtyStep 的整数倍，否则 Bybit 以 10001「Qty invalid」拒单。
            spec.step_size = filter
                .qty_step
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
        }
        if let Some(filter) = row.price_filter {
            spec.tick_size = filter
                .tick_size
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
        }
        Ok(Some(spec))
    }

    /// 单个现货交易对的最小下单数量、最小下单额与数量精度。
    ///
    /// 现货的 `lotSizeFilter` 用 `basePrecision` / `minOrderAmt` 描述精度与最小值，
    /// 与永续的 `qtyStep` / `minNotionalValue` 字段不同，需要单独解析。
    pub fn spot_min_order(&self, symbol: &str) -> anyhow::Result<Option<MinOrderSpec>> {
        let url = format!(
            "{}/v5/market/instruments-info?category=spot&symbol={symbol}",
            self.base_url
        );
        let envelope: Envelope<InstrumentResult> =
            serde_json::from_value(self.http.get_json(&url)?)
                .context("解析 Bybit instruments 失败")?;
        let Some(row) = into_result(envelope, "market/instruments-info")?
            .list
            .into_iter()
            .next()
        else {
            return Ok(None);
        };

        let mut spec = MinOrderSpec::new(VENUE, symbol);
        if let Some(filter) = row.lot_size_filter {
            spec.min_quantity = filter
                .min_order_qty
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
            spec.min_notional_usd = filter
                .min_order_amt
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
            spec.step_size = filter
                .base_precision
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
        }
        if let Some(filter) = row.price_filter {
            spec.tick_size = filter
                .tick_size
                .as_deref()
                .and_then(|value| parse_decimal(value).ok())
                .filter(|value| *value > Decimal::ZERO);
        }
        Ok(Some(spec))
    }

    /// 当前在交易的 USDT 本位交割合约；每个交易对只保留最近到期的一张。
    pub fn delivery_contracts(&self, symbols: &[String]) -> anyhow::Result<Vec<DeliveryContract>> {
        let now = Utc::now();
        let mut nearest: HashMap<String, DeliveryContract> = HashMap::new();
        for item in self.linear_instruments()? {
            if !item.status.eq_ignore_ascii_case("Trading") {
                continue;
            }
            if !item.contract_type.eq_ignore_ascii_case("LinearFutures") {
                continue;
            }
            if !item.quote_coin.eq_ignore_ascii_case("USDT") {
                continue;
            }
            let underlying = format!("{}{}", item.base_coin, item.quote_coin).to_ascii_uppercase();
            if !symbols.is_empty()
                && !symbols
                    .iter()
                    .any(|symbol| symbol.eq_ignore_ascii_case(&underlying))
            {
                continue;
            }
            let Some(expiry) = item
                .delivery_time
                .as_deref()
                .and_then(|value| value.trim().parse::<i64>().ok())
                .and_then(millis_to_time)
            else {
                continue;
            };
            if expiry <= now {
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

    /// 线性合约清单（永续 + 交割），按游标翻页取全。
    fn linear_instruments(&self) -> anyhow::Result<Vec<InstrumentRow>> {
        let mut rows = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = format!(
                "{}/v5/market/instruments-info?category=linear&limit={PAGE_LIMIT}",
                self.base_url
            );
            if let Some(cursor) = cursor.as_deref().filter(|value| !value.is_empty()) {
                url.push_str(&format!("&cursor={cursor}"));
            }
            let envelope: Envelope<InstrumentResult> =
                serde_json::from_value(self.http.get_json(&url)?)
                    .context("解析 Bybit instruments-info 失败")?;
            let result = into_result(envelope, "market/instruments-info")?;
            rows.extend(result.list);

            match result.next_page_cursor.filter(|value| !value.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(rows)
    }

    /// 历史已结算费率，按结算时间升序返回；`start` 为已入库的最后一条。
    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let mut url = format!(
            "{}/v5/market/funding/history?category=linear&symbol={symbol}&limit={HISTORY_LIMIT}",
            self.base_url
        );
        // Bybit 只给 startTime 会返回 10001「Time Is Invalid」，必须同时带 endTime。
        if let Some(start) = start {
            url.push_str(&format!(
                "&startTime={}&endTime={}",
                start.timestamp_millis() + 1,
                Utc::now().timestamp_millis()
            ));
        }

        let envelope: Envelope<HistoryResult> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bybit funding history 失败")?;
        let rows = into_result(envelope, "market/funding/history")?.list;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let millis = row
                .funding_rate_timestamp
                .trim()
                .parse::<i64>()
                .map_err(|_| {
                    anyhow!(
                        "fundingRateTimestamp 不合法: {}",
                        row.funding_rate_timestamp
                    )
                })?;
            let funding_time = millis_to_time(millis)
                .ok_or_else(|| anyhow!("fundingRateTimestamp 越界: {millis}"))?;
            if start.map(|start| funding_time <= start).unwrap_or(false) {
                continue;
            }
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: parse_decimal(&row.funding_rate)?,
                mark_price: None,
            });
        }
        rates.sort_by_key(|row| row.funding_time);
        Ok(rates)
    }

    /// 线性永续 K 线；Bybit 返回按时间倒序的数组行。
    pub fn candles(&self, symbol: &str, interval: &str, limit: u32) -> anyhow::Result<Vec<Candle>> {
        let interval = bybit_interval(interval)?;
        let url = format!(
            "{}/v5/market/kline?category=linear&symbol={symbol}&interval={interval}&limit={}",
            self.base_url,
            limit.clamp(1, 1_000)
        );
        let envelope: Envelope<Value> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bybit K 线失败")?;
        let result = into_result(envelope, "market/kline")?;
        let rows: Vec<Vec<Value>> = serde_json::from_value(
            result
                .get("list")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        )
        .context("解析 Bybit K 线行失败")?;
        candles_from_rows(rows, true)
    }
}

/// Bybit 的 K 线周期用分钟数表示。
fn bybit_interval(interval: &str) -> anyhow::Result<&'static str> {
    Ok(match interval {
        "1m" => "1",
        "5m" => "5",
        "15m" => "15",
        "30m" => "30",
        "1h" => "60",
        "4h" => "240",
        "1d" => "D",
        "1w" => "W",
        other => bail!("Bybit 不支持的 K 线周期: {other}"),
    })
}

impl FundingVenueClient for BybitFundingClient {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_ticker_snapshot() {
        let row = TickerRow {
            symbol: "BTCUSDT".to_string(),
            funding_rate: Some("0.0001".to_string()),
            mark_price: Some("78358.16".to_string()),
            next_funding_time: Some("1793808000000".to_string()),
            bid_price: None,
            bid_size: None,
            ask_price: None,
            ask_size: None,
        };
        let snapshot = snapshot_from(&row).expect("应能解析");
        assert_eq!(snapshot.funding_rate.to_string(), "0.0001");
        assert_eq!(snapshot.mark_price.unwrap().to_string(), "78358.16");
        assert!(snapshot.next_funding_at.is_some());
    }

    #[test]
    fn drops_ticker_without_rate() {
        let row = TickerRow {
            symbol: "BTCUSDT".to_string(),
            funding_rate: Some(String::new()),
            mark_price: None,
            next_funding_time: None,
            bid_price: None,
            bid_size: None,
            ask_price: None,
            ask_size: None,
        };
        assert!(snapshot_from(&row).is_none());
    }

    #[test]
    fn parses_lot_size_qty_step_and_tick_size() {
        let payload: InstrumentResult = serde_json::from_str(
            r#"{"list":[{"symbol":"BTCUSDT","contractType":"LinearPerpetual","status":"Trading",
                "baseCoin":"BTC","quoteCoin":"USDT",
                "lotSizeFilter":{"minOrderQty":"0.001","minNotionalValue":"5","qtyStep":"0.001"},
                "priceFilter":{"tickSize":"0.1"}}]}"#,
        )
        .expect("fixture 应可解析");

        let row = payload.list.into_iter().next().expect("应含一条合约");
        let lot = row.lot_size_filter.expect("应含 lotSizeFilter");
        assert_eq!(
            lot.qty_step
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            Some(dec!(0.001))
        );
        let price = row.price_filter.expect("应含 priceFilter");
        assert_eq!(
            price
                .tick_size
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            Some(dec!(0.1))
        );
    }

    #[test]
    fn parses_spot_lot_size_precision_and_min_amount() {
        let payload: InstrumentResult = serde_json::from_str(
            r#"{"list":[{"symbol":"BTCUSDT","baseCoin":"BTC","quoteCoin":"USDT",
                "lotSizeFilter":{"basePrecision":"0.000001","minOrderQty":"0.000048","minOrderAmt":"1"},
                "priceFilter":{"tickSize":"0.01"}}]}"#,
        )
        .expect("fixture 应可解析");

        let row = payload.list.into_iter().next().expect("应含一条现货");
        let lot = row.lot_size_filter.expect("应含 lotSizeFilter");
        assert_eq!(
            lot.base_precision
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            Some(dec!(0.000001))
        );
        assert_eq!(
            lot.min_order_amt
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            Some(dec!(1))
        );
        assert_eq!(
            lot.min_order_qty
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            Some(dec!(0.000048))
        );
    }
}
