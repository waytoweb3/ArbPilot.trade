//! 技能策略（SKILL.md 参数化）。
//!
//! 一份 `SKILL.md` 由六个部分组成：技能用途、什么时候调用、执行步骤、业务规则、
//! 输出格式与注意事项。这里把这些章节映射成结构化参数，由执行器按参数判断触发条件、
//! 通过统一风控网关下单；止盈止损沿用单腿交易监控。
//!
//! 「什么时候调用」支持四种触发方式：
//! - `event`：交易所公告（上币公告跟随、技能策略）；
//! - `breakout`：唐奇安通道突破（海龟突破）；
//! - `ma_trend`：快慢均线趋势（均线趋势）；
//! - `grid`：围绕锚定价的本地网格档位（网格）。
//!
//! 四种方式共用同一套六章参数与单腿交易记录，因此同一个执行器可以同时运行多条
//! 互不干扰的技能规则。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use arb_core::{
    indicators, DataRules, SandboxCandle, Side, StrategyExecutionMode, StrategyRecord,
    StrategyStatus,
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    events::{AlertLevel, ExecutorEvent},
    listing::{self, CloseReason, OpenListingRequest},
    state::AppState,
    store::Store,
};

/// 公告扫描与持仓检查的节拍。
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// 指标类技能的评估节拍：K 线周期最短 1 分钟，无需跟公告一样频繁。
pub const INDICATOR_INTERVAL_SECS: i64 = 60;
const INDICATOR_INTERVAL: Duration = Duration::from_secs(INDICATOR_INTERVAL_SECS as u64);
/// 每轮扫描的最新公告条数。
const NEWS_SCAN_LIMIT: usize = 40;
/// 事件窗口的合法范围，与 `DataRules.event_window_secs` 保持一致。
pub const MIN_EVENT_WINDOW_SECS: i64 = 60;
pub const MAX_EVENT_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;
/// 网格档位数量上限，避免配置失误一次性挂出过多档位。
pub const MAX_GRID_LEVELS: u32 = 20;

/// 公告类技能的内置触发关键词。
///
/// 新规则会把它填进 `event_keywords` 并随策略设置一起持久化，界面可以直接增删，
/// 调整「哪些公告算上币消息」不需要改代码。
pub const DEFAULT_EVENT_KEYWORDS: &[&str] = &[
    "WILL LIST",
    "WILL BE LISTED",
    "TO LIST",
    "LISTING",
    "LISTS ",
    "NEW LISTING",
    "上币",
    "上线",
    "新增交易",
    "开通交易",
];

/// 内置触发关键词的可变副本，用于给新规则与旧配置补齐参数。
pub fn default_event_keywords() -> Vec<String> {
    DEFAULT_EVENT_KEYWORDS
        .iter()
        .map(|keyword| keyword.to_string())
        .collect()
}

/// 技能被触发后输出什么；指标类技能固定按信号开仓，只有公告类技能会用到通知动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillAction {
    /// 单腿市价开仓，成交后按业务规则止盈止损。
    #[default]
    OpenLong,
    /// 只发送提醒，不下单。
    NotifyOnly,
}

/// 技能的触发方式，对应「什么时候调用」这一章的具体形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    /// 交易所公告等事件。
    #[default]
    Event,
    /// 唐奇安通道突破。
    Breakout,
    /// 快慢均线趋势。
    MaTrend,
    /// 围绕锚定价的网格档位。
    Grid,
}

/// 允许开仓的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDirection {
    #[default]
    Both,
    Long,
    Short,
}

/// 「什么时候调用」：公告条件与指标参数都放在这一章。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillTrigger {
    /// 公告来源；为空表示全部来源。
    pub event_sources: Vec<String>,
    /// 触发关键词；默认是内置上币关键词，可增删。
    pub event_keywords: Vec<String>,
    /// 公告在该窗口内才允许触发（秒）。
    pub event_window_secs: i64,
    /// K 线周期，例如 `1h`。
    pub timeframe: String,
    /// 拉取的 K 线根数，必须覆盖最长的指标窗口。
    pub lookback: u32,
    /// 快均线周期。
    pub fast: u32,
    /// 慢均线周期。
    pub slow: u32,
    /// ATR 周期。
    pub atr: u32,
    /// 唐奇安通道周期。
    pub channel: u32,
    /// 止损距离 = ATR × 该倍数；0 表示不启用 ATR 止损。
    pub atr_multiple: Decimal,
}

impl Default for SkillTrigger {
    fn default() -> Self {
        Self {
            event_sources: Vec::new(),
            event_keywords: default_event_keywords(),
            event_window_secs: 3_600,
            timeframe: "1h".to_string(),
            lookback: 200,
            fast: 20,
            slow: 55,
            atr: 20,
            channel: 20,
            atr_multiple: Decimal::from(2),
        }
    }
}

/// 「执行步骤」：下单模式、执行交易所优先级与允许方向。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillSteps {
    /// 真实提交交易所，或只记录模拟成交。
    pub execution_mode: StrategyExecutionMode,
    /// 执行交易所，按优先级从高到低；为空表示全部可交易交易所。
    pub execution_venues: Vec<String>,
    /// 指标类技能允许的开仓方向；公告类技能固定做多。
    pub direction: SkillDirection,
}

impl Default for SkillSteps {
    fn default() -> Self {
        Self {
            execution_mode: StrategyExecutionMode::Real,
            execution_venues: Vec::new(),
            direction: SkillDirection::Long,
        }
    }
}

/// 「业务规则」：金额、杠杆、止盈止损与网格档位。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillBusinessRules {
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    /// 止盈百分比（20 表示 +20%）；0 表示不启用。
    pub take_profit_pct: Decimal,
    /// 止损百分比（10 表示 -10%）；0 表示不启用。
    pub stop_loss_pct: Decimal,
    /// 网格单边档位数。
    pub grid_levels: u32,
    /// 网格相邻档位间距（百分比，0.5 表示 0.5%）。
    pub grid_spacing_pct: Decimal,
    /// 网格每档使用下单金额的百分比（25 表示 25%）。
    pub grid_share_pct: Decimal,
}

impl Default for SkillBusinessRules {
    fn default() -> Self {
        Self {
            notional_usd: Decimal::from(100),
            leverage: Decimal::ONE,
            take_profit_pct: Decimal::from(20),
            stop_loss_pct: Decimal::from(10),
            grid_levels: 3,
            grid_spacing_pct: Decimal::new(5, 1),
            grid_share_pct: Decimal::from(25),
        }
    }
}

/// 「输出格式」：触发后做什么、是否发送通知。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillOutput {
    pub action: SkillAction,
    pub notify: bool,
}

