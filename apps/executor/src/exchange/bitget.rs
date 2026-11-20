use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    candles_from_rows, depth_rows, make_book, make_depth, millis_to_time, parse_decimal,
    BookSnapshot, Candle, DepthSnapshot, FundingRate, FundingSnapshot, FundingVenueClient,
    MinOrderSpec,
};
use crate::http_client::HttpClient;

/// Bitget 的交易所标识。
pub const VENUE: &str = "bitget";

/// Bitget 现货的交易所标识。
pub const SPOT_VENUE: &str = "bitget-spot";

/// Bitget 历史费率单页最多 100 条。
const HISTORY_LIMIT: u32 = 100;

/// Bitget 现货与永续共用 `BTCUSDT` 形式的 symbol，这里只做去空格与大写。
pub fn spot_symbol(symbol: &str) -> String {
    symbol.trim().to_ascii_uppercase()
}

/// Bitget v2 统一的响应信封：`code` 为 "00000" 才表示成功。
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Envelope<T> {
    #[serde(default)]
    code: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Vec<T>,
}

fn into_data<T>(envelope: Envelope<T>, endpoint: &str) -> anyhow::Result<Vec<T>> {
    if envelope.code != "00000" {
        anyhow::bail!(
            "Bitget {endpoint} 返回错误 {}: {}",
            envelope.code,
            envelope.msg
        );
    }
    Ok(envelope.data)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TickerRow {
    symbol: String,
    #[serde(default)]
    funding_rate: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    /// 买一价 / 买一量，数量单位是币。
    #[serde(default)]
    bid_pr: Option<String>,
    #[serde(default)]
    bid_sz: Option<String>,
    /// 卖一价 / 卖一量，数量单位是币。
    #[serde(default)]
    ask_pr: Option<String>,
    #[serde(default)]
    ask_sz: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContractRow {
    symbol: String,
    #[serde(default)]
    symbol_status: Option<String>,
    #[serde(default)]
    quote_coin: Option<String>,
    /// 最小下单数量（基础币）。
    #[serde(default)]
    min_trade_num: Option<String>,
    /// 最小下单名义额（USDT）。Bitget 字段名里 `USDT` 是全大写。
    #[serde(rename = "minTradeUSDT", default)]
    min_trade_usdt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRow {
    funding_rate: String,
    funding_time: String,
}

/// `/api/v2/spot/public/symbols` 的下单限制字段。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotSymbolRow {
    symbol: String,
    /// 最小下单数量（基础币）。
    #[serde(default)]
    min_trade_num: Option<String>,
    /// 最小下单名义额（USDT）。
    #[serde(rename = "minTradeUSDT", default)]
    min_trade_usdt: Option<String>,
    /// 数量步长。
    #[serde(default)]
    size_multiplier: Option<String>,
    /// 价格精度（小数位数，字符串）。
    #[serde(default)]
    price_precision: Option<String>,
    /// 数量精度（小数位数，字符串）。
    #[serde(default)]
    quantity_precision: Option<String>,
}

/// `market/current-fund-rate` 的一行：tickers 里没有结算间隔与下次结算时间。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentFundRateRow {
    symbol: String,
    /// 结算间隔，单位小时（字符串）。
    #[serde(default)]
    funding_rate_interval: Option<String>,
    /// 下次结算时间（毫秒时间戳，字符串）。
    #[serde(default)]
    next_update: Option<String>,
}

impl CurrentFundRateRow {
    fn interval_secs(&self) -> Option<i64> {
        let hours = self
            .funding_rate_interval
            .as_deref()
            .and_then(|value| value.trim().parse::<i64>().ok())?;
        (1..=24).contains(&hours).then_some(hours * 3600)
    }

    fn next_funding_at(&self) -> Option<DateTime<Utc>> {
        self.next_update
            .as_deref()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .and_then(millis_to_time)
    }
}

/// Bitget 的 tickers 不含下次结算时间，展示时由其它交易所或结算间隔推断。
fn snapshot_from(row: &TickerRow) -> Option<FundingSnapshot> {
    let funding_rate = row
        .funding_rate
        .as_deref()
        .and_then(|value| parse_decimal(value).ok())?;
    let observed_at = row
        .ts
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .and_then(millis_to_time)
        .unwrap_or_else(Utc::now);
    Some(FundingSnapshot {
        funding_rate,
        mark_price: row
            .mark_price
            .as_deref()
            .and_then(|value| parse_decimal(value).ok()),
        next_funding_at: None,
        observed_at,
    })
}

/// Bitget USDT 本位永续公开资金费率客户端。
pub struct BitgetFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl BitgetFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率与标记价。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let url = format!(
            "{}/api/v2/mix/market/tickers?productType=usdt-futures",
            self.base_url
        );
        let envelope: Envelope<TickerRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget tickers 失败")?;
        let rows = into_data(envelope, "market/tickers")?;
        // tickers 不带下次结算时间，合并另一张全量表补齐。
        let fund_rates = match self.current_fund_rates() {
            Ok(map) => map,
            Err(error) => {
                tracing::debug!(%error, "读取 Bitget 结算信息失败，下次结算时间暂缺");
                HashMap::new()
            }
        };

        let mut snapshots = Vec::with_capacity(rows.len());
        for row in &rows {
            if !row.symbol.ends_with("USDT") {
                continue;
            }
            let Some(mut snapshot) = snapshot_from(row) else {
                continue;
            };
            if let Some(next) = fund_rates
                .get(&row.symbol.to_ascii_uppercase())
                .and_then(CurrentFundRateRow::next_funding_at)
            {
                snapshot.next_funding_at = Some(next);
            }
            snapshots.push((row.symbol.to_ascii_uppercase(), snapshot));
        }
        Ok(snapshots)
    }

    /// 全部 USDT 本位永续的结算间隔与下次结算时间，按内部 symbol 索引。
    fn current_fund_rates(&self) -> anyhow::Result<HashMap<String, CurrentFundRateRow>> {
        let url = format!(
            "{}/api/v2/mix/market/current-fund-rate?productType=usdt-futures",
            self.base_url
        );
        let envelope: Envelope<CurrentFundRateRow> =
            serde_json::from_value(self.http.get_json(&url)?)
                .context("解析 Bitget current-fund-rate 失败")?;
        Ok(into_data(envelope, "market/current-fund-rate")?
            .into_iter()
            .map(|row| (row.symbol.to_ascii_uppercase(), row))
            .collect())
    }

    /// 各合约的官方结算间隔（秒）。
    pub fn funding_intervals(&self) -> anyhow::Result<HashMap<String, i64>> {
        let mut intervals = HashMap::new();
        for (symbol, row) in self.current_fund_rates()? {
            if let Some(secs) = row.interval_secs() {
                intervals.insert(symbol, secs);
            }
        }
        Ok(intervals)
    }

    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let url = format!(
            "{}/api/v2/mix/market/ticker?symbol={symbol}&productType=usdt-futures",
            self.base_url
        );
        let envelope: Envelope<TickerRow> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bitget ticker 失败")?;
        let row = into_data(envelope, "market/ticker")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Bitget ticker 返回空数据: {symbol}"))?;
        snapshot_from(&row).ok_or_else(|| anyhow!("Bitget ticker 缺少合法的 fundingRate: {symbol}"))
    }

    /// USDT 本位永续的买一/卖一档；数量单位是币，可直接折算名义价值。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let wanted = symbol.trim().to_ascii_uppercase();
        let url = format!(
            "{}/api/v2/mix/market/tickers?productType=usdt-futures&symbol={wanted}",
            self.base_url
        );
        let envelope: Envelope<TickerRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget tickers 失败")?;
        let row = into_data(envelope, "market/tickers")?
            .into_iter()
            .find(|row| row.symbol.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| anyhow!("Bitget tickers 未返回 {wanted}"))?;
        let field = |value: Option<&str>, name: &str| -> anyhow::Result<Decimal> {
            value
                .ok_or_else(|| anyhow!("Bitget ticker 缺少 {name}: {wanted}"))
                .and_then(parse_decimal)
        };
        make_book(
            field(row.bid_pr.as_deref(), "bidPr")?,
            field(row.bid_sz.as_deref(), "bidSz")?,
            field(row.ask_pr.as_deref(), "askPr")?,
            field(row.ask_sz.as_deref(), "askSz")?,
        )
    }

    /// Bitget 现货的买一/卖一档；现货 tickers 与永续字段一致，数量单位是币。
    pub fn spot_book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let wanted = spot_symbol(symbol);
        let url = format!(
            "{}/api/v2/spot/market/tickers?symbol={wanted}",
            self.base_url
        );
        let envelope: Envelope<TickerRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget 现货 tickers 失败")?;
        let row = into_data(envelope, "spot/market/tickers")?
            .into_iter()
            .find(|row| row.symbol.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| anyhow!("Bitget 现货 tickers 未返回 {wanted}"))?;
        let field = |value: Option<&str>, name: &str| -> anyhow::Result<Decimal> {
            value
                .ok_or_else(|| anyhow!("Bitget 现货 ticker 缺少 {name}: {wanted}"))
                .and_then(parse_decimal)
        };
        make_book(
            field(row.bid_pr.as_deref(), "bidPr")?,
            field(row.bid_sz.as_deref(), "bidSz")?,
            field(row.ask_pr.as_deref(), "askPr")?,
            field(row.ask_sz.as_deref(), "askSz")?,
        )
    }

    /// USDT 本位永续的多档盘口；数量单位是币。
    pub fn depth(&self, symbol: &str, limit: u32) -> anyhow::Result<DepthSnapshot> {
        let wanted = symbol.trim().to_ascii_uppercase();
        let limit = limit.clamp(1, 200);
        let url = format!(
            "{}/api/v2/mix/market/merge-depth?symbol={wanted}&productType=usdt-futures&limit={limit}",
            self.base_url
        );
        let value = self.http.get_json(&url)?;
        let code = value.get("code").and_then(Value::as_str).unwrap_or("00000");
        if code != "00000" {
            let message = value
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bail!("Bitget merge-depth 返回错误 {code}: {message}");
        }
        let data = value
            .get("data")
            .ok_or_else(|| anyhow!("Bitget merge-depth 响应缺少 data"))?;
        make_depth(
            VENUE,
            symbol,
            depth_rows(data, "bids")?,
            depth_rows(data, "asks")?,
            Decimal::ONE,
        )
    }

    /// Bitget 现货多档盘口；`type=step0` 返回原始档位，单次最多 150 档。
    pub fn spot_depth(&self, symbol: &str, limit: u32) -> anyhow::Result<DepthSnapshot> {
        let wanted = spot_symbol(symbol);
        let limit = limit.clamp(50, 150);
        let url = format!(
            "{}/api/v2/spot/market/orderbook?symbol={wanted}&type=step0&limit={limit}",
            self.base_url
        );
        let value = self.http.get_json(&url)?;
        let code = value.get("code").and_then(Value::as_str).unwrap_or("00000");
        if code != "00000" {
            let message = value
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bail!("Bitget 现货 orderbook 返回错误 {code}: {message}");
        }
        let data = value
            .get("data")
            .ok_or_else(|| anyhow!("Bitget 现货 orderbook 响应缺少 data"))?;
        make_depth(
            SPOT_VENUE,
            symbol,
            depth_rows(data, "bids")?,
            depth_rows(data, "asks")?,
            Decimal::ONE,
        )
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let url = format!(
            "{}/api/v2/mix/market/contracts?productType=usdt-futures",
            self.base_url
        );
        let envelope: Envelope<ContractRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget contracts 失败")?;

        let mut symbols: Vec<String> = into_data(envelope, "market/contracts")?
            .into_iter()
            .filter(|item| {
                item.symbol_status
                    .as_deref()
                    .map(|status| status.eq_ignore_ascii_case("normal"))
                    .unwrap_or(true)
            })
            .filter(|item| {
                item.quote_coin
                    .as_deref()
                    .map(|coin| coin.eq_ignore_ascii_case("USDT"))
                    .unwrap_or(true)
            })
            .map(|item| item.symbol.to_ascii_uppercase())
            .collect();
        symbols.sort();
        symbols.dedup();
        Ok(symbols)
    }

    /// 单个 USDT 本位永续的最小下单数量与最小名义额。
    pub fn min_order(&self, symbol: &str) -> anyhow::Result<Option<MinOrderSpec>> {
        let wanted = symbol.trim().to_ascii_uppercase();
        let url = format!(
            "{}/api/v2/mix/market/contracts?productType=usdt-futures&symbol={wanted}",
            self.base_url
        );
        let envelope: Envelope<ContractRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget contracts 失败")?;
        let Some(row) = into_data(envelope, "market/contracts")?
            .into_iter()
            .find(|row| row.symbol.eq_ignore_ascii_case(&wanted))
        else {
            return Ok(None);
        };

        let mut spec = MinOrderSpec::new(VENUE, &wanted);
        spec.min_quantity = row
            .min_trade_num
            .as_deref()
            .and_then(|value| parse_decimal(value).ok());
        spec.min_notional_usd = row
            .min_trade_usdt
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO);
        Ok(Some(spec))
    }

    /// 单个 Bitget 现货交易对的最小下单数量、最小名义额与数量/价格步长。
    pub fn spot_min_order(&self, symbol: &str) -> anyhow::Result<MinOrderSpec> {
        let wanted = spot_symbol(symbol);
        let url = format!("{}/api/v2/spot/public/symbols", self.base_url);
        let envelope: Envelope<SpotSymbolRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget 现货 symbols 失败")?;
        let row = into_data(envelope, "spot/public/symbols")?
            .into_iter()
            .find(|row| row.symbol.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| anyhow!("Bitget 现货未找到交易对: {wanted}"))?;

        let mut spec = MinOrderSpec::new(SPOT_VENUE, symbol);
        spec.min_quantity = row
            .min_trade_num
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO);
        spec.min_notional_usd = row
            .min_trade_usdt
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO);
        // `sizeMultiplier` 就是数量步长；缺失时回退到 `quantityPrecision` 的小数位。
        spec.step_size = row
            .size_multiplier
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO)
            .or_else(|| {
                row.quantity_precision
                    .as_deref()
                    .and_then(|value| value.trim().parse::<u32>().ok())
                    .filter(|decimals| *decimals < 28)
                    .map(|decimals| Decimal::new(1, decimals))
            });
        spec.tick_size = row
            .price_precision
            .as_deref()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|decimals| *decimals < 28)
            .map(|decimals| Decimal::new(1, decimals));
        Ok(spec)
    }

    /// 历史已结算费率，按结算时间升序返回。
    ///
    /// Bitget 不支持按时间过滤，取最新一页后只保留 `start` 之后的记录。
    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let url = format!(
            "{}/api/v2/mix/market/history-fund-rate?symbol={symbol}&productType=usdt-futures&pageSize={HISTORY_LIMIT}&pageNo=1",
            self.base_url
        );
        let envelope: Envelope<HistoryRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget history-fund-rate 失败")?;
        let rows = into_data(envelope, "market/history-fund-rate")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let millis = row
                .funding_time
                .trim()
                .parse::<i64>()
                .map_err(|_| anyhow!("fundingTime 不合法: {}", row.funding_time))?;
            let funding_time =
                millis_to_time(millis).ok_or_else(|| anyhow!("fundingTime 越界: {millis}"))?;
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

    /// USDT 本位永续 K 线；Bitget 返回按时间倒序的数组行，单次最多 200 条。
    pub fn candles(&self, symbol: &str, interval: &str, limit: u32) -> anyhow::Result<Vec<Candle>> {
        let granularity = bitget_interval(interval)?;
        let url = format!(
            "{}/api/v2/mix/market/candles?symbol={symbol}&productType=usdt-futures&granularity={granularity}&limit={}",
            self.base_url,
            limit.clamp(1, 200)
        );
        let envelope: Envelope<Vec<Value>> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Bitget K 线失败")?;
        let rows = into_data(envelope, "market/candles")?;
        candles_from_rows(rows, true)
    }

    /// Bitget 现货 K 线；现货的 `granularity` 用 `1min` / `1day` 这类写法。
    pub fn spot_candles(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let wanted = spot_symbol(symbol);
        let granularity = bitget_spot_interval(interval)?;
        let url = format!(
            "{}/api/v2/spot/market/candles?symbol={wanted}&granularity={granularity}&limit={}",
            self.base_url,
            limit.clamp(1, 200)
        );
        let envelope: Envelope<Vec<Value>> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Bitget 现货 K 线失败")?;
        let rows = into_data(envelope, "spot/market/candles")?;
        candles_from_rows(rows, true)
    }
}

