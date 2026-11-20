use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Months, Utc};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{
    config::Config,
    exchange::{
        BinanceFundingClient, BitgetFundingClient, BybitFundingClient, FundingRate,
        FundingSnapshot, FundingVenueClient, GateFundingClient, HyperliquidFundingClient,
        LighterFundingClient, OkxFundingClient, ASTER_VENUE, BITGET_VENUE, BYBIT_VENUE, GATE_VENUE,
        HYPERLIQUID_VENUE, LIGHTER_VENUE, OKX_VENUE, VENUE,
    },
    http_client::HttpClient,
    realtime::RealtimeFeed,
    store::Store,
};

/// 尚未观测到结算间隔时的默认值（两家交易所的 BTC/ETH 永续多为 8 小时）。
const DEFAULT_INTERVAL_SECS: i64 = 8 * 3600;
/// 交易对清单刷新间隔，用于跟进交易所上新与下架。
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 每轮最多回补多少个交易对的历史，控制交易所限频。
const HISTORY_SYNC_BUDGET: usize = 160;
/// 没有批量接口时，每轮最多用 REST 刷新多少个交易对的当期费率。
const SNAPSHOT_SYNC_BUDGET: usize = 64;
/// 无 WebSocket 推送时，当期费率走 REST 的最小刷新间隔（轮询间隔的倍数）。
const SNAPSHOT_TTL_FACTOR: u32 = 4;
/// 单分块内串行请求，分块之间并行；用于压低单交易所的瞬时请求数。
const REST_CONCURRENCY: usize = 6;
/// 资金费率历史的保留窗口：只保留最近 1 个月，更早的数据在入库时丢弃并定期清理。
const FUNDING_RETENTION_MONTHS: u32 = 1;
/// 超期历史清理间隔：保留窗口按月计，每天清理一次即可。
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// 保留窗口的起始时间，早于该时间的结算费率视为过期。
fn retention_cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
    now.checked_sub_months(Months::new(FUNDING_RETENTION_MONTHS))
        .unwrap_or_else(|| now - chrono::Duration::days(30))
}

/// 按实际结算间隔折算年化百分比（百分比数值，例如 10.95 表示 10.95%）。
///
/// 各交易所结算周期不同（Hyperliquid 1 小时、多数 CEX 8 小时），
/// 跨交易所比较费率前必须先用本函数折算到同一口径。
pub(crate) fn annualize(rate: Decimal, interval_secs: i64) -> Decimal {
    let periods = Decimal::from(365 * 24 * 3600) / Decimal::from(interval_secs.max(1));
    rate * periods * Decimal::from(100)
}

/// 相邻结算时间中出现次数最多的间隔，用作该合约的实际结算周期。
fn dominant_interval(times: &[DateTime<Utc>]) -> Option<i64> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for pair in times.windows(2) {
        let diff = (pair[1] - pair[0]).num_seconds();
        if diff > 0 && diff <= 24 * 3600 {
            *counts.entry(diff).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(seconds, count)| (*count, *seconds))
        .map(|(seconds, _)| seconds)
}

/// 从历史结算时间推断该合约的结算间隔：取出现次数最多的相邻间隔。
fn observed_interval_secs(previous: Option<DateTime<Utc>>, history: &[FundingRate]) -> Option<i64> {
    let mut times: Vec<DateTime<Utc>> = Vec::with_capacity(history.len() + 1);
    times.extend(previous);
    times.extend(history.iter().map(|row| row.funding_time));
    dominant_interval(&times)
}