impl Default for SkillOutput {
    fn default() -> Self {
        Self {
            action: SkillAction::OpenLong,
            notify: true,
        }
    }
}

/// 「注意事项」：额外的风控上限。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillCautions {
    /// 单笔金额上限；0 表示只受策略与执行器风控约束。
    pub max_notional_usd: Decimal,
    /// 每条规则每天最多开仓次数；0 表示不限。
    pub max_daily_opens: u32,
}

impl Default for SkillCautions {
    fn default() -> Self {
        Self {
            max_notional_usd: Decimal::ZERO,
            max_daily_opens: 0,
        }
    }
}

/// 一条技能规则，对应一份 SKILL.md。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillRule {
    pub id: Uuid,
    /// 技能名称，便于在界面与运行记录里区分。
    pub name: String,
    pub enabled: bool,
    /// 技能用途：给人看的说明，也用于运行实例快照。
    pub purpose: String,
    /// 触发方式：公告、通道突破、均线趋势或网格。
    pub kind: SkillKind,
    /// 什么时候调用。
    pub trigger: SkillTrigger,
    /// 执行步骤。
    pub steps: SkillSteps,
    /// 业务规则。
    pub business: SkillBusinessRules,
    /// 输出格式。
    pub output: SkillOutput,
    /// 注意事项。
    pub cautions: SkillCautions,
}

impl Default for SkillRule {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            name: String::new(),
            enabled: true,
            purpose: String::new(),
            kind: SkillKind::Event,
            trigger: SkillTrigger::default(),
            steps: SkillSteps::default(),
            business: SkillBusinessRules::default(),
            output: SkillOutput::default(),
            cautions: SkillCautions::default(),
        }
    }
}

/// 一个技能策略实例的全部技能规则。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillSettings {
    pub rules: Vec<SkillRule>,
}

impl SkillSettings {
    /// 缺规则的旧配置回落到一条默认规则，并补齐规则 ID。
    fn normalized(mut self, leverage: Decimal) -> Self {
        if self.rules.is_empty() {
            self.rules.push(SkillRule {
                business: SkillBusinessRules {
                    leverage,
                    ..SkillBusinessRules::default()
                },
                ..SkillRule::default()
            });
        }
        for rule in &mut self.rules {
            if rule.id.is_nil() {
                rule.id = Uuid::new_v4();
            }
            // 旧配置里的空关键词等价于内置词表，补进参数后界面才能看到并修改它们。
            if rule.kind == SkillKind::Event
                && rule
                    .trigger
                    .event_keywords
                    .iter()
                    .all(|keyword| keyword.trim().is_empty())
            {
                rule.trigger.event_keywords = default_event_keywords();
            }
            // 「只通知不下单」已从输出格式移除，迁移为模拟开仓：
            // 既不会真实下单，也不会出现界面显示开仓而运行期不下单。
            if rule.output.action == SkillAction::NotifyOnly {
                rule.output.action = SkillAction::OpenLong;
                rule.steps.execution_mode = StrategyExecutionMode::Paper;
            }
        }
        self
    }

    /// 界面摘要：符合条件的规则名称与触发条件。
    pub fn enabled_rules(&self) -> impl Iterator<Item = &SkillRule> {
        self.rules.iter().filter(|rule| rule.enabled)
    }
}

/// 去掉模板实例名上的附加信息：重名自动编号「名称 2」与自定义后缀「名称 · 交易对」。
fn base_name(name: &str) -> &str {
    let head = name.trim().split(" · ").next().unwrap_or(name).trim();
    match head.rsplit_once(' ') {
        Some((base, suffix))
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) =>
        {
            base.trim()
        }
        _ => head,
    }
}

/// 内置模板标识 → 技能触发方式；模板实例用稳定标识识别，改策略名不受影响。
pub fn kind_from_template(key: &str) -> Option<SkillKind> {
    match key.trim() {
        "listing-follow" | "skill" => Some(SkillKind::Event),
        "turtle-breakout" => Some(SkillKind::Breakout),
        "ma-trend" => Some(SkillKind::MaTrend),
        "grid" => Some(SkillKind::Grid),
        _ => None,
    }
}

/// 旧记录的模板名称 → 技能触发方式，仅用于升级前的历史数据。
pub fn kind_from_name(name: &str) -> Option<SkillKind> {
    match base_name(name) {
        "上币公告跟随" | "技能策略（SKILL.md）" => Some(SkillKind::Event),
        "海龟突破" => Some(SkillKind::Breakout),
        "均线趋势" => Some(SkillKind::MaTrend),
        "网格" => Some(SkillKind::Grid),
        _ => None,
    }
}

/// 记录的技能触发方式：优先模板标识，兼容未回填的历史记录。
fn kind_for(record: &StrategyRecord) -> Option<SkillKind> {
    record
        .skill
        .as_ref()
        .and_then(|skill| kind_from_template(&skill.template_key))
        .or_else(|| kind_from_name(&record.definition.name))
}

/// 是否是技能运行时接管的策略；沙箱模块对它们不参与运行。
pub fn manages(record: &StrategyRecord) -> bool {
    kind_for(record).is_some()
}

/// 按触发方式生成一条默认规则，参数与改造前的内置模板保持一致。
pub fn default_settings(kind: SkillKind, leverage: Decimal) -> SkillSettings {
    let mut rule = SkillRule {
        business: SkillBusinessRules {
            leverage,
            ..SkillBusinessRules::default()
        },
        ..SkillRule::default()
    };
    rule.kind = kind;
    match kind {
        SkillKind::Event => {
            rule.name = "公告事件".to_string();
            rule.purpose = "监听交易所公告，识别代币后单腿开仓。".to_string();
        }
        SkillKind::Breakout => {
            rule.name = "海龟突破".to_string();
            rule.purpose = "价格突破唐奇安通道入场，反向突破或 2 倍 ATR 止损离场。".to_string();
            rule.trigger.timeframe = "1h".to_string();
            rule.trigger.lookback = 300;
            rule.trigger.fast = 20;
            rule.trigger.slow = 55;
            rule.trigger.atr = 20;
            rule.trigger.channel = 20;
            rule.trigger.atr_multiple = Decimal::from(2);
            rule.business.take_profit_pct = Decimal::ZERO;
            rule.business.stop_loss_pct = Decimal::ZERO;
        }
        SkillKind::MaTrend => {
            rule.name = "均线趋势".to_string();
            rule.purpose = "快慢均线多头排列做多、空头排列做空，均线反转离场。".to_string();
            rule.trigger.timeframe = "1h".to_string();
            rule.trigger.lookback = 200;
            rule.trigger.fast = 20;
            rule.trigger.slow = 55;
            rule.business.take_profit_pct = Decimal::ZERO;
            rule.business.stop_loss_pct = Decimal::ZERO;
        }
        SkillKind::Grid => {
            rule.name = "网格".to_string();
            rule.purpose =
                "现价上下按固定间距布置档位，价格触及后单腿开仓，到达下一档止盈。".to_string();
            rule.trigger.timeframe = "5m".to_string();
            rule.trigger.lookback = 200;
            rule.business.take_profit_pct = Decimal::new(5, 1);
            rule.business.stop_loss_pct = Decimal::ZERO;
            rule.business.grid_levels = 3;
            rule.business.grid_spacing_pct = Decimal::new(5, 1);
            rule.business.grid_share_pct = Decimal::from(25);
        }
    }
    SkillSettings { rules: vec![rule] }
}

