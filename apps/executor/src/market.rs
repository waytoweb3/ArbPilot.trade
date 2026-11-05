use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{anyhow, bail};
use arb_core::{Opportunity, OpportunityKind, Quote};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    config::Config,
    events::ExecutorEvent,
    exchange::{
        BinanceMarketClient, BookSnapshot, Candle, FundingRate, OkxMarketClient, Segment,
        OKX_VENUE, VENUE,
    },
    http_client::HttpClient,
    realtime::RealtimeFeed,
    store::Store,
};

/// Binance 现货腿标识；Binance 永续沿用 `binance`。
pub const SPOT_VENUE: &str = "binance-spot";
/// OKX 现货腿标识；OKX 永续沿用 `okx`。
pub const OKX_SPOT_VENUE: &str = "okx-spot";

/// 单腿手续费估算（bps）：现货与永续各 4 bps。
const LEG_FEE_BPS: i64 = 4;
/// 双腿滑点估算（bps）。
const SLIPPAGE_BPS: i64 = 1;
const KLINE_INTERVALS: [&str; 8] = ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

/// 盘口快照，同时保留买一/卖一档的名义价值用于估算可成交额度。
#[derive(Debug, Clone)]
struct Book {
    quote: Quote,
    bid_notional_usd: Decimal,
    ask_notional_usd: Decimal,
}

#[derive(Default)]
struct MarketState {
    books: HashMap<(String, String), Book>,
    opportunities: Vec<Opportunity>,
}

/// 真实行情源：WebSocket 推送优先，REST 只在推送缺失或过期时兜底。
///
/// 盘口来自 Binance / OKX 的实时代理，并结合已入库的资金费率生成
/// 「现货多 + 永续空」的吃资金费率机会。
pub struct MarketFeed {
    symbols: Vec<String>,
    poll: Duration,
    /// WebSocket 快照的最长可用时长，超过即回退到 REST。
    ws_max_age: Duration,
    binance: Option<Arc<BinanceMarketClient>>,
    okx: Option<Arc<OkxMarketClient>>,
    realtime: Option<Arc<RealtimeFeed>>,
    store: Arc<Store>,
    state: RwLock<MarketState>,
    events: broadcast::Sender<ExecutorEvent>,
}

