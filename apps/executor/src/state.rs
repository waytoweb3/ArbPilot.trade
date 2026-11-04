use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Instant,
};

use arb_core::{OpportunityKind, Order, Position, StrategyRecord, StrategyStatus};
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    config::Config,
    events::{AlertLevel, ExecutorEvent},
    exchange::MockExchange,
    market::MarketFeed,
    store::Store,
};

/// 执行器共享状态。所有写操作通过锁保护；副作用操作必须先写幂等表。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub market: Arc<MarketFeed>,
    pub exchange: Arc<MockExchange>,
    pub orders: Arc<RwLock<Vec<Order>>>,
    pub positions: Arc<RwLock<Vec<Position>>>,
    pub strategies: Arc<RwLock<Vec<StrategyRecord>>>,
    pub idempotency: Arc<RwLock<HashMap<String, Uuid>>>,
    pub events: broadcast::Sender<ExecutorEvent>,
    pub paused: Arc<AtomicBool>,
    pub started_at: Instant,
}

impl AppState {
    pub fn new(config: Config, store: Store) -> Self {
        let (events, _) = broadcast::channel(512);
        let market = MarketFeed::start(events.clone());
        let now = Utc::now();

        let strategies = vec![
            StrategyRecord {
                id: Uuid::new_v4(),
                name: "资金费率基差（模拟）".to_string(),
                kind: OpportunityKind::FundingRate,
                status: StrategyStatus::Paused,
                venues: vec!["binance".into(), "bybit".into()],
                symbols: vec!["BTCUSDT".into()],
                last_heartbeat_at: Some(now),
                updated_at: now,
            },
            StrategyRecord {
                id: Uuid::new_v4(),
                name: "BTC 跨所价差（模拟）".to_string(),
                kind: OpportunityKind::CexCexSpread,
                status: StrategyStatus::Running,
                venues: vec!["binance".into(), "okx".into()],
                symbols: vec!["BTCUSDT".into()],
                last_heartbeat_at: Some(now),
                updated_at: now,
            },
            StrategyRecord {
                id: Uuid::new_v4(),
                name: "ETH 跨所价差（模拟）".to_string(),
                kind: OpportunityKind::CexCexSpread,
                status: StrategyStatus::Draft,
                venues: vec!["okx".into(), "bybit".into()],
                symbols: vec!["ETHUSDT".into()],
                last_heartbeat_at: None,
                updated_at: now,
            },
        ];

        Self {
            config: Arc::new(config),
            store: Arc::new(store),
            market,
            exchange: Arc::new(MockExchange::new()),
            orders: Arc::new(RwLock::new(Vec::new())),
            positions: Arc::new(RwLock::new(Vec::new())),
            strategies: Arc::new(RwLock::new(strategies)),
            idempotency: Arc::new(RwLock::new(HashMap::new())),
            events,
            paused: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 暂停只阻止新开仓，查看仓位、撤单与风险处置始终可用。
    pub fn set_paused(&self, paused: bool, reason: Option<&str>) {
        self.paused.store(paused, Ordering::Relaxed);
        let message = match (paused, reason) {
            (true, Some(reason)) => format!("执行器已暂停开仓: {reason}"),
            (true, None) => "执行器已暂停开仓".to_string(),
            (false, _) => "执行器已恢复开仓".to_string(),
        };
        let _ = self.events.send(ExecutorEvent::Alert {
            level: if paused {
                AlertLevel::Warning
            } else {
                AlertLevel::Info
            },
            message,
            at: Utc::now(),
        });
        let _ = self.events.send(self.status_event());
    }

    pub fn open_notional_usd(&self) -> Decimal {
        self.positions
            .read()
            .expect("仓位锁不可用")
            .iter()
            .map(Position::notional_usd)
            .sum()
    }

    pub fn emit_order(&self, order: &Order) {
        let _ = self.events.send(ExecutorEvent::OrderUpdated {
            order: order.clone(),
        });
    }

    pub fn status_event(&self) -> ExecutorEvent {
        ExecutorEvent::ExecutorStatus {
            paused: self.is_paused(),
            mode: self.config.mode,
            uptime_secs: self.uptime_secs(),
            server_time: Utc::now(),
        }
    }
}