/// 把上币公告跟随的历史设置转成技能参数，升级后行为保持不变。
fn from_listing(settings: &listing::ListingSettings) -> SkillSettings {
    SkillSettings {
        rules: settings
            .rules
            .iter()
            .map(|rule| SkillRule {
                id: rule.id,
                name: rule.name.clone(),
                enabled: rule.enabled,
                purpose: "监听上币公告，识别代币后单腿开仓。".to_string(),
                kind: SkillKind::Event,
                trigger: SkillTrigger {
                    event_sources: rule.watch_venues.clone(),
                    event_keywords: default_event_keywords(),
                    event_window_secs: 3_600,
                    ..SkillTrigger::default()
                },
                steps: SkillSteps {
                    execution_mode: rule.execution_mode,
                    execution_venues: rule.execution_venues.clone(),
                    direction: SkillDirection::Long,
                },
                business: SkillBusinessRules {
                    notional_usd: rule.notional_usd,
                    leverage: rule.leverage,
                    take_profit_pct: rule.take_profit_pct,
                    stop_loss_pct: rule.stop_loss_pct,
                    ..SkillBusinessRules::default()
                },
                output: SkillOutput::default(),
                cautions: SkillCautions::default(),
            })
            .collect(),
    }
}

/// 网格档位状态：锚定价与两侧成交标记。
struct GridState {
    anchor: Decimal,
    buys: Vec<bool>,
    sells: Vec<bool>,
}

impl GridState {
    fn new(anchor: Decimal, levels: usize) -> Self {
        Self {
            anchor,
            buys: vec![false; levels],
            sells: vec![false; levels],
        }
    }
}

/// 技能设置与去重状态；持久化由 `Store` 负责。
pub struct SkillDesk {
    store: Arc<Store>,
    settings: RwLock<BTreeMap<String, SkillSettings>>,
    /// 未保存过设置时使用的默认杠杆（风控杠杆上限）。
    default_leverage: Decimal,
    /// 已处理过的公告，避免同一条新闻重复触发。
    seen: Mutex<HashSet<String>>,
    /// 每条规则当天的开仓次数。
    daily: Mutex<HashMap<String, (String, u32)>>,
    /// 网格档位：键为「策略 + 规则 + 交易对」。
    grids: Mutex<HashMap<String, GridState>>,
}

impl SkillDesk {
    pub fn start(
        store: Arc<Store>,
        settings: BTreeMap<String, SkillSettings>,
        default_leverage: Decimal,
    ) -> Arc<Self> {
        let settings = settings
            .into_iter()
            .map(|(id, settings)| (id, settings.normalized(default_leverage)))
            .collect();
        Arc::new(Self {
            store,
            settings: RwLock::new(settings),
            default_leverage,
            seen: Mutex::new(HashSet::new()),
            daily: Mutex::new(HashMap::new()),
            grids: Mutex::new(HashMap::new()),
        })
    }

    /// 未保存过设置时的默认值：一条公告规则，杠杆跟随风控上限。
    pub fn default_settings(&self) -> SkillSettings {
        default_settings(SkillKind::Event, self.default_leverage)
    }

    /// 风控允许的最大杠杆，供调用方拼默认值。
    pub fn default_leverage(&self) -> Decimal {
        self.default_leverage
    }

    /// 指定策略的设置；未保存过时用调用方给出的默认值（按触发方式区分）。
    pub fn settings_or(
        &self,
        strategy_id: Uuid,
        defaults: impl FnOnce() -> SkillSettings,
    ) -> SkillSettings {
        self.settings
            .read()
            .expect("技能设置锁不可用")
            .get(&strategy_id.to_string())
            .cloned()
            .unwrap_or_else(|| defaults().normalized(self.default_leverage))
    }

    /// 保存某个策略的设置，返回保存后的完整配置。
    pub async fn update_settings(&self, strategy_id: Uuid, next: SkillSettings) -> SkillSettings {
        let next = next.normalized(self.default_leverage);
        let snapshot = {
            let mut settings = self.settings.write().expect("技能设置锁不可用");
            settings.insert(strategy_id.to_string(), next.clone());
            settings.clone()
        };
        self.store.save_skill_settings(&snapshot).await;
        next
    }

    /// 记录一次公告；返回 true 表示这是第一次看到该公告。
    fn mark_seen(&self, key: &str) -> bool {
        self.seen
            .lock()
            .expect("技能公告去重锁不可用")
            .insert(key.to_string())
    }