impl MarketFeed {
    pub fn start(
        config: &Config,
        http: Option<Arc<HttpClient>>,
        realtime: Option<Arc<RealtimeFeed>>,
        store: Arc<Store>,
        events: broadcast::Sender<ExecutorEvent>,
    ) -> Arc<Self> {
        let (binance, okx) = match http {
            Some(http) => {
                let binance = Some(Arc::new(BinanceMarketClient::new(
                    &config.binance_spot_url,
                    &config.binance_fapi_url,
                    http.clone(),
                )));
                let okx = config
                    .okx_enabled
                    .then(|| Arc::new(OkxMarketClient::new(&config.okx_base_url, http.clone())));
                (binance, okx)
            }
            None => {
                tracing::error!("行情 HTTP 客户端不可用，盘口只能依赖 WebSocket 推送");
                (None, None)
            }
        };

        let poll = Duration::from_secs(config.quote_poll_secs.max(1));
        let feed = Arc::new(Self {
            symbols: config.market_symbols.clone(),
            poll,
            ws_max_age: poll * 2,
            binance,
            okx,
            realtime,
            store,
            state: RwLock::new(MarketState::default()),
            events,
        });

        if feed.symbols.is_empty()
            || (feed.binance.is_none() && feed.okx.is_none() && feed.realtime.is_none())
        {
            tracing::info!("盘口采集已停用（检查 ARBPILOT_BINANCE_* / ARBPILOT_OKX_* 与 ARBPILOT_MARKET_SYMBOLS）");
            return feed;
        }

        tracing::info!(
            binance_spot = %config.binance_spot_url,
            binance_futures = %config.binance_fapi_url,
            okx_enabled = feed.okx.is_some(),
            okx_base_url = %config.okx_base_url,
            websocket = feed.realtime.is_some(),
            symbols = ?feed.symbols,
            poll_secs = feed.poll.as_secs(),
            ws_max_age_secs = feed.ws_max_age.as_secs(),
            proxy = ?config.market_proxy,
            "盘口采集已启动"
        );

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(worker.poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                worker.refresh().await;
            }
        });

        feed
    }

    pub fn opportunities(&self) -> Vec<Opportunity> {
        self.state
            .read()
            .expect("行情锁不可用")
            .opportunities
            .clone()
    }

    pub fn quote(&self, venue: &str, symbol: &str) -> Option<Quote> {
        self.state
            .read()
            .expect("行情锁不可用")
            .books
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|book| book.quote.clone())
    }

    /// 现货 K 线，供桌面端图表展示。`venue` 为 `binance-spot` / `okx-spot` 等标识。
    pub async fn candles(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let symbol = normalize_symbol(symbol)?;
        let interval = interval.trim().to_string();
        if !KLINE_INTERVALS.contains(&interval.as_str()) {
            bail!("不支持的 K 线周期: {interval}");
        }
        let limit = limit.clamp(1, 1_000);

        match venue.trim().to_ascii_lowercase().as_str() {
            OKX_VENUE | OKX_SPOT_VENUE => {
                let client = self
                    .okx
                    .clone()
                    .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &interval, limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            _ => {
                let client = self
                    .binance
                    .clone()
                    .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &interval, limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
        }
    }

    async fn refresh(self: &Arc<Self>) {
        if self.binance.is_none() && self.okx.is_none() && self.realtime.is_none() {
            return;
        }

        let now = Utc::now();
        let pushed = self
            .realtime
            .as_ref()
            .map(|feed| feed.books())
            .unwrap_or_default();

        // 先取 WebSocket 快照，缺失或过期（例如刚启动、断线）的腿才走 REST。
        let mut latest: HashMap<(String, String), Book> = HashMap::new();
        let mut stale: Vec<(String, String, Segment)> = Vec::new();
        for symbol in &self.symbols {
            let mut legs: Vec<(&str, Segment)> = Vec::new();
            if self.binance.is_some() {
                legs.push((SPOT_VENUE, Segment::Spot));
                legs.push((VENUE, Segment::Swap));
            }
            if self.okx.is_some() {
                legs.push((OKX_SPOT_VENUE, Segment::Spot));
                legs.push((OKX_VENUE, Segment::Swap));
            }

            for (venue, segment) in legs {
                let key = (venue.to_string(), symbol.clone());
                match pushed.get(&key) {
                    Some(update) if is_fresh(update.observed_at, now, self.ws_max_age) => {
                        latest.insert(
                            key,
                            Book {
                                quote: Quote {
                                    venue: venue.to_string(),
                                    symbol: symbol.clone(),
                                    bid: update.snapshot.bid,
                                    ask: update.snapshot.ask,
                                    observed_at: update.observed_at,
                                },
                                bid_notional_usd: update.snapshot.bid_notional_usd,
                                ask_notional_usd: update.snapshot.ask_notional_usd,
                            },
                        );
                    }
                    _ => stale.push((venue.to_string(), symbol.clone(), segment)),
                }
            }
        }

        if !stale.is_empty() {
            let binance = self.binance.clone();
            let okx = self.okx.clone();
            let fetched = tokio::task::spawn_blocking(move || {
                let mut books = Vec::new();
                let mut errors = Vec::new();
                for (venue, symbol, segment) in stale {
                    match rest_book(binance.as_deref(), okx.as_deref(), &venue, &symbol, segment) {
                        Ok(snapshot) => books.push(Book {
                            quote: Quote {
                                venue,
                                symbol,
                                bid: snapshot.bid,
                                ask: snapshot.ask,
                                observed_at: Utc::now(),
                            },
                            bid_notional_usd: snapshot.bid_notional_usd,
                            ask_notional_usd: snapshot.ask_notional_usd,
                        }),
                        Err(error) => errors.push(format!("{venue} {symbol}: {error}")),
                    }
                }
                (books, errors)
            })
            .await;

            match fetched {
                Ok((books, errors)) => {
                    if !errors.is_empty() {
                        let message = errors.join("; ");
                        tracing::warn!(%message, "盘口 REST 兜底部分失败");
                    }
                    for book in books {
                        latest.insert((book.quote.venue.clone(), book.quote.symbol.clone()), book);
                    }
                }
                Err(error) => tracing::error!(%error, "盘口 REST 兜底任务异常退出"),
            }
        }

        let mut funding = Vec::new();
        for venue in [VENUE, OKX_VENUE] {
            match self.store.funding_latest(venue, &self.symbols).await {
                Ok(rows) => funding.extend(rows),
                Err(error) => tracing::warn!(%error, venue, "读取最新资金费率失败"),
            }
        }

        let opportunities = funding_opportunities(&latest, &funding, now, self.poll.as_secs());

        {
            let mut state = self.state.write().expect("行情锁不可用");
            state.books = latest;
            state.opportunities = opportunities;
        }

        let count = self.opportunities().len();
        let _ = self.events.send(ExecutorEvent::OpportunityUpdated {
            count,
            observed_at: now,
        });
    }
}

/// WebSocket 快照是否仍在可用窗口内。
fn is_fresh(observed_at: DateTime<Utc>, now: DateTime<Utc>, max_age: Duration) -> bool {
    let age_ms = now.signed_duration_since(observed_at).num_milliseconds();
    age_ms >= 0 && age_ms <= max_age.as_millis() as i64
}

/// REST 兜底：按 venue 选择对应交易所的盘口客户端。
fn rest_book(
    binance: Option<&BinanceMarketClient>,
    okx: Option<&OkxMarketClient>,
    venue: &str,
    symbol: &str,
    segment: Segment,
) -> anyhow::Result<BookSnapshot> {
    match venue {
        SPOT_VENUE | VENUE => binance
            .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?
            .book(symbol, segment),
        OKX_SPOT_VENUE | OKX_VENUE => okx
            .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?
            .book(symbol, segment),
        other => bail!("不支持的行情来源: {other}"),
    }
}

/// 现货多 + 永续空的资金费率机会：正费率时永续空头收取资金费。
///
/// 每个资金费率行都配同交易所的现货与永续盘口，因此 Binance / OKX 各自成腿，
/// 不会跨交易所撮合（跨所价差另属基差策略）。
fn funding_opportunities(
    books: &HashMap<(String, String), Book>,
    funding: &[FundingRate],
    now: DateTime<Utc>,
    poll_secs: u64,
) -> Vec<Opportunity> {
    let expires_at = now + ChronoDuration::seconds((poll_secs as i64 * 2).max(10));
    let fee_bps = Decimal::from(LEG_FEE_BPS * 2);
    let slippage_bps = Decimal::from(SLIPPAGE_BPS);

    let mut opportunities = Vec::with_capacity(funding.len());
    for row in funding {
        // 现货无法做空，只有正费率才构成「买现货 + 空永续」的收资费机会。
        if row.funding_rate <= Decimal::ZERO {
            continue;
        }
        let spot_venue = spot_venue(&row.venue);
        let Some(spot) = books.get(&(spot_venue.clone(), row.symbol.clone())) else {
            continue;
        };
        let Some(perp) = books.get(&(row.venue.clone(), row.symbol.clone())) else {
            continue;
        };

        let gross_bps = (row.funding_rate * Decimal::from(10_000)).round_dp(2);
        opportunities.push(Opportunity {
            id: Uuid::new_v4(),
            kind: OpportunityKind::FundingRate,
            symbol: row.symbol.clone(),
            long_venue: spot_venue,
            short_venue: row.venue.clone(),
            gross_spread_bps: gross_bps,
            estimated_fee_bps: fee_bps,
            estimated_slippage_bps: slippage_bps,
            net_edge_bps: gross_bps - fee_bps - slippage_bps,
            max_notional_usd: spot.ask_notional_usd.min(perp.bid_notional_usd).round_dp(0),
            funding_rate: Some(row.funding_rate),
            observed_at: now,
            expires_at,
        });
    }

    opportunities.sort_by(|a, b| b.net_edge_bps.cmp(&a.net_edge_bps));
    opportunities
}

/// 永续 venue → 同交易所的现货 venue（`binance` → `binance-spot`）。
fn spot_venue(venue: &str) -> String {
    format!("{venue}-spot")
}

fn normalize_symbol(symbol: &str) -> anyhow::Result<String> {
    let symbol = symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() || !symbol.chars().all(|char| char.is_ascii_alphanumeric()) {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn book(venue: &str, bid: Decimal, ask: Decimal) -> Book {
        Book {
            quote: Quote {
                venue: venue.to_string(),
                symbol: "BTCUSDT".to_string(),
                bid,
                ask,
                observed_at: Utc::now(),
            },
            bid_notional_usd: bid * dec!(2),
            ask_notional_usd: ask * dec!(3),
        }
    }

    fn funding(venue: &str, rate: Decimal) -> FundingRate {
        FundingRate {
            venue: venue.to_string(),
            symbol: "BTCUSDT".to_string(),
            funding_time: Utc::now(),
            funding_rate: rate,
            mark_price: None,
        }
    }

    #[test]
    fn builds_spot_long_perp_short_opportunity() {
        let mut books = HashMap::new();
        books.insert(
            (SPOT_VENUE.to_string(), "BTCUSDT".to_string()),
            book(SPOT_VENUE, dec!(100), dec!(101)),
        );
        books.insert(
            (VENUE.to_string(), "BTCUSDT".to_string()),
            book(VENUE, dec!(100), dec!(101)),
        );

        let opportunities =
            funding_opportunities(&books, &[funding(VENUE, dec!(0.0001))], Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        let opportunity = &opportunities[0];
        assert_eq!(opportunity.long_venue, SPOT_VENUE);
        assert_eq!(opportunity.short_venue, VENUE);
        assert_eq!(opportunity.gross_spread_bps, dec!(1));
        assert_eq!(opportunity.net_edge_bps, dec!(-8));
        // 现货卖一 101*3 与永续买一 100*2 取较小者。
        assert_eq!(opportunity.max_notional_usd, dec!(200));
    }

    #[test]
    fn builds_okx_opportunity_from_okx_books() {
        let mut books = HashMap::new();
        books.insert(
            (OKX_SPOT_VENUE.to_string(), "BTCUSDT".to_string()),
            book(OKX_SPOT_VENUE, dec!(100), dec!(101)),
        );
        books.insert(
            (OKX_VENUE.to_string(), "BTCUSDT".to_string()),
            book(OKX_VENUE, dec!(100), dec!(101)),
        );

        let opportunities =
            funding_opportunities(&books, &[funding(OKX_VENUE, dec!(0.0002))], Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        assert_eq!(opportunities[0].long_venue, OKX_SPOT_VENUE);
        assert_eq!(opportunities[0].short_venue, OKX_VENUE);
    }

    #[test]
    fn skips_non_positive_funding() {
        let mut books = HashMap::new();
        books.insert(
            (SPOT_VENUE.to_string(), "BTCUSDT".to_string()),
            book(SPOT_VENUE, dec!(100), dec!(101)),
        );
        books.insert(
            (VENUE.to_string(), "BTCUSDT".to_string()),
            book(VENUE, dec!(100), dec!(101)),
        );

        assert!(
            funding_opportunities(&books, &[funding(VENUE, dec!(-0.0001))], Utc::now(), 60)
                .is_empty()
        );
    }

    #[test]
    fn requires_both_legs() {
        let mut books = HashMap::new();
        books.insert(
            (SPOT_VENUE.to_string(), "BTCUSDT".to_string()),
            book(SPOT_VENUE, dec!(100), dec!(101)),
        );

        assert!(
            funding_opportunities(&books, &[funding(VENUE, dec!(0.0001))], Utc::now(), 60)
                .is_empty()
        );
    }

    #[test]
    fn rejects_invalid_symbols() {
        assert!(normalize_symbol("BTCUSDT").is_ok());
        assert!(normalize_symbol("btcusdt").is_ok());
        assert!(normalize_symbol("").is_err());
        assert!(normalize_symbol("BTC-USDT").is_err());
    }
}
