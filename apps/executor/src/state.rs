use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use arb_core::{
    CostAssumptions, EntryRules, ExecutionRules, ExitRules, Instrument, OpportunityKind, Order,
    Position, RiskLimits, StrategyDefinition, StrategyExecutionMode, StrategyRecord, StrategyRisk,
    StrategySource, StrategyStatus, StrategyUniverse,
};
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    accounts::{AccountEndpoints, Accounts},
    config::Config,
    events::{AlertLevel, ExecutorEvent},
    exchange::{LiveEndpoints, LiveExchange, OkxMarketClient, OKX_VENUE, VENUE},
    funding::FundingFeed,
    http_client::HttpClient,
    market::{MarketFeed, OKX_SPOT_VENUE, SPOT_VENUE},
    news::NewsFeed,
    realtime::RealtimeFeed,
    store::Store,
    telegram::TelegramBot,
};

/// 共享 HTTP 客户端的请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 执行器共享状态。所有写操作通过锁保护；副作用操作必须先写幂等表。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub market: Arc<MarketFeed>,
    pub funding: Arc<FundingFeed>,
    pub news: Arc<NewsFeed>,
    pub telegram: Arc<TelegramBot>,
    pub accounts: Arc<Accounts>,
    pub exchange: Arc<LiveExchange>,
    pub orders: Arc<RwLock<Vec<Order>>>,
    pub positions: Arc<RwLock<Vec<Position>>>,
    pub strategies: Arc<RwLock<Vec<StrategyRecord>>>,
    pub idempotency: Arc<RwLock<HashMap<String, Uuid>>>,
    pub events: broadcast::Sender<ExecutorEvent>,
    pub paused: Arc<AtomicBool>,
    /// 可在运行期编辑的风控限额，写入后立即对后续订单生效。
    pub risk_limits: Arc<RwLock<RiskLimits>>,
    pub started_at: Instant,
}

impl AppState {
    pub async fn new(config: Config, store: Store) -> Self {
        let (events, _) = broadcast::channel(512);
        let config = Arc::new(config);
        let store = Arc::new(store);

        // 行情、资金费率与 WebSocket 共用一套 TLS / 代理配置。
        let http = match HttpClient::new(REQUEST_TIMEOUT, config.market_proxy.clone()) {
            Ok(http) => Some(Arc::new(http)),
            Err(error) => {
                tracing::error!(%error, "初始化共享 HTTP 客户端失败，行情与资金费率采集不可用");
                None
            }
        };
        let okx_client = http
            .as_ref()
            .filter(|_| config.okx_enabled)
            .map(|http| Arc::new(OkxMarketClient::new(&config.okx_base_url, http.clone())));
        // 实时推送先启动，REST 在其数据缺失或过期时兜底。
        let realtime = http
            .as_ref()
            .map(|http| RealtimeFeed::start(config.as_ref(), http.clone(), okx_client));

        let market = MarketFeed::start(
            config.as_ref(),
            http.clone(),
            realtime.clone(),
            store.clone(),
            events.clone(),
        );
        let news = NewsFeed::start(config.as_ref(), http.clone());
        let telegram = TelegramBot::start(config.as_ref(), http.clone());
        // 账户余额采集与行情共用同一套 TLS / 代理配置。
        let account_endpoints = AccountEndpoints {
            binance_spot: config.binance_spot_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            gate: config.gate_base_url.clone(),
            aster: config.aster_base_url.clone(),
            htx: config.htx_base_url.clone(),
            mexc: config.mexc_base_url.clone(),
            bingx: config.bingx_base_url.clone(),
            hyperliquid: config.hyperliquid_base_url.clone(),
            lighter: config.lighter_base_url.clone(),
        };
        // 实盘下单走各交易所的私有合约接口，复用已导入账户的凭证。
        let live_endpoints = LiveEndpoints {
            binance: config.binance_fapi_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            gate: config.gate_base_url.clone(),
            aster: config.aster_base_url.clone(),
            htx: config.htx_base_url.clone(),
            mexc: config.mexc_base_url.clone(),
            bingx: config.bingx_base_url.clone(),
        };
        let accounts = Accounts::start(
            http.clone(),
            account_endpoints,
            config.accounts_file.clone(),
            config.accounts_poll_secs,
            store.clone(),
        );
        let exchange = Arc::new(LiveExchange::new(
            http.clone(),
            live_endpoints,
            accounts.clone(),
        ));
        let funding = FundingFeed::start(config.as_ref(), http, realtime, store.clone());

        // 策略定义从存储加载；首次启动时按当前配置生成内置资金费率策略。
        let mut strategies = store.load_strategies().await;
        if strategies.is_empty() {
            strategies = default_strategies(&config);
            for record in &strategies {
                store.save_strategy(record).await;
                store.save_strategy_version(record).await;
            }
        }

        let risk_limits = config.risk.clone();
        let state = Self {
            config,
            store,
            market,
            funding,
            news,
            telegram,
            accounts,
            exchange,
            orders: Arc::new(RwLock::new(Vec::new())),
            positions: Arc::new(RwLock::new(Vec::new())),
            strategies: Arc::new(RwLock::new(strategies)),
            idempotency: Arc::new(RwLock::new(HashMap::new())),
            events,
            paused: Arc::new(AtomicBool::new(false)),
            risk_limits: Arc::new(RwLock::new(risk_limits)),
            started_at: Instant::now(),
        };
        // 机器人需要读取执行器状态，因此在 AppState 构造完成后再挂载轮询。
        state.telegram.clone().attach(state.clone());
        state
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 当前生效的风控限额，设置页修改后立即反映在这里。
    pub fn risk_limits(&self) -> RiskLimits {
        self.risk_limits.read().expect("风控限额锁不可用").clone()
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
            uptime_secs: self.uptime_secs(),
            server_time: Utc::now(),
        }
    }
}