    /// 当天开仓次数是否还在上限内。
    fn allow_daily(&self, key: &str, limit: u32) -> bool {
        if limit == 0 {
            return true;
        }
        let today = Utc::now().date_naive().to_string();
        let mut daily = self.daily.lock().expect("技能开仓计数锁不可用");
        let entry = daily
            .entry(key.to_string())
            .or_insert_with(|| (today.clone(), 0));
        if entry.0 != today {
            *entry = (today, 0);
        }
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// 计算本轮触发的网格档位；首次或全部档位成交后按现价重建。
    fn grid_triggers(&self, key: &str, price: Decimal, rule: &SkillRule) -> Vec<(Side, Decimal)> {
        let levels = rule.business.grid_levels.clamp(1, MAX_GRID_LEVELS) as usize;
        let step = rule.business.grid_spacing_pct / Decimal::from(100);
        if step <= Decimal::ZERO || price <= Decimal::ZERO {
            return Vec::new();
        }
        let mut grids = self.grids.lock().expect("技能网格状态锁不可用");
        let state = grids
            .entry(key.to_string())
            .or_insert_with(|| GridState::new(price, levels));
        if state.buys.len() != levels {
            *state = GridState::new(price, levels);
        }
        if state.buys.iter().all(|filled| *filled) && state.sells.iter().all(|filled| *filled) {
            *state = GridState::new(price, levels);
        }
        let anchor = state.anchor;
        let (buy_levels, sell_levels) = grid_levels(
            anchor,
            rule.business.grid_levels,
            rule.business.grid_spacing_pct,
        );
        let mut hits = Vec::new();
        if rule.steps.direction != SkillDirection::Short {
            for (index, filled) in state.buys.iter_mut().enumerate() {
                if *filled {
                    continue;
                }
                let Some(level) = buy_levels.get(index).copied() else {
                    continue;
                };
                if price <= level {
                    *filled = true;
                    hits.push((Side::Buy, level));
                }
            }
        }
        if rule.steps.direction != SkillDirection::Long {
            for (index, filled) in state.sells.iter_mut().enumerate() {
                if *filled {
                    continue;
                }
                let Some(level) = sell_levels.get(index).copied() else {
                    continue;
                };
                if price >= level {
                    *filled = true;
                    hits.push((Side::Sell, level));
                }
            }
        }
        hits
    }
}

/// 网格档位价：围绕锚定价按固定百分比间距展开，返回（买档、卖档），都由近到远。
///
/// 抽成纯函数便于校验器用固定输入核对档位计算，不必启动行情与下单链路。
pub fn grid_levels(
    anchor: Decimal,
    levels: u32,
    spacing_pct: Decimal,
) -> (Vec<Decimal>, Vec<Decimal>) {
    let levels = levels.clamp(1, MAX_GRID_LEVELS);
    let step = spacing_pct / Decimal::from(100);
    if step <= Decimal::ZERO || anchor <= Decimal::ZERO {
        return (Vec::new(), Vec::new());
    }
    let buys = (1..=levels)
        .map(|index| anchor * (Decimal::ONE - step * Decimal::from(index)))
        .collect();
    let sells = (1..=levels)
        .map(|index| anchor * (Decimal::ONE + step * Decimal::from(index)))
        .collect();
    (buys, sells)
}

/// 正在运行的技能策略快照。
struct SkillStrategy {
    id: Uuid,
    venues: Vec<String>,
    symbols: Vec<String>,
    settings: SkillSettings,
}

/// 策略没有保存过技能参数时，按触发方式给出一份默认设置。
fn fallback_settings(state: &AppState, record: &StrategyRecord) -> SkillSettings {
    let leverage = if record.definition.execution.leverage > Decimal::ZERO {
        record.definition.execution.leverage
    } else {
        state.skill.default_leverage()
    };
    match kind_for(record) {
        Some(SkillKind::Event) if uses_listing_settings(record) => {
            from_listing(&state.listing.settings(record.id))
        }
        Some(kind) => default_settings(kind, leverage),
        None => default_settings(SkillKind::Event, leverage),
    }
}

/// 上币公告跟随沿用历史的上币监听设置，其余事件类技能使用默认事件参数。
fn uses_listing_settings(record: &StrategyRecord) -> bool {
    match record.skill.as_ref() {
        Some(skill) => skill.template_key == "listing-follow",
        None => record.definition.name.trim() == "上币公告跟随",
    }
}

/// 指定策略实际生效的技能设置：已保存的优先，否则按触发方式回落到默认值。
///
/// 上币公告跟随的历史设置会在这里迁移成事件类技能参数，升级后行为保持不变。
pub fn settings_for(state: &AppState, record: &StrategyRecord) -> SkillSettings {
    state
        .skill
        .settings_or(record.id, || fallback_settings(state, record))
}

/// 上币公告跟随策略实际生效的单腿参数。
///
/// 技能运行时接管后以技能设置为准：旧的监听设置可能停留在历史值，
/// 直接读它会让电报状态显示的下单模式与真实运行不一致。
pub fn listing_settings_for(state: &AppState, record: &StrategyRecord) -> listing::ListingSettings {
    listing::ListingSettings {
        rules: settings_for(state, record)
            .rules
            .into_iter()
            .map(|rule| listing::ListingRule {
                id: rule.id,
                name: rule.name,
                enabled: rule.enabled,
                execution_mode: rule.steps.execution_mode,
                notional_usd: rule.business.notional_usd,
                leverage: rule.business.leverage,
                watch_venues: rule.trigger.event_sources,
                execution_venues: rule.steps.execution_venues,
                take_profit_pct: rule.business.take_profit_pct,
                stop_loss_pct: rule.business.stop_loss_pct,
            })
            .collect(),
    }
}

/// 正在运行的技能策略：由模板标识识别，避免误接管用户自定义策略。
async fn running_strategies(state: &AppState) -> Vec<SkillStrategy> {
    let records: Vec<StrategyRecord> = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .filter(|record| record.status == StrategyStatus::Running)
        .filter(|record| manages(record))
        .cloned()
        .collect();
    records
        .into_iter()
        .map(|record| {
            let settings = settings_for(state, &record);
            SkillStrategy {
                id: record.id,
                venues: record.definition.universe.venues.clone(),
                symbols: record.definition.universe.symbols.clone(),
                settings,
            }
        })
        .collect()
}

/// 启动技能扫描：公告每 5 秒一次，指标类技能每分钟一次。
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 首次扫描前先记录当前缓存里的公告，只有之后出现的新公告才触发技能。
        let mut baselined = false;
        let mut last_indicator = Instant::now();
        loop {
            ticker.tick().await;
            if state.news.primed() {
                if baselined {
                    scan_news(&state).await;
                } else {
                    for item in state.news.snapshot(NEWS_SCAN_LIMIT).items {
                        state.skill.mark_seen(&news_key(&item.id));
                    }
                    baselined = true;
                }
            }
            if last_indicator.elapsed() >= INDICATOR_INTERVAL {
                last_indicator = Instant::now();
                scan_indicators(&state).await;
            }
        }
    });
}

fn news_key(id: &str) -> String {
    format!("skill:{id}")
}

