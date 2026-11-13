//! 上币新闻单腿交易。
//!
//! 执行器持续扫描交易所上币公告，识别出代币后按执行交易所优先级自动单腿做多；
//! 成交后后台按标记价监控止盈/止损并自动平仓。
//! 所有开平仓请求仍然走统一网关，因此风控、暂停开关与审计完全复用现有逻辑。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use arb_core::{
    OrderOrigin, OrderStatus, PositionSide, Side, StrategyExecutionMode, StrategyOpenPosition,
    StrategyStatus,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    events::{AlertLevel, ExecutorEvent},
    gateway::{self, LegOrderRequest, OrderBatchRequest},
    state::AppState,
    store::Store,
};

/// 止盈止损的检查节拍。
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// 内存里保留的交易记录条数上限。
const MAX_TRADES: usize = 200;
/// 开仓后至少等待这么久，才允许因「交易所没有持仓」判定为已平仓，
/// 避免持仓同步还没跑完就误判。
const MIN_HOLD_BEFORE_MISSING_SECS: i64 = 60;
/// 执行器刚启动时仓位同步可能还没完成，先等一会儿再判断「交易所已无持仓」。
const STARTUP_GRACE_SECS: u64 = 60;
/// 每轮扫描的最新公告条数；新闻缓存本身已按发布时间倒序。
const NEWS_SCAN_LIMIT: usize = 40;
/// 同一代币的自动开仓冷却时间，避免多个来源对同一条上币重复下单。
const TOKEN_COOLDOWN_SECS: i64 = 24 * 60 * 60;
/// 电报手动下单参数的持久化键；与策略 ID 共用同一张设置表，重启后仍可复用。
const MANUAL_RULE_KEY: &str = "manual";

/// 一条上币公告跟随规则：监听哪家交易所的公告、按什么优先级在哪下单。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ListingRule {
    /// 规则 ID，用于「策略 + 规则 + 代币」维度的冷却去重。
    pub id: Uuid,
    /// 规则名，便于在界面与状态里区分，例如「币安 → OKX」。
    pub name: String,
    /// 该规则是否参与扫描；总开关之外可以单独关掉某条规则，默认启用。
    pub enabled: bool,
    /// 该规则的下单模式：真实提交交易所，或只记录模拟成交。
    pub execution_mode: StrategyExecutionMode,
    /// 单笔名义价值（美元）。
    pub notional_usd: Decimal,
    /// 下单杠杆。
    pub leverage: Decimal,
    /// 监听上币公告的交易所来源；为空表示全部来源。
    pub watch_venues: Vec<String>,
    /// 执行下单的交易所，按优先级从高到低排序；为空表示全部可交易交易所。
    #[serde(alias = "venues")]
    pub execution_venues: Vec<String>,
    /// 止盈百分比（例如 20 表示 +20%）；0 表示不启用。
    pub take_profit_pct: Decimal,
    /// 止损百分比（例如 10 表示 -10%）；0 表示不启用。
    pub stop_loss_pct: Decimal,
}

impl Default for ListingRule {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            enabled: true,
            execution_mode: StrategyExecutionMode::Real,
            notional_usd: Decimal::from(100),
            leverage: Decimal::ONE,
            watch_venues: Vec::new(),
            execution_venues: Vec::new(),
            take_profit_pct: Decimal::from(20),
            stop_loss_pct: Decimal::from(10),
        }
    }
}

/// 一个策略的全部规则：同一条「上币公告跟随」可以分别跟随多家交易所的公告。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListingSettings {
    pub rules: Vec<ListingRule>,
}

impl ListingSettings {
    /// 缺规则的旧配置回落到一条默认规则，并补齐规则 ID。
    fn normalized(mut self, leverage: Decimal) -> Self {
        if self.rules.is_empty() {
            self.rules.push(ListingRule {
                leverage,
                ..ListingRule::default()
            });
        }
        for rule in &mut self.rules {
            if rule.id.is_nil() {
                rule.id = Uuid::new_v4();
            }
        }
        self
    }

    /// 用于手动下单的规则：优先第一条启用的规则；规则列表为空时给一条默认规则。
    pub fn first_rule(&self) -> ListingRule {
        self.rules
            .iter()
            .find(|rule| rule.enabled)
            .or_else(|| self.rules.first())
            .cloned()
            .unwrap_or_else(ListingRule::default)
    }
}

/// 一笔上币单腿交易的生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingStatus {
    /// 已成交，等待止盈/止损或手动平仓。
    Open,
    /// 正在提交平仓单，避免同一笔交易被重复平仓。
    Closing,
    /// 止盈平仓。
    TakeProfit,
    /// 止损平仓。
    StopLoss,
    /// 手动平仓。
    Closed,
    /// 开仓后尚未建仓成功，或平仓多次失败需要人工处理。
    Failed,
}

impl ListingStatus {
    pub fn is_open(self) -> bool {
        self == Self::Open
    }
}

/// 一笔上币单腿交易的完整记录，持久化后重启仍能继续监控止盈止损。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingTrade {
    pub id: Uuid,
    /// 公告里识别出的代币代码，例如 `XYZ`。
    pub token: String,
    /// 公告标题（截断后保存），用于复盘。
    pub headline: String,
    pub venue: String,
    /// 实际下单交易对，例如 `XYZUSDT`；用户可手动改成交易所支持的其他写法。
    pub symbol: String,
    /// 开仓方向：买入做多或卖出做空。
    #[serde(default = "default_side")]
    pub side: Side,
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    pub quantity: Decimal,
    pub entry_price: Decimal,
    pub take_profit_pct: Decimal,
    pub stop_loss_pct: Decimal,
    pub status: ListingStatus,
    pub entry_order_id: Option<Uuid>,
    pub close_order_id: Option<Uuid>,
    pub close_price: Option<Decimal>,
    /// 模拟下单：只记录通过行情快照估出的价格，不向交易所提交任何委托。
    #[serde(default)]
    pub paper: bool,
    /// 技能策略触发的交易：告警文案使用「技能」而不是「上币」。
    #[serde(default)]
    pub skill: bool,
    /// 最长持有时长（秒）；0 表示只按止盈止损离场。
    #[serde(default)]
    pub max_hold_secs: i64,
    /// 是否发送开平仓告警；旧记录默认发送。
    #[serde(default = "default_notify")]
    pub notify: bool,
    /// 触发这笔交易的策略；电报手动确认为 None。
    #[serde(default)]
    pub strategy_id: Option<Uuid>,
    /// 触发这笔交易的技能规则；一条技能可以有多条规则。
    #[serde(default)]
    pub rule_id: Option<Uuid>,
    /// 触发这笔交易的规则名称，便于在仓位里说明来源。
    #[serde(default)]
    pub source: Option<String>,
    /// 海龟系统编号：1 或 2；其他策略为空。
    #[serde(default)]
    pub system: Option<u8>,
    /// 同一组金字塔 Units 共用的分组标识。
    #[serde(default)]
    pub group_id: Option<Uuid>,
    /// 组内 Unit 序号，0 表示首仓。
    #[serde(default)]
    pub unit_index: u32,
    /// 开仓时的 N 值（ATR），用于整组止损跟随最新 Unit。
    #[serde(default)]
    pub n_at_entry: Option<Decimal>,
    pub note: Option<String>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// 旧交易记录都是做多，缺失方向时按买入处理。
