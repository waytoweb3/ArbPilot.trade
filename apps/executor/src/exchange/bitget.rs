use std::sync::Arc;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

use super::{
    make_book, millis_to_time, parse_decimal, BookSnapshot, FundingRate, FundingSnapshot,
    FundingVenueClient,
};
use crate::http_client::HttpClient;

/// Bitget 的交易所标识。
pub const VENUE: &str = "bitget";

/// Bitget 历史费率单页最多 100 条。
const HISTORY_LIMIT: u32 = 100;

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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRow {
    funding_rate: String,
    funding_time: String,
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