/// 扫描最新公告：每条公告类技能规则按自己的来源、关键词与窗口独立判断。
async fn scan_news(state: &AppState) {
    let strategies = running_strategies(state).await;
    if strategies.is_empty() {
        return;
    }
    for item in state.news.snapshot(NEWS_SCAN_LIMIT).items {
        if !state.skill.mark_seen(&news_key(&item.id)) {
            continue;
        }
        let age_secs = Utc::now()
            .signed_duration_since(item.published_at)
            .num_seconds()
            .max(0);
        for strategy in &strategies {
            for rule in strategy.settings.enabled_rules() {
                if rule.kind != SkillKind::Event {
                    continue;
                }
                if !rule.trigger.event_sources.is_empty()
                    && !rule
                        .trigger
                        .event_sources
                        .iter()
                        .any(|source| source.eq_ignore_ascii_case(&item.source))
                {
                    continue;
                }
                if age_secs > rule.trigger.event_window_secs {
                    continue;
                }
                let Some(token) =
                    listing::detect_skill_token(&item.title, &rule.trigger.event_keywords)
                else {
                    continue;
                };
                let key = format!("{}:{}:{token}", strategy.id, rule.id);
                if rule.output.action == SkillAction::NotifyOnly {
                    notify_hit(state, rule, &token, &item.title);
                    continue;
                }
                if !state.skill.allow_daily(&key, rule.cautions.max_daily_opens) {
                    tracing::info!(
                        strategy_id = %strategy.id,
                        %token,
                        rule = %rule.name,
                        "技能规则已达到当日开仓上限，忽略本次触发"
                    );
                    continue;
                }
                auto_open(state, strategy, rule, &token, &item.title).await;
            }
        }
    }
}

/// 只提醒不下单：输出一条与技能同名的告警。
fn notify_hit(state: &AppState, rule: &SkillRule, token: &str, headline: &str) {
    if !rule.output.notify {
        return;
    }
    let name = if rule.name.trim().is_empty() {
        "技能策略"
    } else {
        rule.name.trim()
    };
    let _ = state.events.send(ExecutorEvent::Alert {
        level: AlertLevel::Info,
        message: format!("技能命中：{name} 触发 {token}（{headline}）"),
        message_en: Some(format!(
            "Skill matched: {name} triggered {token} ({headline})"
        )),
        at: Utc::now(),
    });
}

/// 按执行交易所优先级尝试开仓；金额受技能注意事项与统一风控共同约束。
async fn auto_open(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    token: &str,
    headline: &str,
) {
    let venues = candidate_venues(state, rule, &strategy.venues);
    if venues.is_empty() {
        tracing::warn!(
            %token,
            rule = %rule.name,
            "技能已触发，但没有可执行的交易所账户"
        );
        return;
    }
    let notional = capped_notional(rule);
    let paper = rule.steps.execution_mode == StrategyExecutionMode::Paper;
    let mut failures = Vec::new();
    for venue in &venues {
        let request = OpenListingRequest {
            token: token.to_string(),
            headline: headline.to_string(),
            venue: venue.clone(),
            symbol: format!("{token}USDT"),
            side: Side::Buy,
            notional_usd: notional,
            leverage: rule.business.leverage,
            take_profit_pct: rule.business.take_profit_pct,
            stop_loss_pct: rule.business.stop_loss_pct,
            strategy_id: Some(strategy.id),
            rule_id: Some(rule.id),
            rule_name: Some(rule.name.clone()),
            paper,
            skill: true,
            max_hold_secs: 0,
            notify: rule.output.notify,
        };
        match listing::open_position(state, request).await {
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    %token,
                    %venue,
                    %error,
                    "技能自动开仓失败，按优先级尝试下一家交易所"
                );
                failures.push(format!("{venue}: {error}"));
            }
        }
    }
    let message = format!("技能自动开仓失败 {token}：{}", failures.join("；"));
    crate::strategy_runner::note_reject(state, strategy.id, message.clone()).await;
    let _ = state.events.send(ExecutorEvent::Alert {
        level: AlertLevel::Warning,
        message,
        message_en: Some(format!(
            "Skill auto-open failed for {token}: {}",
            failures.join("; ")
        )),
        at: Utc::now(),
    });
}

/// 可执行的交易所：优先按规则设置的优先级，模拟模式允许使用未配置账户的交易所。
pub(crate) fn candidate_venues(
    state: &AppState,
    rule: &SkillRule,
    universe: &[String],
) -> Vec<String> {
    let mut venues = listing::execution_venues(state, &rule.steps.execution_venues);
    if rule.steps.execution_mode == StrategyExecutionMode::Paper && venues.is_empty() {
        venues = if rule.steps.execution_venues.is_empty() {
            universe.to_vec()
        } else {
            rule.steps.execution_venues.clone()
        };
        if venues.is_empty() {
            venues.push("binance".to_string());
        }
    }
    venues
}

/// 单笔金额：业务规则金额与注意事项上限取小。
pub(crate) fn capped_notional(rule: &SkillRule) -> Decimal {
    if rule.cautions.max_notional_usd > Decimal::ZERO {
        rule.business
            .notional_usd
            .min(rule.cautions.max_notional_usd)
    } else {
        rule.business.notional_usd
    }
}

/// 指标类技能：通道突破、均线趋势与网格共用 K 线与单腿交易。
async fn scan_indicators(state: &AppState) {
    let strategies = running_strategies(state).await;
    for strategy in &strategies {
        for rule in strategy.settings.enabled_rules() {
            if rule.kind == SkillKind::Event {
                continue;
            }
            let symbols = if strategy.symbols.is_empty() {
                state.config.market_symbols.clone()
            } else {
                strategy.symbols.clone()
            };
            for symbol in symbols {
                if let Err(error) = evaluate_indicator(state, strategy, rule, &symbol).await {
                    tracing::debug!(
                        %symbol,
                        rule = %rule.name,
                        %error,
                        "技能指标评估失败，下一轮重试"
                    );
                }
            }
        }
    }
}

/// 评估单个交易对的指标信号。
async fn evaluate_indicator(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    symbol: &str,
) -> Result<(), String> {
    let venues = candidate_venues(state, rule, &strategy.venues);
    let Some(venue) = venues
        .into_iter()
        .find(|venue| state.market.quote(venue, symbol).is_some())
    else {
        return Ok(());
    };
    let data = DataRules {
        timeframe: rule.trigger.timeframe.clone(),
        lookback: rule.trigger.lookback.clamp(3, 1_000),
        fast: rule.trigger.fast.max(2),
        slow: rule.trigger.slow.max(2),
        atr: rule.trigger.atr.max(2),
        channel: rule.trigger.channel.max(2),
        ..DataRules::default()
    };
    let candles = state
        .market
        .candles(&venue, symbol, &data.timeframe, data.lookback)
        .await
        .map_err(|error| error.to_string())?;
    if candles.len() < 3 {
        return Ok(());
    }
    let snapshots: Vec<SandboxCandle> = candles
        .into_iter()
        .map(|candle| SandboxCandle {
            open_time: candle.open_time,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
            volume: candle.volume,
        })
        .collect();
    let indicators = indicators::compute(&snapshots, &data);
    let last = snapshots
        .last()
        .map(|candle| candle.close)
        .unwrap_or(Decimal::ZERO);
    if last <= Decimal::ZERO {
        return Ok(());
    }
    let open: Vec<listing::ListingTrade> = state
        .listing
        .open_trades()
        .into_iter()
        .filter(|trade| {
            trade.strategy_id == Some(strategy.id)
                && trade.rule_id == Some(rule.id)
                && trade.symbol.eq_ignore_ascii_case(symbol)
        })
        .collect();

    match rule.kind {
        SkillKind::Breakout => {
            evaluate_breakout(
                state,
                strategy,
                rule,
                &venue,
                symbol,
                last,
                &indicators,
                &open,
            )
            .await
        }
        SkillKind::MaTrend => {
            evaluate_ma_trend(
                state,
                strategy,
                rule,
                &venue,
                symbol,
                last,
                &indicators,
                &open,
            )
            .await
        }
        SkillKind::Grid => evaluate_grid(state, strategy, rule, &venue, symbol, last, &open).await,
        SkillKind::Event => Ok(()),
    }
}