fn default_side() -> Side {
    Side::Buy
}

/// 旧交易记录没有通知开关，默认仍然发送告警。
fn default_notify() -> bool {
    true
}

/// 平仓原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    TakeProfit,
    StopLoss,
    Timeout,
    /// 指标信号反转，由技能运行时主动离场。
    Signal,
    Manual,
}

impl CloseReason {
    fn status(self) -> ListingStatus {
        match self {
            Self::TakeProfit => ListingStatus::TakeProfit,
            Self::StopLoss => ListingStatus::StopLoss,
            Self::Timeout => ListingStatus::Closed,
            Self::Signal => ListingStatus::Closed,
            Self::Manual => ListingStatus::Closed,
        }
    }

    fn label_zh(self) -> &'static str {
        match self {
            Self::TakeProfit => "止盈",
            Self::StopLoss => "止损",
            Self::Timeout => "超时",
            Self::Signal => "信号",
            Self::Manual => "手动",
        }
    }

    fn label_en(self) -> &'static str {
        match self {
            Self::TakeProfit => "take-profit",
            Self::StopLoss => "stop-loss",
            Self::Timeout => "max-hold",
            Self::Signal => "signal",
            Self::Manual => "manual",
        }
    }
}

/// 发起开仓所需的参数。
#[derive(Debug, Clone)]
pub struct OpenListingRequest {
    pub token: String,
    pub headline: String,
    pub venue: String,
    pub symbol: String,
    /// 开仓方向：买入做多或卖出做空。
    pub side: Side,
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    pub take_profit_pct: Decimal,
    pub stop_loss_pct: Decimal,
    /// 触发这条开仓的规则所属策略；电报手动确认为 None。
    pub strategy_id: Option<Uuid>,
    /// 触发这条开仓的技能规则；一条技能可以有多条规则。
    pub rule_id: Option<Uuid>,
    /// 触发开仓的规则名称，用于在仓位与交易记录里说明来源。
    pub rule_name: Option<String>,
    /// 海龟系统编号：1 或 2；其他策略为空。
    pub system: Option<u8>,
    /// 同一组金字塔 Units 共用的分组标识。
    pub group_id: Option<Uuid>,
    /// 组内 Unit 序号，0 表示首仓。
    pub unit_index: u32,
    /// 开仓时的 N 值（ATR），用于整组止损跟随最新 Unit。
    pub n_at_entry: Option<Decimal>,
    /// 模拟下单：按盘口中间价假装成交，不提交交易所。
    pub paper: bool,
    /// 技能策略触发：沿用同一套单腿交易记录，但告警按技能文案展示。
    pub skill: bool,
    /// 最长持有时长（秒）；0 表示只按止盈止损离场。
    pub max_hold_secs: i64,
    /// 是否发送开平仓告警。
    pub notify: bool,
}

/// 从公告里识别出的一条上币信息，供电报确认与自动执行使用。
#[derive(Debug, Clone)]
pub struct ListingAnnouncement {
    /// 公告里识别出的代币代码，例如 `XYZ`。
    pub token: String,
    /// 默认下单交易对，例如 `XYZUSDT`。
    pub symbol: String,
    /// 公告标题。
    pub headline: String,
}

