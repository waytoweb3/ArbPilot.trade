use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    candles_from_rows, make_book, millis_to_time, parse_decimal, BookSnapshot, Candle, FundingRate,
    FundingSnapshot, FundingVenueClient, Segment,
};
use crate::http_client::HttpClient;

/// OKX 的交易所标识。
pub const VENUE: &str = "okx";

/// OKX 单页最多 100 条历史费率。
const HISTORY_LIMIT: u32 = 100;
/// 首次回填最多翻 8 页（800 条），足够 1 小时结算的合约覆盖 30 天。
const HISTORY_PAGES: usize = 8;
/// 首次回填的时间窗口：翻到早于该窗口的记录即停止，避免无谓请求。
const HISTORY_WINDOW_DAYS: i64 = 30;
/// OKX `/market/candles` 单次最多返回 100 根。
const KLINE_LIMIT: u32 = 100;

/// OKX 统一的响应信封：`code` 为 "0" 才表示成功。
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
    if envelope.code != "0" {
        bail!(
            "OKX {endpoint} 返回错误 {}: {}",
            envelope.code,
            envelope.msg
        );
    }
    Ok(envelope.data)
}

fn parse_millis(value: &str) -> anyhow::Result<i64> {
    value
        .trim()
        .parse()
        .map_err(|_| anyhow!("时间戳不是合法整数: {value}"))
}

/// `BTCUSDT` → `BTC-USDT`
fn spot_inst_id(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("OKX 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}-USDT"))
}

/// `BTCUSDT` → `BTC-USDT-SWAP`
fn swap_inst_id(symbol: &str) -> anyhow::Result<String> {
    Ok(format!("{}-SWAP", spot_inst_id(symbol)?))
}