/// 唐奇安突破信号：突破上轨做多，跌破下轨做空；方向限制在这里统一生效。
///
/// 抽成纯函数便于校验器用固定输入核对信号，不必启动行情与下单链路。
pub fn breakout_signal(
    last: Decimal,
    high: Decimal,
    low: Decimal,
    direction: SkillDirection,
) -> Option<Side> {
    if last > high && direction != SkillDirection::Short {
        Some(Side::Buy)
    } else if last < low && direction != SkillDirection::Long {
        Some(Side::Sell)
    } else {
        None
    }
}

/// 均线趋势信号：快线在慢线上方且价格站上快线做多，反之做空。
pub fn ma_trend_signal(
    last: Decimal,
    fast: Decimal,
    slow: Decimal,
    direction: SkillDirection,
) -> Option<Side> {
    if fast <= Decimal::ZERO || slow <= Decimal::ZERO {
        return None;
    }
    if fast > slow && last > fast && direction != SkillDirection::Short {
        Some(Side::Buy)
    } else if fast < slow && last < fast && direction != SkillDirection::Long {
        Some(Side::Sell)
    } else {
        None
    }
}

/// 海龟突破：突破上轨做多、跌破下轨做空；反向突破或 ATR 止损离场。
async fn evaluate_breakout(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    last: Decimal,
    indicators: &arb_core::SandboxIndicators,
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let (Some(high), Some(low)) = (indicators.channel_high, indicators.channel_low) else {
        return Ok(());
    };
    if open.is_empty() {
        match breakout_signal(last, high, low, rule.steps.direction) {
            Some(Side::Buy) => {
                try_open(state, strategy, rule, venue, symbol, Side::Buy, last).await
            }
            Some(Side::Sell) => {
                try_open(state, strategy, rule, venue, symbol, Side::Sell, last).await
            }
            None => {}
        }
        return Ok(());
    }
    for trade in open {
        let exit = if trade.side == Side::Buy {
            last < low || atr_stopped(trade, last, indicators.atr, rule.trigger.atr_multiple)
        } else {
            last > high || atr_stopped(trade, last, indicators.atr, rule.trigger.atr_multiple)
        };
        if exit {
            close_signal(state, trade.id).await;
        }
    }
    Ok(())
}

/// 均线趋势：快线在慢线上方且价格站上快线做多，反之做空；均线反转离场。
async fn evaluate_ma_trend(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    last: Decimal,
    indicators: &arb_core::SandboxIndicators,
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let (Some(fast), Some(slow)) = (indicators.sma_fast, indicators.sma_slow) else {
        return Ok(());
    };
    if fast <= Decimal::ZERO || slow <= Decimal::ZERO {
        return Ok(());
    }
    if open.is_empty() {
        match ma_trend_signal(last, fast, slow, rule.steps.direction) {
            Some(Side::Buy) => {
                try_open(state, strategy, rule, venue, symbol, Side::Buy, last).await
            }
            Some(Side::Sell) => {
                try_open(state, strategy, rule, venue, symbol, Side::Sell, last).await
            }
            None => {}
        }
        return Ok(());
    }
    for trade in open {
        let exit = if trade.side == Side::Buy {
            fast < slow
        } else {
            fast > slow
        };
        if exit {
            close_signal(state, trade.id).await;
        }
    }
    Ok(())
}

/// 网格：价格触及档位时单腿开仓，止盈设在相邻档位。
async fn evaluate_grid(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    last: Decimal,
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let levels = rule.business.grid_levels.clamp(1, MAX_GRID_LEVELS) as usize;
    // 网格同时持仓上限：单边档位全部成交后不再加仓，等待止盈释放。
    if open.len() >= levels * 2 {
        return Ok(());
    }
    let key = format!("{}:{}:{symbol}", strategy.id, rule.id);
    let hits = state.skill.grid_triggers(&key, last, rule);
    for (side, _level) in hits {
        try_open(state, strategy, rule, venue, symbol, side, last).await;
    }
    Ok(())
}

/// ATR 止损：浮亏超过「ATR × 倍数」时离场。
///
/// 抽成纯函数便于校验器用固定输入核对止损计算，不必构造完整交易记录。
pub fn atr_stop_hit(
    side: Side,
    entry: Decimal,
    last: Decimal,
    atr: Option<Decimal>,
    multiple: Decimal,
) -> bool {
    let Some(atr) = atr else {
        return false;
    };
    if atr <= Decimal::ZERO || multiple <= Decimal::ZERO {
        return false;
    }
    let distance = atr * multiple;
    if side == Side::Buy {
        last <= entry - distance
    } else {
        last >= entry + distance
    }
}

fn atr_stopped(
    trade: &listing::ListingTrade,
    last: Decimal,
    atr: Option<Decimal>,
    multiple: Decimal,
) -> bool {
    atr_stop_hit(trade.side, trade.entry_price, last, atr, multiple)
}