/// 上币单腿交易的内存表；持久化由 `Store` 负责。
pub struct ListingDesk {
    store: Arc<Store>,
    /// 每个上币公告跟随策略实例各自的设置，键为策略 ID。
    settings: RwLock<BTreeMap<String, ListingSettings>>,
    /// 从未保存过设置时使用的默认杠杆（风控杠杆上限）。
    default_leverage: Decimal,
    trades: RwLock<Vec<ListingTrade>>,
    /// 已经处理过的公告，避免同一条新闻重复触发。
    seen: Mutex<HashSet<String>>,
    /// 已自动开仓的「策略 + 代币」与时间，避免同一代币重复下单。
    tokens: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl ListingDesk {
    pub fn start(
        store: Arc<Store>,
        settings: BTreeMap<String, ListingSettings>,
        trades: Vec<ListingTrade>,
        default_leverage: Decimal,
    ) -> Arc<Self> {
        // 旧配置是单条设置，读进来后统一补成规则列表。
        let settings = settings
            .into_iter()
            .map(|(id, settings)| (id, settings.normalized(default_leverage)))
            .collect();
        Arc::new(Self {
            store,
            settings: RwLock::new(settings),
            default_leverage,
            trades: RwLock::new(trades),
            seen: Mutex::new(HashSet::new()),
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// 未保存过设置时的默认值：一条规则，杠杆跟随风控上限。
    pub fn default_settings(&self) -> ListingSettings {
        ListingSettings {
            rules: vec![ListingRule {
                leverage: self.default_leverage,
                ..ListingRule::default()
            }],
        }
    }

    /// 指定策略的设置；该策略未保存过时返回默认值。
    pub fn settings(&self, strategy_id: Uuid) -> ListingSettings {
        self.settings
            .read()
            .expect("上币交易设置锁不可用")
            .get(&strategy_id.to_string())
            .cloned()
            .unwrap_or_else(|| self.default_settings())
    }

    /// 保存某个策略的设置，返回保存后的完整配置。
    pub async fn update_settings(
        &self,
        strategy_id: Uuid,
        next: ListingSettings,
    ) -> ListingSettings {
        let next = next.normalized(self.default_leverage);
        let snapshot = {
            let mut settings = self.settings.write().expect("上币交易设置锁不可用");
            settings.insert(strategy_id.to_string(), next.clone());
            settings.clone()
        };
        self.store.save_listing_settings(&snapshot).await;
        next
    }

    /// 电报手动下单保存的参数；没有保存过时返回 None。
    pub fn manual_rule(&self) -> Option<ListingRule> {
        self.settings
            .read()
            .expect("上币交易设置锁不可用")
            .get(MANUAL_RULE_KEY)
            .and_then(|settings| settings.rules.first().cloned())
    }

    /// 保存电报手动下单参数，下次手动下单直接复用。
    pub async fn save_manual_rule(&self, rule: ListingRule) {
        let next = ListingSettings { rules: vec![rule] }.normalized(self.default_leverage);
        let snapshot = {
            let mut settings = self.settings.write().expect("上币交易设置锁不可用");
            settings.insert(MANUAL_RULE_KEY.to_string(), next);
            settings.clone()
        };
        self.store.save_listing_settings(&snapshot).await;
    }

    /// 仍处于持仓中的交易。
    pub fn open_trades(&self) -> Vec<ListingTrade> {
        self.trades
            .read()
            .expect("上币交易锁不可用")
            .iter()
            .filter(|trade| trade.status.is_open())
            .cloned()
            .collect()
    }

    /// 全部交易记录，含已平仓，用于系统一盈利过滤等历史判断。
    pub fn all_trades(&self) -> Vec<ListingTrade> {
        self.trades.read().expect("上币交易锁不可用").clone()
    }

    /// 按 ID 前缀查找持仓中的交易，供电报里输入短编号。
    pub fn find_open_by_prefix(&self, prefix: &str) -> Option<ListingTrade> {
        let prefix = prefix.trim().to_ascii_lowercase();
        if prefix.is_empty() {
            return None;
        }
        self.trades
            .read()
            .expect("上币交易锁不可用")
            .iter()
            .filter(|trade| trade.status.is_open())
            .find(|trade| trade.id.to_string().starts_with(&prefix))
            .cloned()
    }

    /// 记录一次公告；返回 true 表示这是第一次看到该公告。
    pub fn mark_seen(&self, key: &str) -> bool {
        self.seen
            .lock()
            .expect("上币公告去重锁不可用")
            .insert(key.to_string())
    }

    /// 同一代币在冷却期内只自动开仓一次；返回 true 表示可以继续执行。
    fn mark_token(&self, token: &str) -> bool {
        let now = Utc::now();
        let mut tokens = self.tokens.lock().expect("上币代币去重锁不可用");
        tokens.retain(|_, at| now.signed_duration_since(*at).num_seconds() < TOKEN_COOLDOWN_SECS);
        if tokens.contains_key(token) {
            return false;
        }
        tokens.insert(token.to_string(), now);
        true
    }

    fn insert(&self, trade: ListingTrade) {
        let mut trades = self.trades.write().expect("上币交易锁不可用");
        trades.push(trade);
        trades.sort_by(|left, right| right.opened_at.cmp(&left.opened_at));
        trades.truncate(MAX_TRADES);
    }

    fn update(&self, id: Uuid, change: impl FnOnce(&mut ListingTrade)) -> Option<ListingTrade> {
        let mut trades = self.trades.write().expect("上币交易锁不可用");
        let trade = trades.iter_mut().find(|trade| trade.id == id)?;
        change(trade);
        trade.updated_at = Utc::now();
        Some(trade.clone())
    }

    /// 原子地把一笔持仓标记为「平仓中」，防止后台监控与手动平仓重复提交。
    fn begin_close(&self, id: Uuid) -> Option<ListingTrade> {
        self.update(id, |trade| {
            if trade.status.is_open() {
                trade.status = ListingStatus::Closing;
            }
        })
        .filter(|trade| trade.status == ListingStatus::Closing)
    }
}

/// 单腿市价开仓（做多或做空），成交后登记止盈止损。
pub async fn open_position(
    state: &AppState,
    request: OpenListingRequest,
) -> Result<ListingTrade, ApiError> {
    let symbol = request.symbol.trim().to_ascii_uppercase();
    let venue = request.venue.trim().to_ascii_lowercase();
    if symbol.is_empty() || venue.is_empty() {
        return Err(ApiError::BadRequest("交易所与交易对不能为空".to_string()));
    }
    if request.notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("下单金额必须大于 0".to_string()));
    }
    if request.leverage <= Decimal::ZERO {
        return Err(ApiError::BadRequest("杠杆必须大于 0".to_string()));
    }
    if request.take_profit_pct < Decimal::ZERO || request.stop_loss_pct < Decimal::ZERO {
        return Err(ApiError::BadRequest("止盈止损百分比不能为负数".to_string()));
    }
    if state.is_paused() {
        return Err(ApiError::BadRequest("执行器已暂停开仓".to_string()));
    }
    // 现货腿只能买入（做多或平掉已有持仓），没有开空能力。
    if crate::market::is_spot_venue(&venue) && request.side == Side::Sell {
        return Err(ApiError::BadRequest(format!(
            "{venue} 是现货交易所，不支持开空；请改用永续交易所或改为买入"
        )));
    }
    // 现货没有杠杆，单腿记录按 1 倍保存。
    let leverage = if crate::market::is_spot_venue(&venue) {
        Decimal::ONE
    } else {
        request.leverage
    };

    // 先确认该交易所有行情，避免把「未上线」误判成可下单。
    let quote = state
        .market
        .refresh_quote(&venue, &symbol)
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!("{symbol} 在 {venue} 没有可用行情: {error}"))
        })?;

    let now = Utc::now();
    let (entry_price, quantity, entry_order_id, paper, note) = if request.paper {
        // 模拟下单：按盘口中间价假装成交，不提交交易所。
        let price = (quote.bid + quote.ask) / Decimal::from(2);
        if price <= Decimal::ZERO {
            return Err(ApiError::BadRequest(format!(
                "{symbol} 在 {venue} 没有可用盘口"
            )));
        }
        (
            price,
            request.notional_usd / price,
            None,
            true,
            Some("模拟下单，未提交交易所".to_string()),
        )
    } else {
        let outcome = gateway::submit_legs(
            state,
            OrderBatchRequest {
                legs: vec![LegOrderRequest {
                    venue: venue.clone(),
                    symbol: symbol.clone(),
                    side: request.side,
                    notional_usd: request.notional_usd,
                    limit_price: None,
                    quantity: None,
                }],
                leverage: Some(leverage),
                max_slippage_bps: None,
                strategy_id: request.strategy_id,
                source: request.rule_name.clone(),
                // 规则自动触发的订单按策略来源记账；电报手动确认仍记为 Telegram。
                origin: if request.strategy_id.is_some() {
                    OrderOrigin::Strategy
                } else {
                    OrderOrigin::Telegram
                },
                idempotency_key: format!("listing-{}", Uuid::new_v4()),
                closing: false,
                max_unhedged_secs: None,
            },
        )
        .await?;

        let order = outcome.order;
        if order.status != OrderStatus::Filled {
            return Err(ApiError::BadRequest(
                order
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("订单状态 {:?}", order.status)),
            ));
        }
        let leg = order
            .legs
            .first()
            .ok_or_else(|| ApiError::Internal("订单没有成交腿".to_string()))?;
        let entry_price = leg
            .average_price
            .filter(|price| *price > Decimal::ZERO)
            .unwrap_or(leg.intent.limit_price);
        let quantity = if leg.filled_quantity > Decimal::ZERO {
            leg.filled_quantity
        } else {
            leg.intent.quantity
        };
        if entry_price <= Decimal::ZERO || quantity <= Decimal::ZERO {
            return Err(ApiError::Internal("订单成交价格或数量无效".to_string()));
        }
        (entry_price, quantity, Some(order.id), false, None)
    };

    let trade = ListingTrade {
        id: Uuid::new_v4(),
        token: request.token.trim().to_ascii_uppercase(),
        headline: request.headline.chars().take(200).collect(),
        venue: venue.clone(),
        symbol: symbol.clone(),
        side: request.side,
        notional_usd: request.notional_usd,
        leverage,
        quantity,
        entry_price,
        take_profit_pct: request.take_profit_pct,
        stop_loss_pct: request.stop_loss_pct,
        status: ListingStatus::Open,
        entry_order_id,
        close_order_id: None,
        close_price: None,
        paper,
        skill: request.skill,
        max_hold_secs: request.max_hold_secs,
        notify: request.notify,
        strategy_id: request.strategy_id,
        rule_id: request.rule_id,
        source: request.rule_name.clone(),
        system: request.system,
        group_id: request.group_id,
        unit_index: request.unit_index,
        n_at_entry: request.n_at_entry,
        note,
        opened_at: now,
        closed_at: None,
        updated_at: now,
    };
    state.listing.insert(trade.clone());
    state.store.save_listing_trade(&trade).await;
    // 加入实时订阅后，止盈止损可以直接读缓存盘口，不必每轮打 REST。
    state
        .market
        .watch_position_symbols(&venue, std::slice::from_ref(&symbol));

    let mode_zh = if trade.paper { "（模拟）" } else { "" };
    let mode_en = if trade.paper { " (paper)" } else { "" };
    let label_zh = if trade.skill {
        "技能单腿"
    } else {
        "上币单腿"
    };
    let label_en = if trade.skill {
        "Skill leg"
    } else {
        "Listing leg"
    };
    let (side_zh, side_en) = if trade.side == Side::Buy {
        ("开多", "long opened")
    } else {
        ("开空", "short opened")
    };
    // 技能/公告触发的交易回写策略运行状态，让信号、订单与当前持仓和真实成交一致。
    if let Some(strategy_id) = trade.strategy_id {
        crate::strategy_runner::note_open(
            state,
            strategy_id,
            strategy_open_position(&trade),
            format!(
                "{label_zh}{side_zh}{mode_zh} {} {} @ {}，金额 {} USD",
                trade.symbol,
                trade.venue,
                trade.entry_price.round_dp(6),
                trade.notional_usd.round_dp(2),
            ),
            trade.entry_order_id,
        )
        .await;
    }
    if trade.notify {
        let _ = state.events.send(ExecutorEvent::Alert {
            level: AlertLevel::Info,
            message: format!(
                "{label_zh}{side_zh}{mode_zh} {} {} @ {}，金额 {} USD，止盈 {}%，止损 {}%",
                trade.symbol,
                trade.venue,
                trade.entry_price.round_dp(6),
                trade.notional_usd.round_dp(2),
                trade.take_profit_pct.round_dp(2),
                trade.stop_loss_pct.round_dp(2),
            ),
            message_en: Some(format!(
                "{label_en} {side_en}{mode_en}: {} {} @ {}, {} USD, TP {}%, SL {}%",
                trade.symbol,
                trade.venue,
                trade.entry_price.round_dp(6),
                trade.notional_usd.round_dp(2),
                trade.take_profit_pct.round_dp(2),
                trade.stop_loss_pct.round_dp(2),
            )),
            at: Utc::now(),
        });
    }
    Ok(trade)
}

