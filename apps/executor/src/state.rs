use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use arb_core::{Order, OrderStatus, Position, RiskLimits, Side, StrategyRecord};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    accounts::{AccountEndpoints, Accounts, VenuePositions},
    conditional::ConditionalOrders,
    config::{Config, FeeConfig},
    events::{AlertLevel, ExecutorEvent},
    exchange::{LiveEndpoints, LiveExchange, OkxMarketClient},
    funding::FundingFeed,
    gateway::MAX_IN_MEMORY_ORDERS,
    http_client::HttpClient,
    license::LicenseManager,
    listing::ListingDesk,
    market::MarketFeed,
    news::NewsFeed,
    notifier::Notifier,
    onchain::ChainFeed,
    realtime::RealtimeFeed,
    skill::SkillDesk,
    store::{PositionGroupMember, Store},
    telegram::TelegramBot,
};

/// 共享 HTTP 客户端的请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 交易所真实持仓的刷新间隔；只用于发现开/平仓与数量变化，
/// 价格与浮动盈亏由 WebSocket 推送实时刷新。
const POSITION_SYNC_INTERVAL: Duration = Duration::from_secs(30);

/// 用 WebSocket 盘口重算仓位价格的节拍；价格本身来自推送，这里只做汇总广播。
const POSITION_PRICING_INTERVAL: Duration = Duration::from_secs(1);

