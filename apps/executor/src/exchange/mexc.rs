use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    make_book, millis_to_time, parse_json_decimal, value_to_millis, BookSnapshot, FundingRate,
    FundingSnapshot, FundingVenueClient,
};
use crate::http_client::HttpClient;

/// MEXC 的交易所标识。
pub const VENUE: &str = "mexc";

/// 历史费率单页条数，MEXC 上限 100。
const HISTORY_PAGE_SIZE: u32 = 100;
/// 首次回补最多翻几页。
const HISTORY_MAX_PAGES: usize = 3;
/// 两次请求之间的最小间隔，MEXC 对同一 IP 的请求频率限制较严。
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(200);

/// MEXC 合约名 `BTC_USDT` → 内部统一的 `BTCUSDT`；非 USDT 本位返回 `None`。
fn internal_symbol(contract: &str) -> Option<String> {
    let upper = contract.trim().to_ascii_uppercase();
    let base = upper.strip_suffix("_USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}USDT"))
}

/// 内部 `BTCUSDT` → MEXC 的 `BTC_USDT`。
fn mexc_contract(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("MEXC 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}_USDT"))
}

/// 从 `contract/depth` 解析买一/卖一；数量单位是张，乘以合约面值折算成币。
fn book_from_depth(data: &Value, contract_size: Decimal) -> anyhow::Result<BookSnapshot> {
    let level = |key: &str, label: &str| -> anyhow::Result<(Decimal, Decimal)> {
        let row = data
            .get(key)
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("MEXC 盘口缺少{label}"))?;
        if row.len() < 2 {
            bail!("MEXC 盘口{label}字段不足: {row:?}");
        }
        Ok((parse_json_decimal(&row[0])?, parse_json_decimal(&row[1])?))
    };
    let (bid, bid_size) = level("bids", "买一")?;
    let (ask, ask_size) = level("asks", "卖一")?;
    make_book(bid, bid_size * contract_size, ask, ask_size * contract_size)
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

fn into_data(envelope: Envelope, endpoint: &str) -> anyhow::Result<Value> {
    let ok = envelope.success || envelope.code == Some(0);
    if !ok {
        bail!(
            "MEXC {endpoint} 返回错误: {}",
            envelope.message.unwrap_or_else(|| "unknown".to_string())
        );
    }
    envelope
        .data
        .ok_or_else(|| anyhow!("MEXC {endpoint} 响应缺少 data"))
}

#[derive(Debug, Deserialize)]
struct RateRow {
    symbol: String,
    #[serde(rename = "fundingRate", default)]
    funding_rate: Option<Value>,
    /// 下次结算时间（毫秒）。
    #[serde(rename = "nextSettleTime", default)]
    next_settle_time: Option<Value>,
    #[serde(rename = "fairPrice", default)]
    fair_price: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    #[serde(rename = "fundingRate", default)]
    funding_rate: Option<Value>,
    /// 结算时间（毫秒）。
    #[serde(rename = "settleTime", default)]
    settle_time: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HistoryPage {
    #[serde(rename = "resultList", default)]
    result_list: Vec<HistoryRow>,
    #[serde(rename = "totalPage", default)]
    total_page: Option<i64>,
}

/// MEXC USDT 本位永续公开资金费率客户端。
pub struct MexcFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
    /// 上次请求时间，用于进程内节流。
    gate: Mutex<Option<Instant>>,
    /// 合约面值（一张合约对应的币数）缓存，静态数据不必每次盘口都查。
    sizes: Mutex<HashMap<String, Decimal>>,
}