/// 把单腿交易转成策略运行状态里的当前持仓。
fn strategy_open_position(trade: &ListingTrade) -> StrategyOpenPosition {
    StrategyOpenPosition {
        symbol: trade.symbol.clone(),
        long_venue: String::new(),
        short_venue: String::new(),
        venue: Some(trade.venue.clone()),
        side: Some(if trade.side == Side::Buy {
            PositionSide::Long
        } else {
            PositionSide::Short
        }),
        entry_price: Some(trade.entry_price),
        notional_usd: trade.notional_usd,
        opened_at: trade.opened_at,
        entry_net_edge_bps: Decimal::ZERO,
    }
}

/// 市价平掉一笔持仓；止盈止损与手动平仓共用。
pub async fn close_position(
    state: &AppState,
    id: Uuid,
    reason: CloseReason,
) -> Result<ListingTrade, ApiError> {
    let Some(trade) = state.listing.begin_close(id) else {
        return Err(ApiError::Conflict("该交易不在持仓中，无法平仓".to_string()));
    };
    let price = mark_price(state, &trade.venue, &trade.symbol)
        .await
        .unwrap_or(trade.entry_price);
    // 模拟持仓不提交交易所，直接按当前标记价记账。
    let close_side = if trade.side == Side::Buy {
        Side::Sell
    } else {
        Side::Buy
    };
    let (failure, close_order_id) = if trade.paper {
        (None, None)
    } else {
        let notional_usd = trade.quantity * price;
        let outcome = gateway::submit_legs(
            state,
            OrderBatchRequest {
                legs: vec![LegOrderRequest {
                    venue: trade.venue.clone(),
                    symbol: trade.symbol.clone(),
                    side: close_side,
                    notional_usd: notional_usd.max(Decimal::ONE),
                    limit_price: None,
                    // 直接使用开仓数量，避免按价格换算造成少平或多平。
                    quantity: Some(trade.quantity),
                }],
                leverage: None,
                max_slippage_bps: None,
                strategy_id: trade.strategy_id,
                source: trade.source.clone(),
                // 带策略的交易平仓按策略来源记账，其余（电报手动单）记为 Telegram。
                origin: if trade.strategy_id.is_some() {
                    OrderOrigin::Strategy
                } else {
                    OrderOrigin::Telegram
                },
                idempotency_key: format!("listing-close-{}", trade.id),
                closing: true,
                max_unhedged_secs: None,
            },
        )
        .await;

        match outcome {
            Ok(outcome) if outcome.order.status == OrderStatus::Filled => {
                (None, Some(outcome.order.id))
            }
            Ok(outcome) => (
                Some(
                    outcome
                        .order
                        .note
                        .clone()
                        .unwrap_or_else(|| format!("订单状态 {:?}", outcome.order.status)),
                ),
                Some(outcome.order.id),
            ),
            Err(error) => (Some(error.to_string()), None),
        }
    };

    if let Some(failure) = failure {
        // 平仓失败时恢复为持仓中，下一轮继续尝试，避免止盈止损被静默放弃。
        let note = format!("{}平仓失败，稍后重试: {failure}", reason.label_zh());
        if let Some(updated) = state.listing.update(id, |trade| {
            trade.status = ListingStatus::Open;
            trade.note = Some(note.clone());
        }) {
            state.store.save_listing_trade(&updated).await;
        }
        if let Some(strategy_id) = trade.strategy_id {
            crate::strategy_runner::note_error(state, strategy_id, note).await;
        }
        return Err(ApiError::BadRequest(failure));
    }

    let Some(closed) = state.listing.update(id, |trade| {
        trade.status = reason.status();
        trade.close_order_id = close_order_id;
        trade.close_price = Some(price);
        trade.closed_at = Some(Utc::now());
        trade.note = None;
    }) else {
        return Err(ApiError::NotFound("交易记录不存在".to_string()));
    };
    state.store.save_listing_trade(&closed).await;

    let change_pct = pnl_pct(closed.side, closed.entry_price, price);
    let mode_zh = if closed.paper { "（模拟）" } else { "" };
    let mode_en = if closed.paper { " (paper)" } else { "" };
    let label_zh = if closed.skill {
        "技能单腿"
    } else {
        "上币单腿"
    };
    let label_en = if closed.skill {
        "Skill leg"
    } else {
        "Listing leg"
    };
    let (side_zh, side_en) = if closed.side == Side::Buy {
        ("多头", "long")
    } else {
        ("空头", "short")
    };
    // 平仓后把策略的当前持仓切换成同策略剩余持仓，没有剩余则清空。
    if let Some(strategy_id) = closed.strategy_id {
        let remaining = state
            .listing
            .open_trades()
            .into_iter()
            .find(|trade| trade.strategy_id == Some(strategy_id))
            .map(|trade| strategy_open_position(&trade));
        crate::strategy_runner::note_close(
            state,
            strategy_id,
            remaining,
            format!(
                "{label_zh}{}{side_zh}{mode_zh} {} {} @ {}，盈亏 {}%",
                reason.label_zh(),
                closed.symbol,
                closed.venue,
                price.round_dp(6),
                change_pct,
            ),
            close_order_id,
        )
        .await;
    }
    if closed.notify {
        let _ = state.events.send(ExecutorEvent::Alert {
            level: AlertLevel::Info,
            message: format!(
                "{label_zh}{}{side_zh}{mode_zh} {} {} @ {}，盈亏 {}%",
                reason.label_zh(),
                closed.symbol,
                closed.venue,
                price.round_dp(6),
                change_pct,
            ),
            message_en: Some(format!(
                "{label_en} {side_en} {}{mode_en}: {} {} @ {}, PnL {}%",
                reason.label_en(),
                closed.symbol,
                closed.venue,
                price.round_dp(6),
                change_pct,
            )),
            at: Utc::now(),
        });
    }
    Ok(closed)
}