/// 单个交易所的资金费率采集状态。
#[derive(Debug, Clone, Serialize)]
pub struct FundingStatus {
    pub enabled: bool,
    pub venue: String,
    pub base_url: String,
    pub poll_secs: u64,
    pub stored_rows: usize,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub symbols: Vec<FundingSymbolStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FundingSymbolStatus {
    pub symbol: String,
    /// 最近一次已结算的费率（来自数据库）。
    pub last_funding_time: Option<DateTime<Utc>>,
    pub last_funding_rate: Option<Decimal>,
    /// 该合约的实际结算间隔（秒），由历史结算时间推断。
    pub funding_interval_secs: i64,
    /// 当期（未结算）费率按实际结算间隔折算的年化百分比；与 `predicted_rate` 同口径。
    pub annualized_pct: Option<Decimal>,
    /// 当期（未结算）预测费率。
    pub predicted_rate: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub next_funding_at: Option<DateTime<Utc>>,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
struct StatusState {
    snapshots: HashMap<String, FundingSnapshot>,
    /// 最近一次已结算费率，按交易对索引；状态接口直接读缓存，避免高频轮询打库。
    latest: HashMap<String, FundingRate>,
    /// 各交易对的结算间隔（秒），来自已入库的相邻结算时间。
    intervals: HashMap<String, i64>,
    stored_rows: usize,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

/// 单个交易所的采集运行时。
struct VenueRuntime {
    venue: &'static str,
    base_url: String,
    client: Option<Arc<dyn FundingVenueClient>>,
    /// 该交易所当前采集的交易对；自动发现时定期整表替换。
    symbols: RwLock<Vec<String>>,
    /// 该交易所最近一次成功发现交易对清单的时间，失败时保持为空以便下轮重试。
    discovered_at: RwLock<Option<Instant>>,
    state: RwLock<StatusState>,
}

/// Binance / OKX / Bybit / Bitget / Gate 资金费率采集器。
///
/// 启动后先拉取交易所可交易的 USDT 本位永续清单（也可用 `ARBPILOT_FUNDING_SYMBOLS`
/// 手动指定），再逐币回填历史与刷新当期费率。当期费率优先取 WebSocket 推送，
/// 有批量接口的交易所（Binance）一次请求覆盖全部合约，其余按预算轮转走 REST。
pub struct FundingFeed {
    enabled: bool,
    /// 手动指定交易对时为真，不再自动发现。
    manual_symbols: bool,
    poll: Duration,
    venues: Vec<VenueRuntime>,
    realtime: Option<Arc<RealtimeFeed>>,
    store: Arc<Store>,
    /// 上次清理超期历史的时间，避免每轮都执行删除。
    last_prune: RwLock<Option<Instant>>,
    /// 是否已完成首轮的结算间隔恢复；顺序：交易所公开间隔 → 已入库历史推断。
    hydrated: AtomicBool,
}

impl FundingFeed {
    pub fn start(
        config: &Config,
        http: Option<Arc<HttpClient>>,
        realtime: Option<Arc<RealtimeFeed>>,
        store: Arc<Store>,
    ) -> Arc<Self> {
        let mut venues: Vec<VenueRuntime> = Vec::new();
        let mut push_venue =
            |venue: &'static str, base_url: &str, client: Option<Arc<dyn FundingVenueClient>>| {
                venues.push(VenueRuntime {
                    venue,
                    base_url: base_url.trim_end_matches('/').to_string(),
                    client,
                    symbols: RwLock::new(config.funding_symbols.clone()),
                    discovered_at: RwLock::new(None),
                    state: RwLock::new(StatusState::default()),
                });
            };

        match &http {
            Some(http) => {
                push_venue(
                    VENUE,
                    &config.binance_fapi_url,
                    Some(Arc::new(BinanceFundingClient::new(
                        &config.binance_fapi_url,
                        http.clone(),
                    ))),
                );
                if config.okx_enabled {
                    push_venue(
                        OKX_VENUE,
                        &config.okx_base_url,
                        Some(Arc::new(OkxFundingClient::new(
                            &config.okx_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.bybit_enabled {
                    push_venue(
                        BYBIT_VENUE,
                        &config.bybit_base_url,
                        Some(Arc::new(BybitFundingClient::new(
                            &config.bybit_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.bitget_enabled {
                    push_venue(
                        BITGET_VENUE,
                        &config.bitget_base_url,
                        Some(Arc::new(BitgetFundingClient::new(
                            &config.bitget_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.gate_enabled {
                    push_venue(
                        GATE_VENUE,
                        &config.gate_base_url,
                        Some(Arc::new(GateFundingClient::new(
                            &config.gate_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.aster_enabled {
                    push_venue(
                        ASTER_VENUE,
                        &config.aster_base_url,
                        Some(Arc::new(BinanceFundingClient::with_venue(
                            ASTER_VENUE,
                            &config.aster_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.hyperliquid_enabled {
                    push_venue(
                        HYPERLIQUID_VENUE,
                        &config.hyperliquid_base_url,
                        Some(Arc::new(HyperliquidFundingClient::new(
                            &config.hyperliquid_base_url,
                            http.clone(),
                        ))),
                    );
                }
                if config.lighter_enabled {
                    push_venue(
                        LIGHTER_VENUE,
                        &config.lighter_base_url,
                        Some(Arc::new(LighterFundingClient::new(
                            &config.lighter_base_url,
                            http.clone(),
                        ))),
                    );
                }
            }
            None => {
                tracing::error!("HTTP 客户端不可用，资金费率采集不会启动");
                push_venue(VENUE, &config.binance_fapi_url, None);
                if config.okx_enabled {
                    push_venue(OKX_VENUE, &config.okx_base_url, None);
                }
                if config.bybit_enabled {
                    push_venue(BYBIT_VENUE, &config.bybit_base_url, None);
                }
                if config.bitget_enabled {
                    push_venue(BITGET_VENUE, &config.bitget_base_url, None);
                }
                if config.gate_enabled {
                    push_venue(GATE_VENUE, &config.gate_base_url, None);
                }
                if config.aster_enabled {
                    push_venue(ASTER_VENUE, &config.aster_base_url, None);
                }
                if config.hyperliquid_enabled {
                    push_venue(HYPERLIQUID_VENUE, &config.hyperliquid_base_url, None);
                }
                if config.lighter_enabled {
                    push_venue(LIGHTER_VENUE, &config.lighter_base_url, None);
                }
            }
        }

        let manual_symbols = !config.funding_symbols.is_empty();
        let enabled = config.funding_enabled && venues.iter().any(|venue| venue.client.is_some());

        let feed = Arc::new(Self {
            enabled,
            manual_symbols,
            poll: Duration::from_secs(config.funding_poll_secs.max(15)),
            venues,
            realtime,
            store,
            last_prune: RwLock::new(None),
            hydrated: AtomicBool::new(false),
        });

        if !enabled {
            tracing::info!("资金费率采集已停用（检查 ARBPILOT_FUNDING_* 与各交易所开关）");
            return feed;
        }

        tracing::info!(
            venues = ?feed.venues.iter().map(|venue| venue.venue).collect::<Vec<_>>(),
            manual_symbols = ?(!feed.manual_symbols).then(|| "自动发现全部 USDT 本位永续"),
            poll_secs = feed.poll.as_secs(),
            websocket = feed.realtime.is_some(),
            proxy = ?config.market_proxy,
            "资金费率采集已启动"
        );

        // 手动模式不会触发自动发现，这里先把配置里的清单交给 WebSocket 订阅。
        if manual_symbols {
            if let Some(realtime) = &feed.realtime {
                if feed.venues.iter().any(|venue| venue.venue == OKX_VENUE) {
                    realtime
                        .funding_symbols()
                        .set(config.funding_symbols.clone());
                }
            }
        }

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(worker.poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                worker.sync_once().await;
            }
        });

        feed
    }

    /// 每个交易所一份状态，供桌面端切换查看。
    ///
    /// 全部数据来自采集运行时缓存，接口本身不再查库，允许桌面端高频轮询。
    pub async fn status(&self) -> Vec<FundingStatus> {
        let mut statuses = Vec::with_capacity(self.venues.len());
        for venue in &self.venues {
            let symbols = venue
                .symbols
                .read()
                .expect("资金费率交易对锁不可用")
                .clone();

            let state = venue.state.read().expect("资金费率状态锁不可用");
            let mut entries = Vec::with_capacity(symbols.len());
            for symbol in &symbols {
                let row = state.latest.get(symbol);
                let snapshot = state.snapshots.get(symbol);
                // 刚发现还没拿到数据的合约先不展示，避免整页空行。
                if row.is_none() && snapshot.is_none() {
                    continue;
                }
                let rate = row.map(|row| row.funding_rate);
                let predicted = snapshot.map(|snapshot| snapshot.funding_rate);
                let interval_secs = state
                    .intervals
                    .get(symbol)
                    .copied()
                    .or_else(|| {
                        venue
                            .client
                            .as_ref()
                            .and_then(|client| client.funding_interval_secs())
                    })
                    .unwrap_or(DEFAULT_INTERVAL_SECS);
                entries.push(FundingSymbolStatus {
                    symbol: symbol.clone(),
                    last_funding_time: row.map(|row| row.funding_time),
                    last_funding_rate: rate,
                    funding_interval_secs: interval_secs,
                    // 与展示的当期费率保持同一口径；没有当期费率时回退最近一次已结算费率。
                    annualized_pct: predicted
                        .or(rate)
                        .map(|rate| annualize(rate, interval_secs)),
                    predicted_rate: predicted,
                    mark_price: snapshot
                        .and_then(|snapshot| snapshot.mark_price)
                        .or_else(|| row.and_then(|row| row.mark_price)),
                    next_funding_at: snapshot.and_then(|snapshot| snapshot.next_funding_at),
                    observed_at: snapshot.map(|snapshot| snapshot.observed_at),
                });
            }

            statuses.push(FundingStatus {
                enabled: self.enabled && venue.client.is_some(),
                venue: venue.venue.to_string(),
                base_url: venue.base_url.clone(),
                poll_secs: self.poll.as_secs(),
                stored_rows: state.stored_rows,
                last_attempt_at: state.last_attempt_at,
                last_success_at: state.last_success_at,
                last_error: state.last_error.clone(),
                symbols: entries,
            });
        }
        statuses
    }

    /// 各交易所各交易对的结算间隔（秒），供行情侧把费率归一到同一口径。
    pub fn intervals(&self) -> HashMap<(String, String), i64> {
        let mut intervals = HashMap::new();
        for venue in &self.venues {
            let symbols = venue
                .symbols
                .read()
                .expect("资金费率交易对锁不可用")
                .clone();
            let state = venue.state.read().expect("资金费率状态锁不可用");
            let fallback = venue
                .client
                .as_ref()
                .and_then(|client| client.funding_interval_secs())
                .unwrap_or(DEFAULT_INTERVAL_SECS);
            for symbol in &symbols {
                let secs = state.intervals.get(symbol).copied().unwrap_or(fallback);
                intervals.insert((venue.venue.to_string(), symbol.clone()), secs);
            }
        }
        intervals
    }

    /// 全部启用交易所已发现的合约并集，行情侧据此把盘口与机会扩到全市场。
    pub fn traded_symbols(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut symbols = Vec::new();
        for venue in &self.venues {
            if venue.client.is_none() {
                continue;
            }
            for symbol in venue
                .symbols
                .read()
                .expect("资金费率交易对锁不可用")
                .iter()
            {
                if seen.insert(symbol.clone()) {
                    symbols.push(symbol.clone());
                }
            }
        }
        symbols.sort();
        symbols
    }

    /// 每个启用交易所自己的合约清单，供行情侧按交易所增量订阅 WebSocket。
    pub fn venue_symbols(&self) -> Vec<(String, Vec<String>)> {
        self.venues
            .iter()
            .filter(|venue| venue.client.is_some())
            .map(|venue| {
                let mut symbols = venue
                    .symbols
                    .read()
                    .expect("资金费率交易对锁不可用")
                    .clone();
                symbols.sort();
                (venue.venue.to_string(), symbols)
            })
            .collect()
    }

    async fn sync_once(self: &Arc<Self>) {
        let now = Utc::now();
        self.prune_history().await;
        for venue in &self.venues {
            if self.discovery_due(venue) {
                self.discover_venue(venue).await;
            }
        }

        // 重启后先恢复结算间隔：交易所公开值优先，缺失的再用已入库历史推断。
        if !self.hydrated.swap(true, Ordering::Relaxed) {
            for venue in &self.venues {
                self.seed_native_intervals(venue).await;
            }
            self.hydrate_intervals().await;
        }

        for venue in &self.venues {
            venue
                .state
                .write()
                .expect("资金费率状态锁不可用")
                .last_attempt_at = Some(now);
        }

        for venue in &self.venues {
            let Some(client) = venue.client.clone() else {
                continue;
            };
            let symbols = venue
                .symbols
                .read()
                .expect("资金费率交易对锁不可用")
                .clone();
            if symbols.is_empty() {
                continue;
            }

            let mut errors = Vec::new();
            self.sync_snapshots(venue, client.clone(), &symbols, &mut errors)
                .await;
            let latest = match self.store.funding_latest(venue.venue, &symbols).await {
                Ok(rows) => rows,
                Err(error) => {
                    errors.push(format!("读取已入库历史失败: {error}"));
                    Vec::new()
                }
            };
            {
                // 状态接口直接读这份缓存，避免每次轮询都打库。
                let mut state = venue.state.write().expect("资金费率状态锁不可用");
                state.latest = latest
                    .iter()
                    .map(|row| (row.symbol.clone(), row.clone()))
                    .collect();
            }
            self.sync_histories(venue, client, &symbols, &latest, &mut errors)
                .await;

            match self.store.funding_count(venue.venue).await {
                Ok(count) => {
                    venue
                        .state
                        .write()
                        .expect("资金费率状态锁不可用")
                        .stored_rows = count;
                }
                Err(error) => errors.push(format!("统计入库条数失败: {error}")),
            }

            let mut state = venue.state.write().expect("资金费率状态锁不可用");
            if errors.is_empty() {
                state.last_success_at = Some(Utc::now());
                state.last_error = None;
            } else {
                let message = errors.join("; ");
                tracing::warn!(venue = venue.venue, %message, "资金费率采集部分失败");
                // 已有可用数据时（例如部分交易对触发限频）不在页面上报错，只在日志里保留详情。
                if state.snapshots.is_empty() && state.latest.is_empty() {
                    state.last_error = Some(message);
                } else {
                    state.last_success_at = Some(Utc::now());
                    state.last_error = None;
                }
            }
        }
    }

    /// 删除超出保留窗口的资金费率历史，每天最多执行一次。
    async fn prune_history(&self) {
        let due = self
            .last_prune
            .read()
            .expect("资金费率清理时间锁不可用")
            .map(|at| at.elapsed() >= PRUNE_INTERVAL)
            .unwrap_or(true);
        if !due {
            return;
        }
        // 先记录时间：清理失败时也等下一轮，避免每轮轮询都打库。
        *self.last_prune.write().expect("资金费率清理时间锁不可用") = Some(Instant::now());

        let cutoff = retention_cutoff(Utc::now());
        match self.store.prune_funding_rates(cutoff).await {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, %cutoff, "已清理超期的资金费率历史"),
            Err(error) => tracing::warn!(%error, "清理超期的资金费率历史失败"),
        }
    }

    fn discovery_due(&self, venue: &VenueRuntime) -> bool {
        if self.manual_symbols || venue.client.is_none() {
            return false;
        }
        venue
            .discovered_at
            .read()
            .expect("交易对发现时间锁不可用")
            .map(|at| at.elapsed() >= DISCOVERY_INTERVAL)
            .unwrap_or(true)
    }

    /// 从单个交易所拉取可交易的 USDT 本位永续清单；失败时保留原清单并下轮重试。
    async fn discover_venue(&self, venue: &VenueRuntime) {
        let Some(client) = venue.client.clone() else {
            return;
        };
        let result = tokio::task::spawn_blocking(move || client.listed_symbols()).await;
        match result {
            Ok(Ok(mut symbols)) => {
                if symbols.is_empty() {
                    tracing::warn!(venue = venue.venue, "交易所未返回可交易永续，保留现有清单");
                    return;
                }
                symbols.sort();
                let (added, count, snapshot) = {
                    let mut current = venue.symbols.write().expect("资金费率交易对锁不可用");
                    let existing: HashSet<&String> = current.iter().collect();
                    let added = symbols
                        .iter()
                        .filter(|item| !existing.contains(item))
                        .count();
                    let count = symbols.len();
                    let snapshot = symbols.clone();
                    *current = symbols;
                    (added, count, snapshot)
                };
                // 把交易所实际可交易的合约同步到数据库，供策略范围下拉使用。
                self.store
                    .upsert_tradable_symbols(venue.venue, &snapshot)
                    .await;
                // OKX 的费率走 WebSocket 全量订阅，新上市的合约需要增量补订阅。
                if venue.venue == OKX_VENUE {
                    if let Some(realtime) = &self.realtime {
                        realtime.funding_symbols().set(snapshot);
                    }
                }
                *venue.discovered_at.write().expect("交易对发现时间锁不可用") =
                    Some(Instant::now());
                tracing::info!(
                    venue = venue.venue,
                    count,
                    added,
                    "资金费率交易对清单已更新"
                );
            }
            Ok(Err(error)) => {
                tracing::warn!(venue = venue.venue, %error, "拉取交易对清单失败，保留现有清单")
            }
            Err(error) => {
                tracing::error!(venue = venue.venue, %error, "交易对发现任务异常退出")
            }
        }
    }

    /// 用交易所公开的逐合约结算间隔填充缓存；结果只用于启动时的初始值。
    async fn seed_native_intervals(&self, venue: &VenueRuntime) {
        let Some(client) = venue.client.clone() else {
            return;
        };
        let result = tokio::task::spawn_blocking(move || client.funding_intervals()).await;
        match result {
            Ok(Ok(intervals)) if !intervals.is_empty() => {
                let mut state = venue.state.write().expect("资金费率状态锁不可用");
                let mut applied = 0;
                for (symbol, secs) in intervals {
                    if secs > 0 {
                        state.intervals.insert(symbol, secs);
                        applied += 1;
                    }
                }
                tracing::info!(venue = venue.venue, applied, "已同步交易所公开的结算间隔");
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(venue = venue.venue, %error, "读取交易所结算间隔失败，改用历史推断")
            }
            Err(error) => tracing::warn!(venue = venue.venue, %error, "结算间隔任务异常退出"),
        }
    }

    /// 从最近两天的已入库结算时间恢复结算间隔，覆盖交易所未公开间隔的合约。
    async fn hydrate_intervals(&self) {
        let since = Utc::now() - chrono::Duration::days(2);
        for venue in &self.venues {
            let rows = match self.store.funding_settlements(venue.venue, since).await {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(venue = venue.venue, %error, "读取历史结算时间失败");
                    continue;
                }
            };
            if rows.is_empty() {
                continue;
            }

            let mut grouped: HashMap<String, Vec<DateTime<Utc>>> = HashMap::new();
            for (symbol, time) in rows {
                grouped.entry(symbol).or_default().push(time);
            }

            let mut restored = 0;
            let mut state = venue.state.write().expect("资金费率状态锁不可用");
            for (symbol, times) in grouped {
                if state.intervals.contains_key(&symbol) {
                    continue;
                }
                if let Some(secs) = dominant_interval(&times) {
                    state.intervals.insert(symbol, secs);
                    restored += 1;
                }
            }
            tracing::info!(venue = venue.venue, restored, "已从历史恢复结算间隔");
        }
    }

    /// 当期（未结算）费率：批量接口优先，其次 WebSocket，最后按预算逐币 REST。
    async fn sync_snapshots(
        &self,
        venue: &VenueRuntime,
        client: Arc<dyn FundingVenueClient>,
        symbols: &[String],
        errors: &mut Vec<String>,
    ) {
        let bulk = {
            let client = client.clone();
            tokio::task::spawn_blocking(move || client.premium_index_all()).await
        };
        match bulk {
            Ok(Ok(rows)) if !rows.is_empty() => {
                let wanted: HashSet<&str> = symbols.iter().map(String::as_str).collect();
                let mut state = venue.state.write().expect("资金费率状态锁不可用");
                let mut applied = 0;
                for (symbol, snapshot) in rows {
                    if wanted.contains(symbol.as_str()) {
                        state.snapshots.insert(symbol, snapshot);
                        applied += 1;
                    }
                }
                tracing::debug!(venue = venue.venue, applied, "当期费率已批量更新");
                return;
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => errors.push(format!("当期费率批量拉取失败: {error}")),
            Err(error) => errors.push(format!("当期费率批量任务异常退出: {error}")),
        }

        let now = Utc::now();
        let ttl = self.poll * SNAPSHOT_TTL_FACTOR;
        let mut due = Vec::new();
        {
            let mut state = venue.state.write().expect("资金费率状态锁不可用");
            for symbol in symbols {
                if self.insert_ws_snapshot(&mut state, venue.venue, symbol, now, ttl) {
                    continue;
                }
                let fresh = state
                    .snapshots
                    .get(symbol)
                    .map(|snapshot| is_within(now, snapshot.observed_at, ttl))
                    .unwrap_or(false);
                if !fresh {
                    due.push(symbol.clone());
                }
            }
        }

        if due.is_empty() {
            return;
        }
        // 最久未刷新的优先，保证所有合约轮转更新。
        {
            let state = venue.state.read().expect("资金费率状态锁不可用");
            due.sort_by_key(|symbol| state.snapshots.get(symbol).map(|item| item.observed_at));
        }
        due.truncate(SNAPSHOT_SYNC_BUDGET);

        let (snapshots, failures) = fetch_snapshots(client, due).await;
        {
            let mut state = venue.state.write().expect("资金费率状态锁不可用");
            for (symbol, snapshot) in snapshots {
                state.snapshots.insert(symbol, snapshot);
            }
        }
        if !failures.is_empty() {
            errors.push(format_failures("当期费率", &failures));
        }
    }

    /// WebSocket 推送仍然新鲜时直接写入状态，返回是否命中。
    fn insert_ws_snapshot(
        &self,
        state: &mut StatusState,
        venue: &str,
        symbol: &str,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> bool {
        let Some(update) = self
            .realtime
            .as_ref()
            .and_then(|feed| feed.funding(venue, symbol))
        else {
            return false;
        };
        if !is_within(now, update.observed_at, ttl) {
            return false;
        }
        state.snapshots.insert(symbol.to_string(), update.snapshot);
        true
    }

    /// 只回补「该有新的结算但还没入库」的交易对，并按预算轮转。
    async fn sync_histories(
        &self,
        venue: &VenueRuntime,
        client: Arc<dyn FundingVenueClient>,
        symbols: &[String],
        latest: &[FundingRate],
        errors: &mut Vec<String>,
    ) {
        let now = Utc::now().timestamp();
        let latest_by_symbol: HashMap<&str, &FundingRate> = latest
            .iter()
            .map(|row| (row.symbol.as_str(), row))
            .collect();

        let mut due: Vec<(Option<DateTime<Utc>>, String)> = Vec::new();
        {
            let state = venue.state.read().expect("资金费率状态锁不可用");
            for symbol in symbols {
                let row = latest_by_symbol.get(symbol.as_str()).copied();
                let interval = state
                    .intervals
                    .get(symbol)
                    .copied()
                    .or_else(|| client.funding_interval_secs())
                    .unwrap_or(DEFAULT_INTERVAL_SECS);
                let is_due = match row {
                    None => true,
                    Some(row) => {
                        // 交易所已给出下次结算时间时以它为准，避免结算间隔偏大导致漏补。
                        let settled = state
                            .snapshots
                            .get(symbol)
                            .and_then(|snapshot| snapshot.next_funding_at)
                            .map(|at| at.timestamp() <= now)
                            .unwrap_or(false);
                        settled || row.funding_time.timestamp() + interval <= now
                    }
                };
                if is_due {
                    due.push((row.map(|row| row.funding_time), symbol.clone()));
                }
            }
        }
        if due.is_empty() {
            return;
        }

        // 从未回补的排在最前，其次是最久没有新结算的。
        due.sort_by(|left, right| left.0.cmp(&right.0));
        due.truncate(HISTORY_SYNC_BUDGET);

        let (histories, failures) = fetch_histories(client, due).await;
        // 只保留保留窗口内的结算记录，避免把交易所返回的整段历史反复写库。
        let cutoff = retention_cutoff(Utc::now());
        let histories: Vec<(String, Vec<FundingRate>)> = histories
            .into_iter()
            .map(|(symbol, mut history)| {
                history.retain(|row| row.funding_time >= cutoff);
                (symbol, history)
            })
            .collect();
        {
            let mut state = venue.state.write().expect("资金费率状态锁不可用");
            for (symbol, history) in &histories {
                let start = latest_by_symbol
                    .get(symbol.as_str())
                    .map(|row| row.funding_time);
                if let Some(interval_secs) = observed_interval_secs(start, history) {
                    state.intervals.insert(symbol.clone(), interval_secs);
                }
            }
        }

        let mut written = 0;
        for (symbol, history) in histories {
            match self.store.upsert_funding_rates(&history).await {
                Ok(count) => written += count,
                Err(error) => errors.push(format!("{symbol} 历史入库失败: {error}")),
            }
        }
        if written > 0 {
            tracing::info!(venue = venue.venue, written, "资金费率已入库");
        }
        if !failures.is_empty() {
            errors.push(format_failures("历史费率", &failures));
        }
    }
}

/// 时间戳是否落在允许窗口内（同时排除时钟回退造成的负值）。
fn is_within(now: DateTime<Utc>, observed_at: DateTime<Utc>, ttl: Duration) -> bool {
    let age = now.signed_duration_since(observed_at).num_milliseconds();
    age >= 0 && age as u128 <= ttl.as_millis()
}

/// 逐币并发拉取当期费率（分块内串行，避免瞬时打满交易所限频）。
async fn fetch_snapshots(
    client: Arc<dyn FundingVenueClient>,
    symbols: Vec<String>,
) -> (Vec<(String, FundingSnapshot)>, Vec<(String, String)>) {
    let mut handles = Vec::new();
    for chunk in symbols.chunks(REST_CONCURRENCY) {
        let client = client.clone();
        let chunk = chunk.to_vec();
        handles.push(tokio::task::spawn_blocking(move || {
            let mut values = Vec::new();
            let mut errors = Vec::new();
            for symbol in chunk {
                match client.premium_index(&symbol) {
                    Ok(snapshot) => values.push((symbol, snapshot)),
                    Err(error) => errors.push((symbol, error.to_string())),
                }
            }
            (values, errors)
        }));
    }

    let mut values = Vec::new();
    let mut errors = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((chunk_values, chunk_errors)) => {
                values.extend(chunk_values);
                errors.extend(chunk_errors);
            }
            Err(error) => errors.push(("unknown".to_string(), error.to_string())),
        }
    }
    (values, errors)
}

/// 逐币并发拉取历史费率。
async fn fetch_histories(
    client: Arc<dyn FundingVenueClient>,
    due: Vec<(Option<DateTime<Utc>>, String)>,
) -> (Vec<(String, Vec<FundingRate>)>, Vec<(String, String)>) {
    let mut handles = Vec::new();
    for chunk in due.chunks(REST_CONCURRENCY) {
        let client = client.clone();
        let chunk = chunk.to_vec();
        handles.push(tokio::task::spawn_blocking(move || {
            let mut values = Vec::new();
            let mut errors = Vec::new();
            for (start, symbol) in chunk {
                match client.funding_history(&symbol, start) {
                    Ok(history) => values.push((symbol, history)),
                    Err(error) => errors.push((symbol, error.to_string())),
                }
            }
            (values, errors)
        }));
    }

    let mut values = Vec::new();
    let mut errors = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((chunk_values, chunk_errors)) => {
                values.extend(chunk_values);
                errors.extend(chunk_errors);
            }
            Err(error) => errors.push(("unknown".to_string(), error.to_string())),
        }
    }
    (values, errors)
}

/// 失败信息可能成百上千条，只保留少量样本避免撑爆状态接口。
fn format_failures(label: &str, failures: &[(String, String)]) -> String {
    let sample = failures
        .iter()
        .take(3)
        .map(|(symbol, error)| format!("{symbol}: {error}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{label} {} 个交易对失败（{sample}）", failures.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    #[test]
    fn annualizes_eight_hour_rate() {
        // 单期 0.01%，8 小时结算 → 0.01% * 3 * 365 = 10.95%
        assert_eq!(
            annualize(dec!(0.0001), DEFAULT_INTERVAL_SECS).round_dp(4),
            dec!(10.95)
        );
    }

    #[test]
    fn annualizes_four_hour_rate() {
        // 4 小时结算：单期 0.01% → 0.01% * 6 * 365 = 21.9%
        assert_eq!(annualize(dec!(0.0001), 4 * 3600).round_dp(4), dec!(21.9));
    }

    #[test]
    fn retention_window_is_one_month() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 0, 0, 0).unwrap();
        assert_eq!(
            retention_cutoff(now),
            Utc.with_ymd_and_hms(2026, 10, 15, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn infers_interval_from_settlement_times() {
        let history = vec![
            FundingRate {
                venue: VENUE.to_string(),
                symbol: "BTCUSDT".to_string(),
                funding_time: Utc.timestamp_opt(1_700_006_400, 0).unwrap(),
                funding_rate: dec!(0.0001),
                mark_price: None,
            },
            FundingRate {
                venue: VENUE.to_string(),
                symbol: "BTCUSDT".to_string(),
                funding_time: Utc.timestamp_opt(1_700_020_800, 0).unwrap(),
                funding_rate: dec!(0.0001),
                mark_price: None,
            },
        ];

        assert_eq!(observed_interval_secs(None, &history), Some(4 * 3600));
        assert_eq!(
            observed_interval_secs(
                Some(Utc.timestamp_opt(1_700_006_400 - 4 * 3600, 0).unwrap()),
                &history
            ),
            Some(4 * 3600)
        );
    }
}
