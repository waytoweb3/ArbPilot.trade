use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    make_book, parse_decimal, parse_json_decimal, BookSnapshot, FundingRate, FundingSnapshot,
    FundingVenueClient,
};
use crate::http_client::HttpClient;

/// Gate.io 的交易所标识。
pub const VENUE: &str = "gate";

/// Gate 单次最多返回 1000 条历史费率。
const HISTORY_LIMIT: u32 = 1_000;

/// `BTC_USDT` → 内部统一的 `BTCUSDT`；非 USDT 本位合约返回 `None`。
fn internal_symbol(contract: &str) -> Option<String> {
    let upper = contract.trim().to_ascii_uppercase();
    let base = upper.strip_suffix("_USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}USDT"))
}

/// 内部 `BTCUSDT` → Gate 的 `BTC_USDT`。
fn gate_contract(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Gate 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}_USDT"))
}

#[derive(Debug, Deserialize)]
struct ContractRow {
    name: String,
    #[serde(default)]
    funding_rate: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    /// 下次结算时间（Unix 秒）。
    #[serde(default)]
    funding_next_apply: Option<i64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    in_delisting: Option<bool>,
    #[serde(default)]
    is_pre_market: Option<bool>,
    /// 一张合约对应的标的数量，用于把盘口张数折算成币。
    #[serde(default)]
    quanto_multiplier: Option<String>,
}

impl ContractRow {
    fn is_tradable(&self) -> bool {
        self.status
            .as_deref()
            .map(|status| status.eq_ignore_ascii_case("trading"))
            .unwrap_or(true)
            && !self.in_delisting.unwrap_or(false)
            && !self.is_pre_market.unwrap_or(false)
    }

    fn snapshot(&self) -> Option<FundingSnapshot> {
        let funding_rate = self
            .funding_rate
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())?;
        Some(FundingSnapshot {
            funding_rate,
            mark_price: self
                .mark_price
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            next_funding_at: self
                .funding_next_apply
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single()),
            observed_at: Utc::now(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    /// 结算时间（Unix 秒）。
    t: i64,
    /// 该期费率。
    r: String,
}

#[derive(Debug, Deserialize)]
struct OrderBookRow {
    #[serde(default)]
    bids: Vec<Vec<Value>>,
    #[serde(default)]
    asks: Vec<Vec<Value>>,
}

/// Gate.io USDT 本位永续公开资金费率客户端。
pub struct GateFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl GateFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    fn contracts(&self) -> anyhow::Result<Vec<ContractRow>> {
        let url = format!("{}/api/v4/futures/usdt/contracts", self.base_url);
        serde_json::from_value(self.http.get_json(&url)?).context("解析 Gate contracts 失败")
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率、标记价与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let mut snapshots = Vec::new();
        for row in self.contracts()? {
            let Some(symbol) = internal_symbol(&row.name) else {
                continue;
            };
            if !row.is_tradable() {
                continue;
            }
            if let Some(snapshot) = row.snapshot() {
                snapshots.push((symbol, snapshot));
            }
        }
        Ok(snapshots)
    }

    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let contract = gate_contract(symbol)?;
        let url = format!("{}/api/v4/futures/usdt/contracts/{contract}", self.base_url);
        let row: ContractRow =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Gate contract 失败")?;
        row.snapshot()
            .ok_or_else(|| anyhow!("Gate contract 缺少合法的 funding_rate: {contract}"))
    }

    /// USDT 本位永续的买一/卖一档；盘口张数需乘以合约乘数折算成币。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let contract = gate_contract(symbol)?;
        let multiplier = self.quanto_multiplier(&contract)?;
        let url = format!(
            "{}/api/v4/futures/usdt/order_book?contract={contract}&limit=1",
            self.base_url
        );
        let row: OrderBookRow = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Gate order_book 失败")?;
        let level = |rows: &[Vec<Value>], label: &str| -> anyhow::Result<(Decimal, Decimal)> {
            let level = rows
                .first()
                .ok_or_else(|| anyhow!("Gate order_book 缺少{label}: {contract}"))?;
            if level.len() < 2 {
                bail!("Gate order_book {label}字段不足: {level:?}");
            }
            Ok((
                parse_json_decimal(&level[0])?,
                parse_json_decimal(&level[1])?,
            ))
        };
        let (bid, bid_size) = level(&row.bids, "买一")?;
        let (ask, ask_size) = level(&row.asks, "卖一")?;
        make_book(bid, bid_size * multiplier, ask, ask_size * multiplier)
    }

    /// 合约乘数：一张合约对应的标的数量。
    fn quanto_multiplier(&self, contract: &str) -> anyhow::Result<Decimal> {
        let url = format!("{}/api/v4/futures/usdt/contracts/{contract}", self.base_url);
        let row: ContractRow = serde_json::from_value(self.http.get_json(&url)?)
            .with_context(|| format!("解析 Gate contract 失败: {contract}"))?;
        row.quanto_multiplier
            .as_deref()
            .ok_or_else(|| anyhow!("Gate contract 缺少 quanto_multiplier: {contract}"))
            .and_then(parse_decimal)
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols: Vec<String> = self
            .contracts()?
            .into_iter()
            .filter(ContractRow::is_tradable)
            .filter_map(|row| internal_symbol(&row.name))
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
        let contract = gate_contract(symbol)?;
        let mut url = format!(
            "{}/api/v4/futures/usdt/funding_rate?contract={contract}&limit={HISTORY_LIMIT}",
            self.base_url
        );
        if let Some(start) = start {
            url.push_str(&format!("&from={}", start.timestamp() + 1));
        }

        let rows: Vec<HistoryRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Gate funding_rate 失败")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let funding_time = Utc
                .timestamp_opt(row.t, 0)
                .single()
                .ok_or_else(|| anyhow!("Gate 结算时间越界: {}", row.t))?;
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: parse_decimal(&row.r)?,
                mark_price: None,
            });
        }
        rates.sort_by_key(|row| row.funding_time);
        Ok(rates)
    }
}

impl FundingVenueClient for GateFundingClient {
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
    fn maps_contract_names() {
        assert_eq!(internal_symbol("BTC_USDT").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("btc_usdt").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("BTC_USDC"), None);
        assert_eq!(gate_contract("btcusdt").unwrap(), "BTC_USDT");
        assert!(gate_contract("BTCUSDC").is_err());
    }

    #[test]
    fn builds_snapshot_from_contract() {
        let row = ContractRow {
            name: "BTC_USDT".to_string(),
            funding_rate: Some("0.000042".to_string()),
            mark_price: Some("78351.95".to_string()),
            funding_next_apply: Some(1_793_808_000),
            status: Some("trading".to_string()),
            in_delisting: Some(false),
            is_pre_market: Some(false),
            quanto_multiplier: Some("0.0001".to_string()),
        };
        let snapshot = row.snapshot().expect("应能解析");
        assert_eq!(snapshot.funding_rate, dec!(0.000042));
        assert!(snapshot.next_funding_at.is_some());
        assert!(row.is_tradable());
    }
}