/// 首次启动时的内置策略：每个可用交易所一条「现货多 + 永续空」资金费率策略。
///
/// 这些策略只是可直接运行的初始定义，用户可复制后修改并重新校验、回测。
fn default_strategies(config: &Config) -> Vec<StrategyRecord> {
    let status = if config.funding_enabled {
        StrategyStatus::Running
    } else {
        StrategyStatus::Paused
    };
    let mut records = vec![builtin_funding_record(
        config,
        status,
        SPOT_VENUE,
        VENUE,
        "Binance Spot/Perp",
    )];
    if config.okx_enabled {
        records.push(builtin_funding_record(
            config,
            status,
            OKX_SPOT_VENUE,
            OKX_VENUE,
            "OKX Spot/Swap",
        ));
    }
    records
}

fn builtin_funding_record(
    config: &Config,
    status: StrategyStatus,
    spot_venue: &str,
    perp_venue: &str,
    label: &str,
) -> StrategyRecord {
    let definition = StrategyDefinition {
        name: format!("Funding Basis · {label}"),
        kind: OpportunityKind::FundingRate,
        universe: StrategyUniverse {
            venues: vec![spot_venue.to_string(), perp_venue.to_string()],
            symbols: config.market_symbols.clone(),
            instruments: vec![Instrument::Spot, Instrument::Perp],
        },
        entry: EntryRules {
            funding_spread_min: Decimal::new(3, 4),
            net_edge_min: Decimal::new(1, 4),
        },
        exit: ExitRules {
            spread_below: Decimal::new(2, 4),
            max_holding_hours: Decimal::from(24),
        },
        risk: StrategyRisk {
            max_notional_usd: config.risk.max_order_notional_usd.min(Decimal::from(1_000)),
            max_unhedged_seconds: 3,
        },
        execution: ExecutionRules {
            mode: StrategyExecutionMode::Live,
            leverage: Decimal::ONE,
            max_slippage_bps: Decimal::from(10),
            cooldown_secs: 300,
        },
        costs: CostAssumptions::default(),
        notes: Some(
            "现货多 + 永续空，收取正资金费率；费率低于退出阈值或超过最长持仓时间后平仓。"
                .to_string(),
        ),
    };

    let mut record = StrategyRecord::new(definition, StrategySource::BuiltIn, status);
    record.validation = record.definition.validate(&config.risk);
    record
}