/// 指标类技能开仓：遵守冷却、每日上限与单笔金额上限。
async fn try_open(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    side: Side,
    last: Decimal,
) {
    let key = format!("{}:{}:{symbol}", strategy.id, rule.id);
    if !state.skill.allow_daily(&key, rule.cautions.max_daily_opens) {
        tracing::info!(
            strategy_id = %strategy.id,
            %symbol,
            rule = %rule.name,
            "技能规则已达到当日开仓上限，忽略本次信号"
        );
        return;
    }
    let paper = rule.steps.execution_mode == StrategyExecutionMode::Paper;
    // 网格每档只使用下单金额的一部分，其余指标技能按整笔金额下单。
    let notional = if rule.kind == SkillKind::Grid {
        let share = rule.business.grid_share_pct / Decimal::from(100);
        capped_notional(rule) * share
    } else {
        capped_notional(rule)
    };
    if notional <= Decimal::ZERO {
        return;
    }
    let side_zh = if side == Side::Buy {
        "做多"
    } else {
        "做空"
    };
    let headline = if rule.purpose.trim().is_empty() {
        format!("{} · {side_zh}信号 {last}", rule.name)
    } else {
        format!("{} · {side_zh}信号 {last}", rule.purpose.trim())
    };
    let take_profit = if rule.kind == SkillKind::Grid {
        rule.business.grid_spacing_pct
    } else {
        rule.business.take_profit_pct
    };
    let request = OpenListingRequest {
        token: symbol.trim_end_matches("USDT").to_string(),
        headline,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        notional_usd: notional,
        leverage: rule.business.leverage,
        take_profit_pct: take_profit,
        stop_loss_pct: rule.business.stop_loss_pct,
        strategy_id: Some(strategy.id),
        rule_id: Some(rule.id),
        rule_name: Some(rule.name.clone()),
        paper,
        skill: true,
        max_hold_secs: 0,
        notify: rule.output.notify,
    };
    if let Err(error) = listing::open_position(state, request).await {
        tracing::warn!(
            %symbol,
            %venue,
            %error,
            rule = %rule.name,
            "技能指标信号开仓失败"
        );
        crate::strategy_runner::note_reject(
            state,
            strategy.id,
            format!("技能开仓失败 {symbol}（{venue}）：{error}"),
        )
        .await;
    }
}

/// 信号反转离场。
async fn close_signal(state: &AppState, trade_id: Uuid) {
    if let Err(error) = listing::close_position(state, trade_id, CloseReason::Signal).await {
        tracing::warn!(
            trade = %trade_id,
            %error,
            "技能信号平仓失败，下一轮继续尝试"
        );
    }
}

/// 单条规则试跑公告的结果：命中情况、开仓交易所与交易记录。
#[derive(Debug, Serialize)]
pub struct EventTestResult {
    pub rule_id: Uuid,
    pub rule_name: String,
    /// 来源、关键词与代币解析是否命中。
    pub matched: bool,
    pub token: Option<String>,
    pub venue: Option<String>,
    pub trade: Option<listing::ListingTrade>,
    /// 未命中的原因，便于直接定位来源或关键词配置。
    pub reason: Option<String>,
    /// 已命中但开仓失败的原因。
    pub error: Option<String>,
}