impl MexcFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            gate: Mutex::new(None),
            sizes: Mutex::new(HashMap::new()),
        }
    }

    /// USDT 本位永续的买一/卖一档；盘口数量单位是张，需乘以合约面值折算成币。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let contract = mexc_contract(symbol)?;
        let contract_size = self.contract_size(&contract)?;
        let url = format!("{}/api/v1/contract/depth/{contract}", self.base_url);
        let envelope: Envelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 MEXC depth 失败")?;
        let data = into_data(envelope, "contract/depth")?;
        book_from_depth(&data, contract_size)
    }

    /// 一张合约对应的标的数量，供 WebSocket 盘口把张数折算成币。
    pub fn symbol_contract_size(&self, symbol: &str) -> anyhow::Result<Decimal> {
        let contract = mexc_contract(symbol)?;
        self.contract_size(&contract)
    }

    /// 合约面值：一张合约对应的标的数量。
    fn contract_size(&self, contract: &str) -> anyhow::Result<Decimal> {
        if let Some(size) = self
            .sizes
            .lock()
            .expect("MEXC 合约面值锁不可用")
            .get(contract)
            .copied()
        {
            return Ok(size);
        }
        let url = format!("{}/api/v1/contract/detail?symbol={contract}", self.base_url);
        let envelope: Envelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 MEXC 合约信息失败")?;
        let data = into_data(envelope, "contract/detail")?;
        let raw = data
            .get("contractSize")
            .ok_or_else(|| anyhow!("MEXC 合约信息缺少 contractSize: {contract}"))?;
        let size = parse_json_decimal(raw)?;
        if size <= Decimal::ZERO {
            bail!("MEXC 合约面值不合法: {contract} {size}");
        }
        self.sizes
            .lock()
            .expect("MEXC 合约面值锁不可用")
            .insert(contract.to_string(), size);
        Ok(size)
    }

    /// 按最小间隔串行化请求，避免触发 MEXC 的频率限制。
    fn fetch(&self, url: &str) -> anyhow::Result<Value> {
        {
            let mut last = self.gate.lock().expect("MEXC 限流锁不可用");
            if let Some(previous) = *last {
                let elapsed = previous.elapsed();
                if elapsed < MIN_REQUEST_INTERVAL {
                    std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
                }
            }
            *last = Some(Instant::now());
        }
        self.http.get_json(url)
    }

    fn rates(&self) -> anyhow::Result<Vec<RateRow>> {
        let url = format!("{}/api/v1/contract/funding_rate", self.base_url);
        let envelope: Envelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 MEXC 费率失败")?;
        let data = into_data(envelope, "contract/funding_rate")?;
        serde_json::from_value(data).context("解析 MEXC 费率列表失败")
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率、标记价与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let now = Utc::now();
        let mut snapshots = Vec::new();
        for row in self.rates()? {
            let Some(symbol) = internal_symbol(&row.symbol) else {
                continue;
            };
            let Some(rate) = row
                .funding_rate
                .as_ref()
                .and_then(|item| parse_json_decimal(item).ok())
            else {
                continue;
            };
            snapshots.push((
                symbol,
                FundingSnapshot {
                    funding_rate: rate,
                    mark_price: row
                        .fair_price
                        .as_ref()
                        .and_then(|item| parse_json_decimal(item).ok()),
                    next_funding_at: row
                        .next_settle_time
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
            .rates()?
            .iter()
            .filter_map(|row| internal_symbol(&row.symbol))
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
        let contract = mexc_contract(symbol)?;
        let mut rates = Vec::new();

        for page in 1..=HISTORY_MAX_PAGES {
            let url = format!(
                "{}/api/v1/contract/funding_rate/history?symbol={contract}&page_num={page}&page_size={HISTORY_PAGE_SIZE}",
                self.base_url
            );
            let envelope: Envelope =
                serde_json::from_value(self.fetch(&url)?).context("解析 MEXC 历史费率失败")?;
            let data = into_data(envelope, "contract/funding_rate/history")?;
            let parsed: HistoryPage =
                serde_json::from_value(data).context("解析 MEXC 历史费率列表失败")?;

            let mut reached_known = false;
            for row in parsed.result_list {
                let Some(funding_time) = row
                    .settle_time
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

impl FundingVenueClient for MexcFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let mut rows = self.premium_index_all()?;
        rows.iter()
            .position(|(item, _)| item == symbol)
            .map(|index| rows.swap_remove(index).1)
            .ok_or_else(|| anyhow!("MEXC 未找到交易对: {symbol}"))
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
        assert_eq!(internal_symbol("BTC_USDT").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("btc_usdt").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("BTC_USDC"), None);
        assert_eq!(mexc_contract("btcusdt").unwrap(), "BTC_USDT");
        assert!(mexc_contract("BTCUSDC").is_err());
    }

    #[test]
    fn parses_numeric_fields() {
        assert_eq!(
            parse_json_decimal(&Value::from(0.0001)).unwrap(),
            rust_decimal_macros::dec!(0.0001)
        );
        assert_eq!(
            value_to_millis(&Value::from(1_793_808_000_000i64)),
            Some(1_793_808_000_000)
        );
    }

    #[test]
    fn builds_book_from_depth() {
        let data = serde_json::json!({
            "bids": [[75722.5, 120000]],
            "asks": [[75723.0, 80000]],
        });
        let book = book_from_depth(&data, rust_decimal_macros::dec!(0.0001)).unwrap();
        assert_eq!(book.bid, rust_decimal_macros::dec!(75722.5));
        assert_eq!(book.bid_notional_usd, rust_decimal_macros::dec!(908670));
        assert_eq!(book.ask_notional_usd, rust_decimal_macros::dec!(605784));
    }
}
