use std::sync::Arc;

use anyhow::{anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{
    millis_to_time, parse_json_decimal, value_to_millis, FundingRate, FundingSnapshot,
    FundingVenueClient,
};
use crate::http_client::HttpClient;

/// BingX 的交易所标识。
pub const VENUE: &str = "bingx";

/// 历史费率单次最多返回的条数。
const HISTORY_LIMIT: u32 = 100;

/// BingX 合约名 `BTC-USDT` → 内部统一的 `BTCUSDT`；非 USDT 本位返回 `None`。
fn internal_symbol(contract: &str) -> Option<String> {
    let upper = contract.trim().to_ascii_uppercase();
    let base = upper.strip_suffix("-USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}USDT"))
}

/// 内部 `BTCUSDT` → BingX 的 `BTC-USDT`。
fn bingx_contract(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("BingX 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}-USDT"))
}

/// BingX 的响应既可能是裸数组，也可能是 `{code, msg, data}` 信封，统一取出数组。
fn rows(value: Value, endpoint: &str) -> anyhow::Result<Vec<Value>> {
    if let Some(array) = value.as_array() {
        return Ok(array.clone());
    }
    if let Some(code) = value.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = value
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bail!("BingX {endpoint} 返回错误 {code}: {message}");
        }
    }
    value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("BingX {endpoint} 响应缺少 data 数组"))
}

fn field<'a>(row: &'a Value, key: &str) -> Option<&'a Value> {
    row.get(key).filter(|value| !value.is_null())
}

/// BingX USDT 本位永续公开资金费率客户端。
pub struct BingxFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl BingxFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    fn premium_rows(&self) -> anyhow::Result<Vec<Value>> {
        let url = format!("{}/openApi/swap/v2/quote/premiumIndex", self.base_url);
        rows(self.http.get_json(&url)?, "premiumIndex")
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率、标记价与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let now = Utc::now();
        let mut snapshots = Vec::new();
        for row in self.premium_rows()? {
            let Some(contract) = field(&row, "symbol").and_then(Value::as_str) else {
                continue;
            };
            let Some(symbol) = internal_symbol(contract) else {
                continue;
            };
            let Some(rate) = field(&row, "lastFundingRate")
                .and_then(|value| parse_json_decimal(value).ok())
            else {
                continue;
            };
            snapshots.push((
                symbol,
                FundingSnapshot {
                    funding_rate: rate,
                    mark_price: field(&row, "markPrice")
                        .and_then(|value| parse_json_decimal(value).ok()),
                    next_funding_at: field(&row, "nextFundingTime")
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
            .premium_rows()?
            .iter()
            .filter_map(|row| {
                field(row, "symbol")
                    .and_then(Value::as_str)
                    .and_then(internal_symbol)
            })
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
        let contract = bingx_contract(symbol)?;
        let url = format!(
            "{}/openApi/swap/v2/quote/fundingRate?symbol={contract}&limit={HISTORY_LIMIT}",
            self.base_url
        );
        let payload: Value = self.http.get_json(&url)?;
        let rows = rows(payload, "fundingRate")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(funding_time) = field(&row, "fundingTime")
                .and_then(value_to_millis)
                .and_then(millis_to_time)
            else {
                continue;
            };
            if start.map(|start| funding_time <= start).unwrap_or(false) {
                continue;
            }
            let Some(rate) = field(&row, "fundingRate")
                .and_then(|value| parse_json_decimal(value).ok())
            else {
                continue;
            };
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: rate,
                mark_price: field(&row, "markPrice")
                    .and_then(|value| parse_json_decimal(value).ok()),
            });
        }
        rates.sort_by_key(|row| row.funding_time);
        rates.dedup_by_key(|row| row.funding_time);
        Ok(rates)
    }
}

impl FundingVenueClient for BingxFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let mut rows = self.premium_index_all()?;
        rows.iter()
            .position(|(item, _)| item == symbol)
            .map(|index| rows.swap_remove(index).1)
            .ok_or_else(|| anyhow!("BingX 未找到交易对: {symbol}"))
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
        assert_eq!(internal_symbol("BTC-USDC"), None);
        assert_eq!(bingx_contract("btcusdt").unwrap(), "BTC-USDT");
        assert!(bingx_contract("BTCUSDC").is_err());
    }

    #[test]
    fn unwraps_envelope_or_array() {
        let array = serde_json::json!([{ "symbol": "BTC-USDT" }]);
        assert_eq!(rows(array, "premiumIndex").unwrap().len(), 1);

        let envelope = serde_json::json!({ "code": 0, "data": [{ "symbol": "ETH-USDT" }] });
        assert_eq!(rows(envelope, "premiumIndex").unwrap().len(), 1);

        let error = serde_json::json!({ "code": 100400, "msg": "bad" });
        assert!(rows(error, "premiumIndex").is_err());
    }
}
