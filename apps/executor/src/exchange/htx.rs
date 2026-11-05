use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::{
    millis_to_time, parse_json_decimal, value_to_millis, FundingRate, FundingSnapshot,
    FundingVenueClient,
};
use crate::http_client::HttpClient;

/// HTX（火币）的交易所标识。
pub const VENUE: &str = "htx";

/// 历史费率单页条数，HTX 上限为 50。
const HISTORY_PAGE_SIZE: u32 = 50;
/// 首次回补最多翻几页（50 条/页，约 16 天）。
const HISTORY_MAX_PAGES: usize = 3;

/// HTX 合约名 `BTC-USDT` → 内部统一的 `BTCUSDT`；非 USDT 本位返回 `None`。
fn internal_symbol(contract: &str) -> Option<String> {
    let upper = contract.trim().to_ascii_uppercase();
    let base = upper.strip_suffix("-USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}USDT"))
}

/// 内部 `BTCUSDT` → HTX 的 `BTC-USDT`。
fn htx_contract(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("HTX 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}-USDT"))
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    status: String,
    #[serde(default)]
    err_msg: Option<Value>,
    #[serde(default)]
    data: Option<Value>,
}

fn into_data(envelope: Envelope, endpoint: &str) -> anyhow::Result<Value> {
    if !envelope.status.is_empty() && !envelope.status.eq_ignore_ascii_case("ok") {
        let message = envelope
            .err_msg
            .map(|value| match value {
                Value::String(text) => text,
                other => other.to_string(),
            })
            .unwrap_or_else(|| envelope.status.clone());
        bail!("HTX {endpoint} 返回错误: {message}");
    }
    envelope
        .data
        .ok_or_else(|| anyhow!("HTX {endpoint} 响应缺少 data"))
}

#[derive(Debug, Deserialize)]
struct BatchRow {
    contract_code: String,
    #[serde(default)]
    funding_rate: Option<Value>,
    /// 下次结算时间（毫秒）。
    #[serde(default)]
    funding_time: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    #[serde(default)]
    funding_rate: Option<Value>,
    #[serde(default)]
    funding_time: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HistoryPage {
    #[serde(default)]
    data: Vec<HistoryRow>,
    #[serde(default)]
    total_page: Option<i64>,
}

/// HTX USDT 本位永续公开资金费率客户端。
pub struct HtxFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl HtxFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    fn batch(&self) -> anyhow::Result<Vec<BatchRow>> {
        let url = format!(
            "{}/linear-swap-api/v1/swap_batch_funding_rate",
            self.base_url
        );
        let envelope: Envelope =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 HTX 批量费率失败")?;
        let data = into_data(envelope, "swap_batch_funding_rate")?;
        serde_json::from_value(data).context("解析 HTX 批量费率列表失败")
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let now = Utc::now();
        let mut snapshots = Vec::new();
        for row in self.batch()? {
            let Some(symbol) = internal_symbol(&row.contract_code) else {
                continue;
            };
            let Some(rate) = row.funding_rate.as_ref().and_then(|item| parse_json_decimal(item).ok())
            else {
                continue;
            };
            snapshots.push((
                symbol,
                FundingSnapshot {
                    funding_rate: rate,
                    mark_price: None,
                    next_funding_at: row
                        .funding_time
                        .as_ref()
                        .and_then(value_to_millis)
                        .and_then(millis_to_time),
                    observed_at: now,
                },
            ));
        }
        Ok(snapshots)
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols: Vec<String> = self
            .batch()?
            .iter()
            .filter_map(|row| internal_symbol(&row.contract_code))
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
        let contract = htx_contract(symbol)?;
        let mut rates = Vec::new();

        for page in 1..=HISTORY_MAX_PAGES {
            let url = format!(
                "{}/linear-swap-api/v1/swap_historical_funding_rate?contract_code={contract}&page_index={page}&page_size={HISTORY_PAGE_SIZE}",
                self.base_url
            );
            let envelope: Envelope = serde_json::from_value(self.http.get_json(&url)?)
                .context("解析 HTX 历史费率失败")?;
            let data = into_data(envelope, "swap_historical_funding_rate")?;
            let parsed: HistoryPage =
                serde_json::from_value(data).context("解析 HTX 历史费率列表失败")?;

            let mut reached_known = false;
            for row in parsed.data {
                let Some(funding_time) = row
                    .funding_time
                    .as_ref()
                    .and_then(value_to_millis)
                    .and_then(millis_to_time)
                else {
                    continue;
                };
                if start.map(|start| funding_time <= start).unwrap_or(false) {
                    reached_known = true;
                    continue;
                }
                let Some(rate) = row
                    .funding_rate
                    .as_ref()
                    .and_then(|item| parse_json_decimal(item).ok())
                else {
                    continue;
                };
                rates.push(FundingRate {
                    venue: VENUE.to_string(),
                    symbol: symbol.to_string(),
                    funding_time,
                    funding_rate: rate,
                    mark_price: None,
                });
            }

            let has_more = parsed
                .total_page
                .map(|total| (page as i64) < total)
                .unwrap_or(false);
            if reached_known || !has_more {
                break;
            }
        }

        rates.sort_by_key(|row| row.funding_time);
        rates.dedup_by_key(|row| row.funding_time);
        Ok(rates)
    }
}

impl FundingVenueClient for HtxFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let mut rows = self.premium_index_all()?;
        rows.iter()
            .position(|(item, _)| item == symbol)
            .map(|index| rows.swap_remove(index).1)
            .ok_or_else(|| anyhow!("HTX 未找到交易对: {symbol}"))
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
    fn maps_contract_names() {
        assert_eq!(internal_symbol("BTC-USDT").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("btc-usdt").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("BTC-USD"), None);
        assert_eq!(htx_contract("btcusdt").unwrap(), "BTC-USDT");
        assert!(htx_contract("BTCUSDC").is_err());
    }

    #[test]
    fn parses_numeric_strings() {
        assert_eq!(
            parse_json_decimal(&Value::String("0.00010000".into())).unwrap(),
            rust_decimal_macros::dec!(0.0001)
        );
        assert_eq!(
            value_to_millis(&Value::String("1793808000000".into())),
            Some(1_793_808_000_000)
        );
    }
}