/// 执行器共享状态。所有写操作通过锁保护；副作用操作必须先写幂等表。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub market: Arc<MarketFeed>,
    pub funding: Arc<FundingFeed>,
    pub news: Arc<NewsFeed>,
    /// 链上池子价格与预言机价格的最近一次快照。
    pub chain: Arc<ChainFeed>,
    pub telegram: Arc<TelegramBot>,
    pub accounts: Arc<Accounts>,
    pub exchange: Arc<LiveExchange>,
    /// 账号服务签发的授权令牌；只做离线校验，不联网。
    pub license: Arc<LicenseManager>,
    pub orders: Arc<RwLock<Vec<Order>>>,
    pub positions: Arc<RwLock<Vec<Position>>>,
    pub strategies: Arc<RwLock<Vec<StrategyRecord>>>,
    /// 条件单：等待触发，由后台轮询判断有效期与阈值。
    pub conditional: Arc<ConditionalOrders>,
    /// 上币单腿交易：公告识别、开仓记录与止盈止损监控。
    pub listing: Arc<ListingDesk>,
    /// 技能策略：SKILL.md 六章参数化后的触发设置。
    pub skill: Arc<SkillDesk>,
    pub idempotency: Arc<RwLock<HashMap<String, Uuid>>>,
    pub events: broadcast::Sender<ExecutorEvent>,
    pub paused: Arc<AtomicBool>,
    /// 可在运行期编辑的风控限额，写入后立即对后续订单生效。
    pub risk_limits: Arc<RwLock<RiskLimits>>,
    /// 机会成本与收益估算使用的手续费档位。
    pub fees: Arc<RwLock<FeeConfig>>,
    /// 连续失败/被拒的次数；达到阈值自动停摆，成功一笔后清零。
    pub consecutive_rejects: Arc<AtomicU32>,
    /// 上一次账户权益快照，用于发现余额异常。
    pub last_equity_usd: Arc<RwLock<Option<Decimal>>>,
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
        // 链上价格监听复用同一套 HTTP 客户端与事件总线；未配置 RPC 时不发请求。
        let chain = ChainFeed::start(config.as_ref(), http.clone(), events.clone());
        let telegram = TelegramBot::start(config.as_ref(), http.clone());
        // 账户余额采集与行情共用同一套 TLS / 代理配置。
        let account_endpoints = AccountEndpoints {
            binance_spot: config.binance_spot_url.clone(),
            binance_fapi: config.binance_fapi_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            gate: config.gate_base_url.clone(),
            aster: config.aster_base_url.clone(),
            hyperliquid: config.hyperliquid_base_url.clone(),
            lighter: config.lighter_base_url.clone(),
        };
        // 实盘下单走各交易所的私有合约接口，复用已导入账户的凭证。
        let live_endpoints = LiveEndpoints {
            binance: config.binance_fapi_url.clone(),
            binance_spot: config.binance_spot_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            bitget_spot: config.bitget_spot_base_url.clone(),
            gate: config.gate_base_url.clone(),
            gate_spot: config.gate_spot_base_url.clone(),
            aster: config.aster_base_url.clone(),
            hyperliquid: config.hyperliquid_base_url.clone(),
            hyperliquid_spot: config.hyperliquid_spot_base_url.clone(),
            lighter: config.lighter_base_url.clone(),
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

        // 策略定义完全以数据库为准：没有保存过策略时列表就是空的，不再自动生成内置策略。
        let mut strategies = store.load_strategies().await;
        // 条件单持久化在库里，重启后继续等待触发。
        let conditional = ConditionalOrders::with_orders(store.conditional_orders().await);
        // 上币单腿交易的设置与持仓同样持久化，重启后继续监控止盈止损；
        // 每个策略实例一套设置，未保存过时杠杆默认取风控的杠杆上限。
        let listing = ListingDesk::start(
            store.clone(),
            store.listing_settings().await,
            store.listing_trades().await,
            config.risk.max_leverage,
        );
        // 技能策略的设置按策略 ID 持久化，重启后继续按最新参数扫描公告。
        let skill = SkillDesk::start(
            store.clone(),
            store.skill_settings().await,
            config.risk.max_leverage,
        );
        // 历史委托与成交记录来自订单表，重启后底部面板仍能看到历史。
        let mut orders = store.orders().await;
        orders.sort_by_key(|order| std::cmp::Reverse(order.created_at));
        orders.truncate(MAX_IN_MEMORY_ORDERS);
        // 运行计数只记录上线后的增量，升级前落库的订单不会计入；
        // 重启时按订单历史回填一次，避免策略卡片上的信号/订单数一直是 0。
        for record in &mut strategies {
            let counted = orders
                .iter()
                .filter(|order| order.intent.strategy_id == Some(record.id))
                .count() as u64;
            record.runtime.orders = record.runtime.orders.max(counted);
            record.runtime.signals = record.runtime.signals.max(record.runtime.orders);
        }

        // 授权令牌在启动时从本地恢复，过期后仍需回桌面端重新登录换取。
        let license = Arc::new(LicenseManager::start(
            config.license_required,
            config.license_public_key.as_deref(),
            config.license_grace_hours,
            config.license_file.clone(),
        ));

        let risk_limits = config.risk.clone();
        let fees_config = config.fees;
        // 通知分发器复用同一份 Store / 事件总线 / 电报机器人，行情与新闻采集命中条件时调用。
        let notifier = Notifier::new(store.clone(), events.clone(), telegram.clone());
        let state = Self {
            config,
            store,
            market,
            funding,
            news,
            chain,
            telegram,
            accounts,
            exchange,
            license,
            orders: Arc::new(RwLock::new(orders)),
            positions: Arc::new(RwLock::new(Vec::new())),
            strategies: Arc::new(RwLock::new(strategies)),
            conditional: Arc::new(conditional),
            listing,
            skill,
            idempotency: Arc::new(RwLock::new(HashMap::new())),
            events,
            paused: Arc::new(AtomicBool::new(false)),
            risk_limits: Arc::new(RwLock::new(risk_limits)),
            fees: Arc::new(RwLock::new(fees_config)),
            consecutive_rejects: Arc::new(AtomicU32::new(0)),
            last_equity_usd: Arc::new(RwLock::new(None)),
            started_at: Instant::now(),
        };
        // 机器人需要读取执行器状态，因此在 AppState 构造完成后再挂载轮询。
        state.telegram.clone().attach(state.clone());
        // 行情与新闻采集在产生新数据后按通知设置推送。
        state.market.attach_notifier(notifier.clone());
        // 行情侧需要结算间隔来归一跨所资金费率。
        state.market.attach_funding(state.funding.clone());
        state.news.attach_notifier(notifier);
        // 交易所真实持仓同步：桌面端与 Telegram 展示的仓位以交易所为准。
        state.spawn_position_sync();
        // 启动对账：有未完成订单时先暂停，等人工确认后再继续开仓。
        state.spawn_startup_reconciliation();
        // 行情侧通过事件总线请求停摆（如资金费率突变），由状态层统一执行。
        state.spawn_risk_halt_listener();
        // 价格与浮盈由 WebSocket 推送驱动，与持仓同步周期解耦。
        state.spawn_position_pricing();
        state
    }

    /// 订阅行情侧的停摆请求；用事件总线避免行情模块直接依赖执行器状态。
    fn spawn_risk_halt_listener(&self) {
        let mut events = self.events.subscribe();
        let state = self.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let ExecutorEvent::RiskHalt {
                    reason, reason_en, ..
                } = event
                {
                    if !state.is_paused() {
                        state.set_paused(true, Some((&reason, &reason_en)));
                    }
                }
            }
        });
    }

    /// 启动对账：库中仍有未完成订单（提交中/部分成交/在平仓）时暂停开仓。
    ///
    /// 交易所持仓与本地仓位的差异已由 `spawn_position_sync` 以交易所为准覆盖；
    /// 这里负责把「本地以为还在进行中的订单」暴露出来，避免重启后继续叠加仓位。
    fn spawn_startup_reconciliation(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            // 给首轮持仓同步留出时间，避免把「尚未同步」误判成「没有持仓」。
            tokio::time::sleep(Duration::from_secs(5)).await;
            let report = state.reconcile_orders().await;
            if report.incomplete_orders.is_empty() {
                tracing::info!("启动对账通过：没有未完成订单");
            }
        });
    }

    /// 检查库中未完成订单；发现不一致时暂停并写审计。可被桌面端手动触发。
    pub async fn reconcile_orders(&self) -> ReconcileReport {
        let incomplete: Vec<Uuid> = self
            .store
            .orders()
            .await
            .into_iter()
            .filter(|order| !order.status.is_terminal())
            .map(|order| order.id)
            .collect();
        if !incomplete.is_empty() {
            let ids = incomplete
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(",");
            self.store
                .append_audit_log(
                    "system",
                    "reconcile.mismatch",
                    None,
                    json!({ "incomplete_orders": &ids }),
                )
                .await;
            self.set_paused(
                true,
                Some((
                    &format!("启动对账发现 {} 笔未完成订单: {ids}", incomplete.len()),
                    &format!(
                        "{} unfinished orders found during reconciliation: {ids}",
                        incomplete.len()
                    ),
                )),
            );
        }
        ReconcileReport {
            incomplete_orders: incomplete,
            paused: self.is_paused(),
        }
    }

    /// 定期把交易所的真实持仓并入内存仓位，替换本机订单留下的过期记录。
    fn spawn_position_sync(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(POSITION_SYNC_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // 历史仓位组只回填一次，之后完全依赖持久化映射。
            let mut backfilled = false;
            loop {
                ticker.tick().await;
                let snapshots = state.accounts.sync_open_positions().await;
                // 余额异常检测：权益骤降时暂停并告警，而不是继续开仓。
                let equity = state.accounts.total_equity_usd();
                if equity > Decimal::ZERO {
                    state.observe_equity(equity);
                }
                if snapshots.is_empty() {
                    continue;
                }
                // 持仓币种可能不在 watchlist；加入订阅后才能收到实时盘口。
                for snapshot in &snapshots {
                    let symbols = snapshot
                        .positions
                        .iter()
                        .map(|position| position.symbol.clone())
                        .collect::<Vec<_>>();
                    state
                        .market
                        .watch_position_symbols(&snapshot.venue, &symbols);
                }
                // 读取持久化的仓位组，把重启后只剩交易所持仓的分组还原回来。
                let mut groups = state.store.load_position_groups().await;
                if !backfilled {
                    backfilled = true;
                    // 兼容本次修复上线前成交的历史双腿仓位：库里没有映射时从订单库重建一次。
                    if groups.is_empty() {
                        groups = state.backfill_position_groups(&snapshots).await;
                    }
                }
                let lookup: HashMap<(String, String, Side), (Uuid, Option<Uuid>, Option<String>)> =
                    groups
                        .iter()
                        .flat_map(|(group_id, members)| {
                            members.iter().map(move |member| {
                                (
                                    (member.venue.clone(), member.symbol.clone(), member.side),
                                    (*group_id, member.strategy_id, member.source.clone()),
                                )
                            })
                        })
                        .collect();
                // 单腿上币仓位没有仓位组，用持久化的上币交易补回策略与规则来源。
                let listing_sources: HashMap<
                    (String, String, Side),
                    (Option<Uuid>, Option<String>),
                > = state
                    .listing
                    .open_trades()
                    .into_iter()
                    .filter(|trade| !trade.paper)
                    .map(|trade| {
                        (
                            (trade.venue.clone(), trade.symbol.clone(), Side::Buy),
                            (trade.strategy_id, trade.source.clone()),
                        )
                    })
                    .collect();

                let stale = {
                    let mut positions = state.positions.write().expect("仓位锁不可用");
                    merge_exchange_positions(&mut positions, snapshots, &lookup, &listing_sources);
                    // 组内成员全部不在持仓里，说明该组已平仓，删除映射避免同键位被误归组。
                    let live: HashSet<(String, String, Side)> = positions
                        .iter()
                        .map(|position| {
                            (
                                position.venue.clone(),
                                position.symbol.clone(),
                                position.side,
                            )
                        })
                        .collect();
                    groups
                        .iter()
                        .filter(|(_, members)| {
                            members.iter().all(|member| {
                                !live.contains(&(
                                    member.venue.clone(),
                                    member.symbol.clone(),
                                    member.side,
                                ))
                            })
                        })
                        .map(|(group_id, _)| *group_id)
                        .collect::<Vec<_>>()
                };
                if !stale.is_empty() {
                    state.store.delete_position_groups(&stale).await;
                }
            }
        });
    }

    /// 兼容本次修复上线前成交的双腿仓位：本地还没有映射时，从持久化订单里重建一次。
    ///
    /// 只登记当前仍在交易所持仓中的腿，避免把已经平掉的旧组合带回来。
    async fn backfill_position_groups(
        &self,
        snapshots: &[VenuePositions],
    ) -> Vec<(Uuid, Vec<PositionGroupMember>)> {
        let live: HashSet<(String, String, Side)> = snapshots
            .iter()
            .flat_map(|snapshot| {
                snapshot.positions.iter().map(|position| {
                    (
                        position.venue.clone(),
                        position.symbol.clone(),
                        position.side,
                    )
                })
            })
            .collect();
        if live.is_empty() {
            return Vec::new();
        }

        let mut restored = Vec::new();
        for order in self.store.orders().await {
            // 只认开仓双腿单，且必须两条腿都还在持仓里。
            if order.status != OrderStatus::Filled
                || order.intent.legs.len() < 2
                || order.intent.legs.iter().any(|leg| leg.reduce_only)
            {
                continue;
            }
            let members = order
                .intent
                .legs
                .iter()
                .filter(|leg| live.contains(&(leg.venue.clone(), leg.symbol.clone(), leg.side)))
                .map(|leg| PositionGroupMember {
                    strategy_id: order.intent.strategy_id,
                    source: None,
                    venue: leg.venue.clone(),
                    symbol: leg.symbol.clone(),
                    side: leg.side,
                })
                .collect::<Vec<_>>();
            if members.len() < 2 {
                continue;
            }
            self.store.save_position_group(order.id, &members).await;
            restored.push((order.id, members));
        }
        if !restored.is_empty() {
            tracing::info!(groups = restored.len(), "已从历史订单回填仓位组映射");
        }
        restored
    }

    /// 用 WebSocket 最新盘口刷新标记价与浮动盈亏，变化时推送给桌面端。
    fn spawn_position_pricing(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(POSITION_PRICING_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let changed = {
                    let mut positions = state.positions.write().expect("仓位锁不可用");
                    state.apply_realtime_prices(&mut positions)
                };
                if !changed {
                    continue;
                }
                let positions = state.positions.read().expect("仓位锁不可用").clone();
                let _ = state.events.send(ExecutorEvent::PositionsUpdated {
                    positions,
                    at: Utc::now(),
                });
            }
        });
    }

    /// 用 WebSocket 盘口覆盖仓位的标记价并重算浮动盈亏；返回是否有变化。
    pub fn apply_realtime_prices(&self, positions: &mut [Position]) -> bool {
        let mut changed = false;
        for position in positions {
            let Some(mark_price) = self
                .market
                .realtime_price(&position.venue, &position.symbol)
            else {
                continue;
            };
            if mark_price == position.mark_price {
                continue;
            }
            position.unrealized_pnl = match position.side {
                Side::Buy => (mark_price - position.entry_price) * position.quantity,
                Side::Sell => (position.entry_price - mark_price) * position.quantity,
            };
            position.mark_price = mark_price;
            position.updated_at = Utc::now();
            changed = true;
        }
        changed
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

    pub fn fees(&self) -> FeeConfig {
        *self.fees.read().expect("费率锁不可用")
    }

    /// 设置页保存后立即生效：写文件、更新内存，并让行情侧下一轮重算机会。
    pub fn set_fees(&self, fees: FeeConfig) -> anyhow::Result<FeeConfig> {
        let fees = fees.validate()?;
        fees.save(&self.config.fees_file)?;
        *self.fees.write().expect("费率锁不可用") = fees;
        self.market.set_fees(fees);
        Ok(fees)
    }

    /// 记录一次失败/拒单；连续次数达到阈值即停摆，避免极端行情下继续开仓。
    pub fn record_reject(&self, reason: &str) {
        let count = self.consecutive_rejects.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.config.risk_guard.max_consecutive_rejects && !self.is_paused() {
            self.set_paused(
                true,
                Some((
                    &format!("连续 {count} 次下单失败或拒单（最近一次: {reason}）"),
                    &format!("{count} consecutive order failures/rejections (last: {reason})"),
                )),
            );
        }
    }

    /// 成功成交后清零连续失败计数，避免偶发失败把执行器困在暂停状态。
    pub fn record_success(&self) {
        self.consecutive_rejects.store(0, Ordering::Relaxed);
    }

    /// 记录账户权益；跌幅超过阈值时暂停并告警。
    pub fn observe_equity(&self, equity_usd: Decimal) {
        let mut last = self.last_equity_usd.write().expect("权益锁不可用");
        if let Some(previous) = *last {
            if previous > Decimal::ZERO {
                let drop_pct = (previous - equity_usd) / previous * Decimal::from(100);
                if drop_pct >= self.config.risk_guard.equity_drop_pct {
                    self.set_paused(
                        true,
                        Some((
                            &format!(
                                "账户权益从 {previous} 跌至 {equity_usd}（-{drop_pct}%）"
                            ),
                            &format!(
                                "Account equity dropped from {previous} to {equity_usd} (-{drop_pct}%)"
                            ),
                        )),
                    );
                }
            }
        }
        *last = Some(equity_usd);
    }

    /// 暂停只阻止新开仓，查看仓位、撤单与风险处置始终可用。
    /// `reason` 同时提供中英文版本，告警会按语言分别推送到桌面端与 Telegram。
    pub fn set_paused(&self, paused: bool, reason: Option<(&str, &str)>) {
        self.paused.store(paused, Ordering::Relaxed);
        let (message, message_en) = match (paused, reason) {
            (true, Some((reason, reason_en))) => (
                format!("执行器已暂停开仓: {reason}"),
                format!("Executor paused new entries: {reason_en}"),
            ),
            (true, None) => (
                "执行器已暂停开仓".to_string(),
                "Executor paused new entries".to_string(),
            ),
            (false, _) => (
                "执行器已恢复开仓".to_string(),
                "Executor resumed entries".to_string(),
            ),
        };
        let _ = self.events.send(ExecutorEvent::Alert {
            level: if paused {
                AlertLevel::Warning
            } else {
                AlertLevel::Info
            },
            message,
            message_en: Some(message_en),
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

/// 启动对账结果：返回未完成订单，供桌面端与 Telegram 展示。
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileReport {
    pub incomplete_orders: Vec<Uuid>,
    pub paused: bool,
}

/// 用交易所真实持仓覆盖某交易所的内存仓位。
///
/// 同一 (交易所, 交易对, 方向) 若已有本机订单产生的记录，保留其策略归属与
/// UUID；交易所已不存在的仓位直接移除，保证展示与真实敞口一致。
/// 本地记录已丢失（如执行器重启）时，用持久化的仓位组把分组还原回来。
fn merge_exchange_positions(
    positions: &mut Vec<Position>,
    snapshots: Vec<VenuePositions>,
    groups: &HashMap<(String, String, Side), (Uuid, Option<Uuid>, Option<String>)>,
    listing_sources: &HashMap<(String, String, Side), (Option<Uuid>, Option<String>)>,
) {
    for snapshot in snapshots {
        let (mut local, mut retained): (Vec<Position>, Vec<Position>) = positions
            .drain(..)
            .partition(|position| position.venue == snapshot.venue);

        for mut position in snapshot.positions {
            let key = (
                position.venue.clone(),
                position.symbol.clone(),
                position.side,
            );
            if let Some(index) = local
                .iter()
                .position(|item| item.symbol == position.symbol && item.side == position.side)
            {
                let existing = local.remove(index);
                position.id = existing.id;
                position.strategy_id = existing.strategy_id;
                position.source = existing.source;
                position.group_id = existing.group_id;
            }
            if position.group_id.is_none() {
                if let Some((group_id, strategy_id, source)) = groups.get(&key) {
                    position.group_id = Some(*group_id);
                    if position.strategy_id.is_none() {
                        position.strategy_id = *strategy_id;
                        position.source = source.clone();
                    }
                }
            }
            // 交易所有仓位但本地没有记录（例如重启后），用上币交易补回来源。
            if position.strategy_id.is_none() {
                if let Some((strategy_id, source)) = listing_sources.get(&key) {
                    position.strategy_id = *strategy_id;
                    position.source = source.clone();
                }
            }
            retained.push(position);
        }

        *positions = retained;
    }
}