/// 内部周期 → OKX bar 参数（仅大小写差异）。
fn okx_bar(interval: &str) -> anyhow::Result<&'static str> {
    let bar = match interval {
        "1m" => "1m",
        "5m" => "5m",
        "15m" => "15m",
        "30m" => "30m",
        "1h" => "1H",
        "4h" => "4H",
        "1d" => "1D",
        "1w" => "1W",
        other => bail!("OKX 不支持的 K 线周期: {other}"),
    };
    Ok(bar)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxFundingItem {
    funding_rate: String,
    funding_time: String,
    #[serde(default)]
    ts: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxTicker {
    bid_px: String,
    bid_sz: String,
    ask_px: String,
    ask_sz: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxInstrument {
    #[serde(default)]
    ct_val: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxSwapInstrument {
    inst_id: String,
    #[serde(default)]
    settle_ccy: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    ct_type: Option<String>,
}

/// `BTC-USDT-SWAP` → `BTCUSDT`；非 USDT 本位线性合约返回 `None`。
fn swap_symbol(inst_id: &str) -> Option<String> {
    let base = inst_id.trim().strip_suffix("-USDT-SWAP")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{}USDT", base.to_ascii_uppercase()))
}

/// OKX 永续资金费率客户端（公开接口，无需 API Key）。
pub struct OkxFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl OkxFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// 当期费率与下次结算时间；OKX 的 funding-rate 不返回标记价。
    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let inst_id = swap_inst_id(symbol)?;
        let url = format!(
            "{}/api/v5/public/funding-rate?instId={inst_id}",
            self.base_url
        );
        let envelope: Envelope<OkxFundingItem> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 OKX funding-rate 失败")?;
        let item = into_data(envelope, "funding-rate")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OKX funding-rate 返回空数据: {inst_id}"))?;

        Ok(FundingSnapshot {
            funding_rate: parse_decimal(&item.funding_rate)?,
            mark_price: None,
            next_funding_at: millis_to_time(parse_millis(&item.funding_time)?),
            observed_at: item
                .ts
                .as_deref()
                .and_then(|value| parse_millis(value).ok())
                .and_then(millis_to_time)
                .unwrap_or_else(Utc::now),
        })
    }

    /// 历史已结算费率，按结算时间升序返回。
    ///
    /// OKX 的游标语义与字面相反：`before` 返回比游标更新的记录，`after` 返回更早的记录。
    /// - `start` 为已入库的最后一条时，用 `before` 只增量拉取更新的记录；
    /// - 首次运行则先用 `after` 从最新一页向前翻页，最多回填 `HISTORY_PAGES * HISTORY_LIMIT` 条。
    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let inst_id = swap_inst_id(symbol)?;
        let mut rates = Vec::new();

        match start {
            Some(start) => {
                let url = format!(
                    "{}/api/v5/public/funding-rate-history?instId={inst_id}&limit={HISTORY_LIMIT}&before={}",
                    self.base_url,
                    start.timestamp_millis()
                );
                rates.extend(self.fetch_history(&url, symbol)?);
            }
            None => {
                let cutoff = Utc::now() - chrono::Duration::days(HISTORY_WINDOW_DAYS);
                let mut cursor: Option<i64> = None;
                for _ in 0..HISTORY_PAGES {
                    let mut url = format!(
                        "{}/api/v5/public/funding-rate-history?instId={inst_id}&limit={HISTORY_LIMIT}",
                        self.base_url
                    );
                    if let Some(after) = cursor {
                        url.push_str(&format!("&after={after}"));
                    }
                    let page = self.fetch_history(&url, symbol)?;
                    if page.is_empty() {
                        break;
                    }
                    let oldest = page.iter().map(|row| row.funding_time).min();
                    cursor = page
                        .iter()
                        .map(|row| row.funding_time.timestamp_millis())
                        .min();
                    rates.extend(page);
                    if oldest.map(|row| row < cutoff).unwrap_or(true) {
                        break;
                    }
                }
            }
        }

        rates.sort_by_key(|row| row.funding_time);
        rates.dedup_by(|a, b| a.funding_time == b.funding_time);
        Ok(rates)
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn swap_symbols(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/api/v5/public/instruments?instType=SWAP", self.base_url);
        let envelope: Envelope<OkxSwapInstrument> =
            serde_json::from_value(self.http.get_json(&url)?)
                .context("解析 OKX instruments 失败")?;

        let mut symbols: Vec<String> = into_data(envelope, "public/instruments")?
            .into_iter()
            .filter(|item| item.state.as_deref() == Some("live"))
            .filter(|item| {
                item.ct_type
                    .as_deref()
                    .map(|kind| kind.eq_ignore_ascii_case("linear"))
                    .unwrap_or(true)
            })
            .filter(|item| item.settle_ccy.as_deref() == Some("USDT"))
            .filter_map(|item| swap_symbol(&item.inst_id))
            .collect();
        symbols.sort();
        symbols.dedup();
        Ok(symbols)
    }

    fn fetch_history(&self, url: &str, symbol: &str) -> anyhow::Result<Vec<FundingRate>> {
        let envelope: Envelope<OkxFundingItem> = serde_json::from_value(self.http.get_json(url)?)
            .context("解析 OKX funding-rate-history 失败")?;
        let rows = into_data(envelope, "funding-rate-history")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let funding_time = millis_to_time(parse_millis(&row.funding_time)?)
                .ok_or_else(|| anyhow!("fundingTime 不合法: {}", row.funding_time))?;
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: parse_decimal(&row.funding_rate)?,
                mark_price: None,
            });
        }
        Ok(rates)
    }
}

impl FundingVenueClient for OkxFundingClient {
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
        self.swap_symbols()
    }
}

/// OKX 现货与 USDT 本位永续的盘口 / K 线客户端。
pub struct OkxMarketClient {
    base_url: String,
    http: Arc<HttpClient>,
    /// 永续合约面值缓存：OKX 的盘口数量单位是「张」，需乘 ctVal 换成币本位。
    ct_values: RwLock<HashMap<String, Decimal>>,
}