/// 试跑一条公告：绕过启动基线与事件窗口，复用运行期的来源、关键词与代币解析逻辑。
///
/// 开仓固定为模拟模式，避免测试时误下真实订单；交易记录会在 5 分钟后按超时自动离场。
pub(crate) async fn test_event(
    state: &AppState,
    record: &StrategyRecord,
    source: &str,
    title: &str,
    only_rule: Option<Uuid>,
    symbol_override: Option<&str>,
    dry_run: bool,
    live: bool,
    venue_override: Option<&str>,
) -> Result<Vec<EventTestResult>, String> {
    if !manages(record) {
        return Err("该策略不是技能策略，无法试跑公告".to_string());
    }
    let settings = settings_for(state, record);
    let mut results = Vec::new();
    for rule in settings.enabled_rules() {
        if rule.kind != SkillKind::Event || only_rule.is_some_and(|id| id != rule.id) {
            continue;
        }
        if !rule.trigger.event_sources.is_empty()
            && !rule
                .trigger
                .event_sources
                .iter()
                .any(|item| item.eq_ignore_ascii_case(source))
        {
            results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: false,
                token: None,
                venue: None,
                trade: None,
                reason: Some(format!("公告来源 {source} 不在规则的监听列表")),
                error: None,
            });
            continue;
        }
        let Some(token) = listing::detect_skill_token(title, &rule.trigger.event_keywords) else {
            results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: false,
                token: None,
                venue: None,
                trade: None,
                reason: Some("公告未命中触发关键词，或解析不出代币代码".to_string()),
                error: None,
            });
            continue;
        };
        // 批量校验只关心匹配结果，不开仓。
        if dry_run {
            results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: true,
                token: Some(token),
                venue: None,
                trade: None,
                reason: None,
                error: None,
            });
            continue;
        }
        let notional = capped_notional(rule);
        if notional <= Decimal::ZERO {
            results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: true,
                token: Some(token),
                venue: None,
                trade: None,
                reason: None,
                error: Some("下单金额为 0，无法开仓".to_string()),
            });
            continue;
        }

        // 默认按模拟模式试跑；只有显式 live 才使用规则里的真实下单配置。
        let mut test_rule = rule.clone();
        test_rule.steps.execution_mode = if live {
            StrategyExecutionMode::Real
        } else {
            StrategyExecutionMode::Paper
        };
        let mut venues = candidate_venues(state, &test_rule, &record.definition.universe.venues);
        if let Some(venue) = venue_override {
            let venue = venue.trim().to_ascii_lowercase();
            if !venues.contains(&venue) {
                results.push(EventTestResult {
                    rule_id: rule.id,
                    rule_name: rule.name.clone(),
                    matched: true,
                    token: Some(token),
                    venue: None,
                    trade: None,
                    reason: None,
                    error: Some(format!("{venue} 不在该规则的可执行交易所列表")),
                });
                continue;
            }
            venues = vec![venue];
        }
        if venues.is_empty() {
            results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: true,
                token: Some(token),
                venue: None,
                trade: None,
                reason: None,
                error: Some("没有可执行的交易所".to_string()),
            });
            continue;
        }

        // 交易对可以覆盖，用于只验证下单链路（例如 BTCUSDT）。
        let (trade_token, symbol) = match symbol_override {
            Some(symbol) => {
                let symbol = symbol.trim().to_ascii_uppercase();
                let token = symbol
                    .trim_end_matches("USDT")
                    .trim_end_matches("USDC")
                    .to_string();
                (token, symbol)
            }
            None => (token.clone(), format!("{token}USDT")),
        };

        let mut failures = Vec::new();
        let mut opened = None;
        for venue in venues {
            let request = OpenListingRequest {
                token: trade_token.clone(),
                headline: if live {
                    title.to_string()
                } else {
                    format!("[测试] {title}")
                },
                venue: venue.clone(),
                symbol: symbol.clone(),
                side: Side::Buy,
                notional_usd: notional,
                leverage: rule.business.leverage,
                take_profit_pct: rule.business.take_profit_pct,
                stop_loss_pct: rule.business.stop_loss_pct,
                strategy_id: Some(record.id),
                rule_id: Some(rule.id),
                rule_name: Some(rule.name.clone()),
                paper: !live,
                skill: true,
                // 模拟试跑 5 分钟后自动离场；真实下单按规则止盈止损持有。
                max_hold_secs: if live { 0 } else { 300 },
                notify: live && rule.output.notify,
            };
            match listing::open_position(state, request).await {
                Ok(trade) => {
                    opened = Some((venue, trade));
                    break;
                }
                Err(error) => failures.push(format!("{venue}: {error}")),
            }
        }
        match opened {
            Some((venue, trade)) => results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: true,
                token: Some(token),
                venue: Some(venue),
                trade: Some(trade),
                reason: None,
                error: None,
            }),
            None => results.push(EventTestResult {
                rule_id: rule.id,
                rule_name: rule.name.clone(),
                matched: true,
                token: Some(token),
                venue: None,
                trade: None,
                reason: None,
                error: Some(format!("开仓失败：{}", failures.join("；"))),
            }),
        }
    }

    if only_rule.is_some() && results.is_empty() {
        return Err("指定的规则不存在、未启用，或不是公告类技能".to_string());
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_runtime_only_manages_known_templates() {
        let mut definition = crate::templates::builtin().remove(0).definition;
        definition.name = "自定义策略".to_string();

        // 没有技能绑定的自定义策略按沙箱执行。
        let custom = StrategyRecord::new(
            definition.clone(),
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        assert!(!manages(&custom));

        // 绑定技能模板的策略由技能运行时接管。
        let mut bound = StrategyRecord::new(
            definition,
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        bound.skill = Some(arb_core::SkillBinding {
            template_key: "grid".to_string(),
            settings: None,
        });
        assert!(manages(&bound));
    }

    #[test]
    fn empty_settings_fall_back_to_one_rule_with_default_leverage() {
        let settings = SkillSettings::default().normalized(Decimal::from(3));
        assert_eq!(settings.rules.len(), 1);
        assert_eq!(settings.rules[0].business.leverage, Decimal::from(3));
        assert!(!settings.rules[0].id.is_nil());
        assert!(settings.rules[0].enabled);
        assert_eq!(settings.rules[0].kind, SkillKind::Event);
        assert_eq!(settings.rules[0].output.action, SkillAction::OpenLong);
    }

    #[test]
    fn keeps_each_skill_section_from_json() {
        let parsed: SkillSettings = serde_json::from_str(
            r#"{"rules":[{
                "name":"公告套利",
                "purpose":"上币公告后短线做多",
                "enabled":false,
                "trigger":{"event_sources":["binance"],"event_keywords":["will list"],"event_window_secs":900},
                "steps":{"execution_mode":"paper","execution_venues":["okx"]},
                "business":{"notional_usd":"50","leverage":"2","take_profit_pct":"15","stop_loss_pct":"8"},
                "output":{"action":"notify_only","notify":false},
                "cautions":{"max_notional_usd":"40","max_daily_opens":3}
            }]}"#,
        )
        .unwrap();
        let settings = parsed.normalized(Decimal::ONE);
        let rule = &settings.rules[0];
        assert_eq!(rule.purpose, "上币公告后短线做多");
        assert_eq!(rule.trigger.event_sources, vec!["binance".to_string()]);
        assert_eq!(rule.trigger.event_keywords, vec!["will list".to_string()]);
        assert_eq!(rule.trigger.event_window_secs, 900);
        assert_eq!(rule.steps.execution_mode, StrategyExecutionMode::Paper);
        assert_eq!(rule.steps.execution_venues, vec!["okx".to_string()]);
        assert_eq!(rule.business.notional_usd, Decimal::from(50));
        // 原文档是 notify_only，归一化时迁移成模拟开仓。
        assert_eq!(rule.output.action, SkillAction::OpenLong);
        assert!(!rule.output.notify);
        assert_eq!(rule.cautions.max_notional_usd, Decimal::from(40));
        assert_eq!(rule.cautions.max_daily_opens, 3);
        assert!(!rule.enabled);
    }

    #[test]
    fn empty_event_keywords_are_backfilled_for_editing() {
        let mut event = SkillRule::default();
        event.trigger.event_keywords = vec![String::new()];
        let mut grid = SkillRule::default();
        grid.kind = SkillKind::Grid;
        grid.trigger.event_keywords = Vec::new();
        let settings = SkillSettings {
            rules: vec![event, grid],
        }
        .normalized(Decimal::ONE);

        assert_eq!(
            settings.rules[0].trigger.event_keywords,
            default_event_keywords()
        );
        // 非公告类技能不读关键词，不要凭空写入。
        assert!(settings.rules[1].trigger.event_keywords.is_empty());
    }

    #[test]
    fn notify_only_is_migrated_to_paper_open() {
        let mut rule = SkillRule::default();
        rule.output.action = SkillAction::NotifyOnly;
        let settings = SkillSettings { rules: vec![rule] }.normalized(Decimal::ONE);

        assert_eq!(settings.rules[0].output.action, SkillAction::OpenLong);
        assert_eq!(
            settings.rules[0].steps.execution_mode,
            StrategyExecutionMode::Paper
        );
    }

    #[test]
    fn indicator_presets_match_legacy_templates() {
        let breakout = default_settings(SkillKind::Breakout, Decimal::ONE);
        let rule = &breakout.rules[0];
        assert_eq!(rule.trigger.channel, 20);
        assert_eq!(rule.trigger.atr_multiple, Decimal::from(2));
        assert_eq!(rule.business.take_profit_pct, Decimal::ZERO);

        let grid = default_settings(SkillKind::Grid, Decimal::ONE);
        let rule = &grid.rules[0];
        assert_eq!(rule.business.grid_levels, 3);
        assert_eq!(rule.business.grid_spacing_pct, Decimal::new(5, 1));
        assert_eq!(rule.business.grid_share_pct, Decimal::from(25));
    }

    #[test]
    fn detects_managed_strategy_names() {
        assert_eq!(kind_from_name("海龟突破"), Some(SkillKind::Breakout));
        assert_eq!(kind_from_name("均线趋势"), Some(SkillKind::MaTrend));
        assert_eq!(kind_from_name("网格"), Some(SkillKind::Grid));
        assert_eq!(kind_from_name("上币公告跟随"), Some(SkillKind::Event));
        assert_eq!(
            kind_from_name("技能策略（SKILL.md）"),
            Some(SkillKind::Event)
        );
        // 同一模板创建多条实例时重名会自动编号，后缀不影响识别。
        assert_eq!(kind_from_name("海龟突破 2"), Some(SkillKind::Breakout));
        assert_eq!(kind_from_name("网格 · BTCUSDT"), Some(SkillKind::Grid));
        assert_eq!(kind_from_name("我的网格"), None);
    }
}