/// Bitget 的 K 线周期格式：分钟用小写 m，小时/天/周用大写。
fn bitget_interval(interval: &str) -> anyhow::Result<&'static str> {
    Ok(match interval {
        "1m" => "1m",
        "5m" => "5m",
        "15m" => "15m",
        "30m" => "30m",
        "1h" => "1H",
        "4h" => "4H",
        "1d" => "1D",
        "1w" => "1W",
        other => bail!("Bitget 不支持的 K 线周期: {other}"),
    })
}

/// Bitget 现货的 K 线周期格式：分钟用 `min`，其余用 `1h` / `1day` / `1week`。
fn bitget_spot_interval(interval: &str) -> anyhow::Result<&'static str> {
    Ok(match interval {
        "1m" => "1min",
        "5m" => "5min",
        "15m" => "15min",
        "30m" => "30min",
        "1h" => "1h",
        "4h" => "4h",
        "1d" => "1day",
        "1w" => "1week",
        other => bail!("Bitget 现货不支持的 K 线周期: {other}"),
    })
}

impl FundingVenueClient for BitgetFundingClient {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_ticker_snapshot() {
        let row = TickerRow {
            symbol: "BTCUSDT".to_string(),
            funding_rate: Some("-0.000028".to_string()),
            mark_price: Some("78347.2".to_string()),
            ts: Some("1793799862316".to_string()),
            bid_pr: Some("78347.1".to_string()),
            bid_sz: Some("1.5".to_string()),
            ask_pr: Some("78347.3".to_string()),
            ask_sz: Some("2.5".to_string()),
        };
        let snapshot = snapshot_from(&row).expect("应能解析");
        assert_eq!(snapshot.funding_rate, dec!(-0.000028));
        assert!(snapshot.mark_price.is_some());
        assert!(snapshot.next_funding_at.is_none());
    }