/// 启动止盈止损监控与上币公告扫描。
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 首次扫描前先记录当前缓存里的公告，只有之后出现的新公告才自动开仓。
        let mut news_baselined = false;
        loop {
            ticker.tick().await;
            if state.news.primed() {
                if news_baselined {
                    scan_news(&state).await;
                } else {
                    baseline_news(&state);
                    news_baselined = true;
                }
            }
            evaluate(&state).await;
        }
    });
}

/// 记录当前缓存里的公告，避免打开开关时把历史新闻全部弹一遍。
fn baseline_news(state: &AppState) {
    for item in state.news.snapshot(NEWS_SCAN_LIMIT).items {
        state.listing.mark_seen(&news_key(&item.id));
    }
}

/// 扫描最新公告：每个运行中策略的各条规则按自己的监听来源与执行优先级独立开仓。
async fn scan_news(state: &AppState) {
    let rules = running_strategies(state).await;
    // 技能运行已接管内置策略（含上币公告跟随），同一公告由 skill 模块按技能参数开仓；
    // 这里跳过它们，避免同一条公告被两套运行时各开一单。
    let managed: Vec<Uuid> = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .filter(|record| crate::skill::manages(record))
        .map(|record| record.id)
        .collect();
    for item in state.news.snapshot(NEWS_SCAN_LIMIT).items {
        if !state.listing.mark_seen(&news_key(&item.id)) {
            continue;
        }
        let Some(token) = detect_listing_token(&item.title) else {
            continue;
        };
        for (strategy_id, _, settings) in &rules {
            if managed.contains(strategy_id) {
                continue;
            }
            for rule in &settings.rules {
                // 规则开关优先于策略总开关：关掉的规则保留配置但不参与开仓。
                if !rule.enabled {
                    continue;
                }
                if !rule.watch_venues.is_empty()
                    && !rule.watch_venues.iter().any(|venue| venue == &item.source)
                {
                    continue;
                }
                // 同一策略的多条规则可能监听同一来源，按「策略 + 规则 + 代币」分别冷却。
                if !state
                    .listing
                    .mark_token(&format!("{strategy_id}:{}:{token}", rule.id))
                {
                    tracing::info!(
                        %strategy_id,
                        %token,
                        rule = %rule.name,
                        "该规则已在冷却期内执行过，忽略重复公告"
                    );
                    continue;
                }
                auto_open(
                    state,
                    *strategy_id,
                    rule,
                    rule.execution_mode == StrategyExecutionMode::Paper,
                    ListingAnnouncement {
                        symbol: format!("{token}USDT"),
                        token: token.clone(),
                        headline: item.title.clone(),
                    },
                )
                .await;
            }
        }
    }
}