impl OkxMarketClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            ct_values: RwLock::new(HashMap::new()),
        }
    }

    pub fn book(&self, symbol: &str, segment: Segment) -> anyhow::Result<BookSnapshot> {
        let inst_id = match segment {
            Segment::Spot => spot_inst_id(symbol)?,
            Segment::Swap => swap_inst_id(symbol)?,
        };
        let url = format!("{}/api/v5/market/ticker?instId={inst_id}", self.base_url);
        let envelope: Envelope<OkxTicker> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 OKX ticker 失败")?;
        let ticker = into_data(envelope, "market/ticker")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OKX ticker 返回空数据: {inst_id}"))?;

        let bid = parse_decimal(&ticker.bid_px)?;
        let ask = parse_decimal(&ticker.ask_px)?;
        let (bid_qty, ask_qty) = match segment {
            Segment::Spot => (
                parse_decimal(&ticker.bid_sz)?,
                parse_decimal(&ticker.ask_sz)?,
            ),
            Segment::Swap => {
                let ct_val = self.contract_value(&inst_id)?;
                (
                    parse_decimal(&ticker.bid_sz)? * ct_val,
                    parse_decimal(&ticker.ask_sz)? * ct_val,
                )
            }
        };

        make_book(bid, bid_qty, ask, ask_qty)
    }

    pub fn candles(&self, symbol: &str, interval: &str, limit: u32) -> anyhow::Result<Vec<Candle>> {
        let inst_id = spot_inst_id(symbol)?;
        let bar = okx_bar(interval)?;
        let url = format!(
            "{}/api/v5/market/candles?instId={inst_id}&bar={bar}&limit={}",
            self.base_url,
            limit.clamp(1, KLINE_LIMIT)
        );
        let envelope: Envelope<Vec<Value>> =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 OKX K 线失败")?;
        candles_from_rows(into_data(envelope, "market/candles")?, true)
    }

    /// 永续合约面值（1 张 = ctVal 个基础币），供 WebSocket 盘口把「张」换算成币。
    pub fn swap_contract_value(&self, symbol: &str) -> anyhow::Result<Decimal> {
        let inst_id = swap_inst_id(symbol)?;
        self.contract_value(&inst_id)
    }

    /// 永续合约面值（1 张 = ctVal 个基础币），按合约缓存，进程内只查一次。
    fn contract_value(&self, inst_id: &str) -> anyhow::Result<Decimal> {
        if let Some(value) = self
            .ct_values
            .read()
            .expect("ctVal 缓存锁不可用")
            .get(inst_id)
            .copied()
        {
            return Ok(value);
        }

        let url = format!(
            "{}/api/v5/public/instruments?instType=SWAP&instId={inst_id}",
            self.base_url
        );
        let envelope: Envelope<OkxInstrument> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 OKX instruments 失败")?;
        let instrument = into_data(envelope, "public/instruments")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("OKX instruments 缺少合约: {inst_id}"))?;
        let raw = instrument
            .ct_val
            .ok_or_else(|| anyhow!("OKX 合约 {inst_id} 缺少 ctVal"))?;
        let ct_val = parse_decimal(&raw)?;
        if ct_val <= Decimal::ZERO {
            bail!("OKX 合约 {inst_id} 的 ctVal 非正数: {ct_val}");
        }

        self.ct_values
            .write()
            .expect("ctVal 缓存锁不可用")
            .insert(inst_id.to_string(), ct_val);
        Ok(ct_val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn maps_internal_symbols_to_okx_inst_ids() {
        assert_eq!(spot_inst_id("BTCUSDT").unwrap(), "BTC-USDT");
        assert_eq!(swap_inst_id("btcusdt").unwrap(), "BTC-USDT-SWAP");
        assert!(spot_inst_id("BTCUSDC").is_err());
        assert!(spot_inst_id("USDT").is_err());
    }

    #[test]
    fn maps_bars() {
        assert_eq!(okx_bar("5m").unwrap(), "5m");
        assert_eq!(okx_bar("1h").unwrap(), "1H");
        assert_eq!(okx_bar("1d").unwrap(), "1D");
        assert!(okx_bar("2h").is_err());
    }

    #[test]
    fn keeps_only_usdt_linear_swaps() {
        let instruments: Vec<OkxSwapInstrument> = serde_json::from_str(
            r#"[
                {"instId":"BTC-USDT-SWAP","settleCcy":"USDT","state":"live","ctType":"linear"},
                {"instId":"1INCH-USDT-SWAP","settleCcy":"USDT","state":"live","ctType":"linear"},
                {"instId":"BTC-USD-SWAP","settleCcy":"BTC","state":"live","ctType":"inverse"},
                {"instId":"ETH-USDT-SWAP","settleCcy":"USDT","state":"suspend","ctType":"linear"}
            ]"#,
        )
        .expect("fixture 应可解析");

        let symbols: Vec<String> = instruments
            .into_iter()
            .filter(|item| item.state.as_deref() == Some("live"))
            .filter(|item| item.settle_ccy.as_deref() == Some("USDT"))
            .filter_map(|item| swap_symbol(&item.inst_id))
            .collect();
        assert_eq!(
            symbols,
            vec!["BTCUSDT".to_string(), "1INCHUSDT".to_string()]
        );
    }

    #[test]
    fn parses_funding_rate_payload() {
        let envelope: Envelope<OkxFundingItem> = serde_json::from_str(
            r#"{"code":"0","msg":"","data":[{"fundingRate":"0.0000714858746824","fundingTime":"1793808000000","nextFundingRate":"","nextFundingTime":"1793836800000","instId":"BTC-USDT-SWAP","ts":"1793794447664"}]}"#,
        )
        .expect("fixture 应可解析");

        let rows = into_data(envelope, "funding-rate").unwrap();
        assert_eq!(
            parse_decimal(&rows[0].funding_rate).unwrap(),
            dec!(0.0000714858746824)
        );
        assert_eq!(
            millis_to_time(parse_millis(&rows[0].funding_time).unwrap())
                .map(|time| time.timestamp()),
            Some(1_793_808_000)
        );
        assert_eq!(rows[0].ts.as_deref(), Some("1793794447664"));
    }

    #[test]
    fn surfaces_okx_error_code() {
        let envelope: Envelope<OkxFundingItem> = serde_json::from_str(
            r#"{"code":"51001","msg":"Instrument ID does not exist","data":[]}"#,
        )
        .expect("fixture 应可解析");

        let error = into_data(envelope, "funding-rate").unwrap_err().to_string();
        assert!(error.contains("51001"), "{error}");
        assert!(error.contains("Instrument ID does not exist"), "{error}");
    }

    #[test]
    fn parses_ticker_payload() {
        let envelope: Envelope<OkxTicker> = serde_json::from_str(
            r#"{"code":"0","msg":"","data":[{"instId":"BTC-USDT","last":"78065.1","askPx":"78063.7","askSz":"1.46701494","bidPx":"78063.6","bidSz":"0.2343","ts":"1793794507664"}]}"#,
        )
        .expect("fixture 应可解析");

        let ticker = into_data(envelope, "market/ticker").unwrap().remove(0);
        assert_eq!(parse_decimal(&ticker.bid_px).unwrap(), dec!(78063.6));
        assert_eq!(parse_decimal(&ticker.ask_sz).unwrap(), dec!(1.46701494));
    }

    #[test]
    fn parses_candles_newest_first_into_ascending_order() {
        let rows: Vec<Vec<Value>> = serde_json::from_str(
            r#"[["1793794500000","78072.3","78080.2","78072.3","78080","0.71159329","55561.23","55561.23","0"],
                ["1793794200000","78183.1","78221.9","78056","78072.4","26.5392735","2073414.85","2073414.85","1"]]"#,
        )
        .expect("fixture 应可解析");

        let candles = candles_from_rows(rows, true).unwrap();
        assert_eq!(candles.len(), 2);
        assert!(candles[0].open_time < candles[1].open_time);
        assert_eq!(candles[0].close, dec!(78072.4));
        assert_eq!(candles[0].volume, dec!(26.5392735));
    }
}