    #[test]
    fn parses_current_fund_rate_metadata() {
        let row: CurrentFundRateRow = serde_json::from_value(serde_json::json!({
            "symbol": "BTCUSDT",
            "fundingRate": "0.000086",
            "fundingRateInterval": "4",
            "nextUpdate": "1794211200000"
        }))
        .expect("应能解析 current-fund-rate");
        assert_eq!(row.interval_secs(), Some(4 * 3600));
        assert_eq!(
            row.next_funding_at().map(|at| at.timestamp()),
            Some(1_794_211_200)
        );
    }

    #[test]
    fn builds_book_from_ticker() {
        let row = TickerRow {
            symbol: "BTCUSDT".to_string(),
            funding_rate: None,
            mark_price: None,
            ts: None,
            bid_pr: Some("78347.1".to_string()),
            bid_sz: Some("1.5".to_string()),
            ask_pr: Some("78347.3".to_string()),
            ask_sz: Some("2".to_string()),
        };
        let book = make_book(
            parse_decimal(row.bid_pr.as_deref().unwrap()).unwrap(),
            parse_decimal(row.bid_sz.as_deref().unwrap()).unwrap(),
            parse_decimal(row.ask_pr.as_deref().unwrap()).unwrap(),
            parse_decimal(row.ask_sz.as_deref().unwrap()).unwrap(),
        )
        .expect("盘口应合法");
        assert_eq!(book.bid, dec!(78347.1));
        assert_eq!(book.ask_notional_usd, dec!(156694.6));
    }
}
