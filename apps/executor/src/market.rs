use std::{collections::HashMap, sync::RwLock, time::Duration};

use arb_core::{Opportunity, OpportunityKind, Quote};
use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::events::ExecutorEvent;

const SYMBOLS: [(&str, i64); 2] = [("BTCUSDT", 68_000), ("ETHUSDT", 3_200)];
const VENUES: [&str; 3] = ["binance", "okx", "bybit"];

#[derive(Default)]
struct MarketState {
    tick: u64,
    quotes: HashMap<(String, String), Quote>,
    opportunities: Vec<Opportunity>,
}

/// 模拟行情源：仅用于 Phase 1 流程验证。
/// 真实行情将通过各交易所 WebSocket 适配器替换该实现，接口保持不变。
pub struct MarketFeed {
    state: RwLock<MarketState>,
    events: broadcast::Sender<ExecutorEvent>,
}

impl MarketFeed {
    pub fn start(events: broadcast::Sender<ExecutorEvent>) -> std::sync::Arc<Self> {
        let feed = std::sync::Arc::new(Self {
            state: RwLock::new(MarketState::default()),
            events,
        });
        feed.advance();

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                worker.advance();
                let count = worker.opportunities().len();
                let _ = worker.events.send(ExecutorEvent::OpportunityUpdated {
                    count,
                    observed_at: Utc::now(),
                });
            }
        });

        feed
    }

    pub fn opportunities(&self) -> Vec<Opportunity> {
        self.state.read().expect("行情锁不可用").opportunities.clone()
    }

    pub fn quote(&self, venue: &str, symbol: &str) -> Option<Quote> {
        self.state
            .read()
            .expect("行情锁不可用")
            .quotes
            .get(&(venue.to_string(), symbol.to_string()))
            .cloned()
    }

    fn advance(&self) {
        let mut state = self.state.write().expect("行情锁不可用");
        state.tick += 1;
        let now = Utc::now();

        let mut quotes = HashMap::new();
        for (symbol, base_price) in SYMBOLS {
            for (index, venue) in VENUES.iter().enumerate() {
                let seed = state.tick * 31 + index as u64 * 7 + base_price as u64;
                let drift_bps = jitter(seed);
                let venue_skew_bps = (index as i64 - 1) * 6;
                let mid = Decimal::from(base_price)
                    * (Decimal::ONE
                        + Decimal::from(drift_bps + venue_skew_bps) / Decimal::from(10_000));
                let half_spread = mid / Decimal::from(20_000);
                let quote = Quote {
                    venue: (*venue).to_string(),
                    symbol: (*symbol).to_string(),
                    bid: (mid - half_spread).round_dp(2),
                    ask: (mid + half_spread).round_dp(2),
                    observed_at: now,
                };
                quotes.insert((quote.venue.clone(), quote.symbol.clone()), quote);
            }
        }

        let mut opportunities = Vec::new();
        for (symbol, _) in SYMBOLS {
            let symbol_quotes: Vec<Quote> = quotes
                .values()
                .filter(|quote| quote.symbol == symbol)
                .cloned()
                .collect();
            let mid = symbol_quotes
                .iter()
                .map(|quote| (quote.bid + quote.ask) / Decimal::from(2))
                .sum::<Decimal>()
                / Decimal::from(symbol_quotes.len() as i64);

            let mut best: Option<(Decimal, Quote, Quote)> = None;
            for long in &symbol_quotes {
                for short in &symbol_quotes {
                    if long.venue == short.venue {
                        continue;
                    }
                    let edge_bps = (short.bid - long.ask) / mid * Decimal::from(10_000);
                    if best
                        .as_ref()
                        .map(|(current, _, _)| edge_bps > *current)
                        .unwrap_or(true)
                    {
                        best = Some((edge_bps, long.clone(), short.clone()));
                    }
                }
            }

            if let Some((gross_bps, long, short)) = best {
                let fee_bps = Decimal::from(8);
                let slippage_bps = Decimal::from(2);
                opportunities.push(Opportunity {
                    id: Uuid::new_v4(),
                    kind: OpportunityKind::CexCexSpread,
                    symbol: symbol.to_string(),
                    long_venue: long.venue,
                    short_venue: short.venue,
                    gross_spread_bps: gross_bps.round_dp(2),
                    estimated_fee_bps: fee_bps,
                    estimated_slippage_bps: slippage_bps,
                    net_edge_bps: (gross_bps - fee_bps - slippage_bps).round_dp(2),
                    max_notional_usd: Decimal::from(15_000),
                    funding_rate: None,
                    observed_at: now,
                    expires_at: now + ChronoDuration::seconds(4),
                });
            }

            let funding_rate = Decimal::new(1 + (state.tick % 3) as i64, 4);
            opportunities.push(Opportunity {
                id: Uuid::new_v4(),
                kind: OpportunityKind::FundingRate,
                symbol: symbol.to_string(),
                long_venue: "binance".to_string(),
                short_venue: "bybit".to_string(),
                gross_spread_bps: (funding_rate * Decimal::from(10_000)).round_dp(2),
                estimated_fee_bps: Decimal::from(6),
                estimated_slippage_bps: Decimal::from(1),
                net_edge_bps: (funding_rate * Decimal::from(10_000) - Decimal::from(7))
                    .round_dp(2),
                max_notional_usd: Decimal::from(25_000),
                funding_rate: Some(funding_rate),
                observed_at: now,
                expires_at: now + ChronoDuration::seconds(4),
            });
        }

        opportunities.sort_by(|a, b| b.net_edge_bps.cmp(&a.net_edge_bps));

        state.quotes = quotes;
        state.opportunities = opportunities;
    }
}

/// 确定性伪随机抖动，返回 -150..=150 的整数（约 ±1.5 bps 的价格噪声）。
fn jitter(seed: u64) -> i64 {
    let value = (seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407)
        >> 33)
        % 301;
    value as i64 - 150
}
