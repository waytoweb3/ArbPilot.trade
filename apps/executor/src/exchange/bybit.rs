use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{millis_to_time, parse_decimal, FundingRate, FundingSnapshot, FundingVenueClient};
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
    quote_coin: String,
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

    /// 当前正在交易的 USDT 本位永续合约清单，按游标翻页取全。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = format!(
                "{}/v5/market/instruments-info?category=linear&limit={PAGE_LIMIT}",
                self.base_url
            );
            if let Some(cursor) = cursor.as_deref().filter(|value| !value.is_empty()) {
                url.push_str(&format!("&cursor={cursor}"));
            }
            let envelope: Envelope<InstrumentResult> = serde_json::from_value(
                self.http.get_json(&url)?,
            )
            .context("解析 Bybit instruments-info 失败")?;
            let result = into_result(envelope, "market/instruments-info")?;

            symbols.extend(
                result
                    .list
                    .into_iter()
                    .filter(|item| item.status.eq_ignore_ascii_case("Trading"))
                    .filter(|item| item.contract_type.eq_ignore_ascii_case("LinearPerpetual"))
                    .filter(|item| item.quote_coin.eq_ignore_ascii_case("USDT"))
                    .map(|item| item.symbol.to_ascii_uppercase()),
            );

            match result.next_page_cursor.filter(|value| !value.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

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
                .map_err(|_| anyhow!("fundingRateTimestamp 不合法: {}", row.funding_rate_timestamp))?;
            let funding_time =
                millis_to_time(millis).ok_or_else(|| anyhow!("fundingRateTimestamp 越界: {millis}"))?;
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

    #[test]
    fn parses_ticker_snapshot() {
        let row = TickerRow {
            symbol: "BTCUSDT".to_string(),
            funding_rate: Some("0.0001".to_string()),
            mark_price: Some("78358.16".to_string()),
            next_funding_time: Some("1793808000000".to_string()),
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
        };
        assert!(snapshot_from(&row).is_none());
    }
}