/// 上币公告跟随的内置模板标识。
const LISTING_TEMPLATE_KEY: &str = "listing-follow";

/// 正在运行的上币公告跟随策略：策略 ID、名称与设置。
pub async fn running_strategies(state: &AppState) -> Vec<(Uuid, String, ListingSettings)> {
    state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .filter(|record| record.status == StrategyStatus::Running)
        .filter(|record| {
            record
                .skill
                .as_ref()
                .map(|skill| skill.template_key == LISTING_TEMPLATE_KEY)
                // 兼容未回填模板标识的历史记录。
                .unwrap_or_else(|| record.definition.name.contains("上币公告跟随"))
        })
        .map(|record| {
            // 技能运行时接管后，状态与手动下单都应以技能设置为准；
            // 旧的监听设置可能停在历史值，直接读会显示错误的下单模式。
            let settings = if crate::skill::manages(record) {
                crate::skill::listing_settings_for(state, record)
            } else {
                state.listing.settings(record.id)
            };
            (record.id, record.definition.name.clone(), settings)
        })
        .collect()
}

/// 电报手动命令使用的设置：优先用保存过的手动参数，其次取运行中上币策略的第一条规则，否则用默认值。
pub async fn manual_settings(state: &AppState) -> ListingRule {
    if let Some(rule) = state.listing.manual_rule() {
        return rule;
    }
    running_strategies(state)
        .await
        .into_iter()
        .map(|(_, _, settings)| settings.first_rule())
        .next()
        .unwrap_or_else(|| state.listing.default_settings().first_rule())
}

/// 按执行交易所优先级依次尝试开仓，直到某一家成交或全部失败。
async fn auto_open(
    state: &AppState,
    strategy_id: Uuid,
    rule: &ListingRule,
    paper: bool,
    announcement: ListingAnnouncement,
) {
    let mut venues = execution_venues(state, &rule.execution_venues);
    // 模拟下单不需要真实账户，按设置里的优先级直接模拟。
    if paper && venues.is_empty() {
        venues = if rule.execution_venues.is_empty() {
            vec!["binance".to_string()]
        } else {
            rule.execution_venues.clone()
        };
    }
    if venues.is_empty() {
        tracing::warn!(
            token = %announcement.token,
            rule = %rule.name,
            "收到上币公告，但没有可执行的交易所账户"
        );
        return;
    }
    let mut failures = Vec::new();
    for venue in &venues {
        let request = OpenListingRequest {
            token: announcement.token.clone(),
            headline: announcement.headline.clone(),
            venue: venue.clone(),
            symbol: announcement.symbol.clone(),
            side: Side::Buy,
            notional_usd: rule.notional_usd,
            leverage: rule.leverage,
            take_profit_pct: rule.take_profit_pct,
            stop_loss_pct: rule.stop_loss_pct,
            strategy_id: Some(strategy_id),
            rule_id: Some(rule.id),
            rule_name: Some(rule.name.clone()),
            system: None,
            group_id: None,
            unit_index: 0,
            n_at_entry: None,
            paper,
            skill: false,
            max_hold_secs: 0,
            notify: true,
        };
        match open_position(state, request).await {
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    token = %announcement.token,
                    %venue,
                    %error,
                    "上币自动开仓失败，按优先级尝试下一家交易所"
                );
                failures.push(format!("{venue}: {error}"));
            }
        }
    }
    let _ = state.events.send(ExecutorEvent::Alert {
        level: AlertLevel::Warning,
        message: format!(
            "上币自动开仓失败 {}：{}",
            announcement.token,
            failures.join("；")
        ),
        message_en: Some(format!(
            "Listing auto-open failed for {}: {}",
            announcement.token,
            failures.join("; ")
        )),
        at: Utc::now(),
    });
}

/// 可下单的交易所：已接入账户的永续交易所，加上同一账户的现货腿。
///
/// 现货腿与永续腿共用同一个交易所账户凭证（例如 `binance-spot` 用 `binance`
/// 账户），因此账户可交易且交易所支持现货时，一并纳入可执行交易所。
pub(crate) fn tradable_venues(state: &AppState) -> Vec<String> {
    let mut perp: Vec<String> = state
        .accounts
        .list()
        .into_iter()
        .filter(|account| account.can_trade)
        .map(|account| account.venue.trim().to_lowercase())
        .filter(|venue| !venue.is_empty())
        .collect();
    perp.sort();
    perp.dedup();

    let mut venues = perp.clone();
    for spot in crate::market::SPOT_VENUES {
        let Some(base) = spot.strip_suffix("-spot") else {
            continue;
        };
        if perp.iter().any(|venue| venue == base) {
            venues.push(spot.to_string());
        }
    }
    venues.sort();
    venues.dedup();
    venues
}

/// 执行交易所：按设置里的优先级排列，只保留当前可下单的交易所。
pub(crate) fn execution_venues(state: &AppState, priority: &[String]) -> Vec<String> {
    let tradable = tradable_venues(state);
    if priority.is_empty() {
        return tradable;
    }
    priority
        .iter()
        .filter(|venue| tradable.contains(venue))
        .cloned()
        .collect()
}

fn news_key(id: &str) -> String {
    format!("news:{id}")
}

async fn evaluate(state: &AppState) {
    let trades = state.listing.open_trades();
    if trades.is_empty() {
        return;
    }
    for trade in trades {
        // 模拟持仓不进入交易所仓位表，只按行情判断止盈止损。
        let held = trade.paper
            || state
                .positions
                .read()
                .expect("仓位锁不可用")
                .iter()
                .any(|position| {
                    position.venue == trade.venue
                        && position.symbol == trade.symbol
                        && position.side == trade.side
                });
        let held_secs = Utc::now()
            .signed_duration_since(trade.opened_at)
            .num_seconds();
        if !held
            && held_secs > MIN_HOLD_BEFORE_MISSING_SECS
            && state.uptime_secs() > STARTUP_GRACE_SECS
        {
            if let Some(closed) = state.listing.update(trade.id, |trade| {
                trade.status = ListingStatus::Closed;
                trade.closed_at = Some(Utc::now());
                trade.note = Some("交易所已无该持仓，按已平仓处理".to_string());
            }) {
                state.store.save_listing_trade(&closed).await;
            }
            continue;
        }

        let Some(price) = mark_price(state, &trade.venue, &trade.symbol).await else {
            continue;
        };
        let Some(reason) = exit_reason(&trade, price) else {
            continue;
        };
        if let Err(error) = close_position(state, trade.id, reason).await {
            tracing::warn!(
                trade = %trade.id,
                symbol = %trade.symbol,
                %error,
                "上币单腿平仓失败，下一轮继续尝试"
            );
        }
    }
}

/// 按持仓方向计算盈亏百分比：做多按价格上涨，做空按价格下跌。
pub(crate) fn pnl_pct(side: Side, entry: Decimal, price: Decimal) -> Decimal {
    if entry <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let diff = if side == Side::Buy {
        price - entry
    } else {
        entry - price
    };
    (diff / entry * Decimal::from(100)).round_dp(2)
}

/// 命中止盈或止损时返回平仓原因。
fn exit_reason(trade: &ListingTrade, price: Decimal) -> Option<CloseReason> {
    if trade.entry_price <= Decimal::ZERO || price <= Decimal::ZERO {
        return None;
    }
    // 技能规则可以配置最长持有时间，到时按超时平仓。
    if trade.max_hold_secs > 0
        && Utc::now()
            .signed_duration_since(trade.opened_at)
            .num_seconds()
            >= trade.max_hold_secs
    {
        return Some(CloseReason::Timeout);
    }
    let change_pct = pnl_pct(trade.side, trade.entry_price, price);
    if trade.take_profit_pct > Decimal::ZERO && change_pct >= trade.take_profit_pct {
        return Some(CloseReason::TakeProfit);
    }
    if trade.stop_loss_pct > Decimal::ZERO && change_pct <= -trade.stop_loss_pct {
        return Some(CloseReason::StopLoss);
    }
    None
}

/// 标记价：优先用缓存/实时盘口的中间价，缺失时向交易所取一次最新盘口。
async fn mark_price(state: &AppState, venue: &str, symbol: &str) -> Option<Decimal> {
    if let Some(quote) = state.market.quote(venue, symbol) {
        let mid = (quote.bid + quote.ask) / Decimal::from(2);
        if mid > Decimal::ZERO {
            return Some(mid);
        }
    }
    state
        .market
        .refresh_quote(venue, symbol)
        .await
        .ok()
        .map(|quote| (quote.bid + quote.ask) / Decimal::from(2))
        .filter(|mid| *mid > Decimal::ZERO)
}

/// 不能当作代币代码的常见词。
const TICKER_STOPWORDS: &[&str] = &[
    "THE",
    "AND",
    "FOR",
    "WITH",
    "FROM",
    "NEW",
    "WILL",
    "LIST",
    "LISTED",
    "LISTING",
    "ADD",
    "ADDS",
    "LAUNCH",
    "LAUNCHES",
    "TRADING",
    "TRADE",
    "PAIR",
    "PAIRS",
    "SPOT",
    "PERP",
    "PERPETUAL",
    "PERPETUALS",
    "FUTURES",
    "CONTRACT",
    "CONTRACTS",
    "USDT",
    "USDC",
    "USD",
    "BINANCE",
    "OKX",
    "BYBIT",
    "BITGET",
    "GATE",
    "MEXC",
    "KUCOIN",
    "UPBIT",
    "HTX",
    "BINGX",
    "ASTER",
    "HYPERLIQUID",
    "LIGHTER",
    "ON",
];

/// 从任意公告文本里识别上币代币代码。
///
/// 优先取括号里的代码（`Predict (PREDICT)`），其次 `$TOKEN`，最后取关键词后的第一个
/// 合法单词。识别不出时返回 `None`，用户可以改用 `/listing open <代币>` 手动发起。
pub fn detect_listing_token(text: &str) -> Option<String> {
    detect_token(text, crate::skill::DEFAULT_EVENT_KEYWORDS)
}

/// 技能规则可以自定义触发关键词；关键词为空时沿用上币关键词。
///
/// 关键词只决定「是否触发」，代币代码仍按括号、`$TOKEN`、关键词后单词的顺序解析；
/// 解析不出代币时跳过该公告，避免对未知标的盲目下单。
pub fn detect_skill_token(text: &str, keywords: &[String]) -> Option<String> {
    let keywords: Vec<&str> = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .collect();
    if keywords.is_empty() {
        return detect_listing_token(text);
    }
    detect_token(text, &keywords)
}

/// 下架、暂停交易、风险提示类公告不参与上币开仓，即使标题里带有 `LISTING` 等词。
const NON_LISTING_PHRASES: &[&str] = &[
    "DELIST",
    "DELISTING",
    "WILL REMOVE",
    "REMOVAL",
    "TRADING SUSPENSION",
    "SUSPEND TRADING",
    "거래지원 종료",
    "유의 종목",
    "유의 촉구",
    "상장 폐지",
    "상장폐지",
    "下架",
    "下线",
    "暂停交易",
];

fn is_non_listing(text: &str) -> bool {
    let upper = text.to_uppercase();
    NON_LISTING_PHRASES
        .iter()
        .any(|phrase| upper.contains(&phrase.to_uppercase()))
}

fn detect_token(text: &str, keywords: &[&str]) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || is_non_listing(text) {
        return None;
    }
    let upper = text.to_uppercase();
    if !keywords
        .iter()
        .any(|keyword| upper.contains(&keyword.to_uppercase()))
    {
        return None;
    }
    // 兼容中文全角括号。
    let normalized = text.replace('（', "(").replace('）', ")");
    if let Some(token) = parenthesized_ticker(&normalized) {
        return Some(token);
    }
    if let Some(token) = dollar_ticker(text) {
        return Some(token);
    }
    after_keyword_ticker(&upper, keywords)
}

fn parenthesized_ticker(text: &str) -> Option<String> {
    for (start, _) in text.match_indices('(') {
        let rest = &text[start + 1..];
        let Some(end) = rest.find(')') else {
            continue;
        };
        if let Some(token) = normalize_ticker(&rest[..end]) {
            return Some(token);
        }
    }
    None
}

fn dollar_ticker(text: &str) -> Option<String> {
    for (index, ch) in text.char_indices() {
        if ch != '$' {
            continue;
        }
        let rest = &text[index + 1..];
        let end = rest
            .find(|value: char| !value.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        if let Some(token) = normalize_ticker(&rest[..end]) {
            return Some(token);
        }
    }
    None
}

/// 取「上币」「WILL LIST」等关键词后的第一个合法单词。
fn after_keyword_ticker(upper: &str, keywords: &[&str]) -> Option<String> {
    // `find` 返回字节下标，关键词长度也用字节数，避免中文关键词按字符数截断。
    let mut positions: Vec<(usize, usize)> = keywords
        .iter()
        .filter_map(|keyword| {
            let keyword = keyword.to_uppercase();
            upper.find(&keyword).map(|start| (start, keyword.len()))
        })
        .collect();
    positions.sort_by_key(|(start, _)| *start);
    for (start, length) in positions {
        let Some(rest) = upper.get(start + length..) else {
            continue;
        };
        for word in rest.split(|value: char| !value.is_ascii_alphanumeric()) {
            if let Some(token) = normalize_ticker(word) {
                return Some(token);
            }
        }
    }
    None
}

/// 规范化代币代码：大写、去掉常见计价后缀、过滤停用词。
fn normalize_ticker(raw: &str) -> Option<String> {
    let value = raw
        .trim()
        .trim_matches(|value: char| !value.is_ascii_alphanumeric())
        .to_ascii_uppercase();
    if value.len() < 2 || value.len() > 12 {
        return None;
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '$')
    {
        return None;
    }
    if !value.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    // 公告里常写成 `XYZUSDT`，去掉计价币后才是代币代码。
    let token = ["USDT", "USDC"]
        .iter()
        .find_map(|suffix| {
            value
                .strip_suffix(suffix)
                .filter(|rest| rest.len() >= 2)
                .map(str::to_string)
        })
        .unwrap_or(value);
    if TICKER_STOPWORDS.contains(&token.as_str()) {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn detects_parenthesized_ticker() {
        assert_eq!(
            detect_listing_token("Binance Will List Predict (PREDICT)"),
            Some("PREDICT".to_string())
        );
        assert_eq!(
            detect_listing_token("币安将上线 XXX（ABC）"),
            Some("ABC".to_string())
        );
    }

    #[test]
    fn detects_ticker_after_keyword() {
        assert_eq!(
            detect_listing_token("Binance Will List ZKJ on Spot"),
            Some("ZKJ".to_string())
        );
        assert_eq!(
            detect_listing_token("币安将上线 XYZ 永续合约"),
            Some("XYZ".to_string())
        );
    }

    #[test]
    fn strips_quote_suffix_and_ignores_non_listing_text() {
        assert_eq!(
            detect_listing_token("Bitget Lists FISVUSDT Stock Perp"),
            Some("FISV".to_string())
        );
        assert_eq!(detect_listing_token("BTC 今天涨了 5%"), None);
    }

    #[test]
    fn legacy_flat_settings_fall_back_to_one_rule() {
        // 旧版本把参数平铺在 ListingSettings 上，升级后应回落成一条默认规则而不是丢配置。
        let legacy: ListingSettings = serde_json::from_str(
            r#"{"notional_usd":"100","leverage":"3","venues":["okx"],"take_profit_pct":"20","stop_loss_pct":"10"}"#,
        )
        .unwrap();
        let settings = legacy.normalized(dec!(5));
        assert_eq!(settings.rules.len(), 1);
        assert_eq!(settings.rules[0].leverage, dec!(5));
        assert!(settings.rules[0].enabled);
        assert!(!settings.rules[0].id.is_nil());
    }

    #[test]
    fn keeps_multiple_rules_and_assigns_ids() {
        let parsed: ListingSettings = serde_json::from_str(
            r#"{"rules":[{"name":"币安 → OKX","watch_venues":["binance"],"execution_venues":["okx"]},{"name":"Upbit → Bybit","enabled":false,"watch_venues":["upbit"],"execution_venues":["bybit"]}]}"#,
        )
        .unwrap();
        let settings = parsed.normalized(dec!(5));
        assert_eq!(settings.rules.len(), 2);
        assert_eq!(settings.rules[0].name, "币安 → OKX");
        assert!(settings.rules[0].enabled);
        assert!(!settings.rules[1].enabled);
        assert_eq!(
            settings.rules[1].execution_venues,
            vec!["bybit".to_string()]
        );
        // 手动下单优先取启用的规则。
        assert_eq!(settings.first_rule().name, "币安 → OKX");
        assert_ne!(settings.rules[0].id, settings.rules[1].id);
    }

    #[test]
    fn exit_reason_hits_take_profit_and_stop_loss() {
        let trade = ListingTrade {
            id: Uuid::new_v4(),
            token: "XYZ".into(),
            headline: String::new(),
            venue: "bitget".into(),
            symbol: "XYZUSDT".into(),
            side: Side::Buy,
            notional_usd: dec!(100),
            leverage: Decimal::ONE,
            quantity: dec!(10),
            entry_price: dec!(10),
            take_profit_pct: dec!(20),
            stop_loss_pct: dec!(10),
            status: ListingStatus::Open,
            entry_order_id: None,
            close_order_id: None,
            close_price: None,
            paper: false,
            skill: false,
            max_hold_secs: 0,
            notify: true,
            strategy_id: None,
            rule_id: None,
            source: None,
            system: None,
            group_id: None,
            unit_index: 0,
            n_at_entry: None,
            note: None,
            opened_at: Utc::now(),
            closed_at: None,
            updated_at: Utc::now(),
        };
        assert_eq!(exit_reason(&trade, dec!(11)), None);
        assert_eq!(exit_reason(&trade, dec!(12)), Some(CloseReason::TakeProfit));
        assert_eq!(exit_reason(&trade, dec!(9)), Some(CloseReason::StopLoss));
    }
}
