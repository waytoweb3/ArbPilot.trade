//! 技能策略（SKILL.md 参数化）。
//!
//! 一份 `SKILL.md` 由六个部分组成：技能用途、什么时候调用、执行步骤、业务规则、
//! 输出格式与注意事项。这里把这些章节映射成结构化参数，由执行器按参数判断触发条件、
//! 通过统一风控网关下单；止盈止损沿用单腿交易监控。
//!
//! 「什么时候调用」支持五种触发方式：
//! - `event`：交易所公告（上币公告跟随、技能策略）；
//! - `breakout`：唐奇安通道突破（海龟突破）；
//! - `ma_trend`：快慢均线趋势（均线趋势）；
//! - `grid`：围绕锚定价的本地网格档位（网格）；
//! - `program`：作者组合指标与逻辑表达式的通用程序。
//!
//! 五种方式共用同一套六章参数与单腿交易记录，因此同一个执行器可以同时运行多条
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
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    events::{AlertLevel, ExecutorEvent},
    listing::{self, CloseReason, OpenListingRequest},
    skill_turtle,
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
/// 网格档位数量上限，与交易所网格的数量范围保持一致。
pub const MAX_GRID_LEVELS: u32 = 1_000;

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
    /// 通用表达式程序：入场、离场、过滤与风控由作者组合，不再受内置引擎限制。
    Program,
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

/// 海龟双系统参数：系统一 20/10、系统二 55/20，可分别启停。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TurtleSystems {
    pub system1_enabled: bool,
    pub system2_enabled: bool,
    /// 系统一入场突破周期。
    pub entry1: u32,
    /// 系统一退出突破周期。
    pub exit1: u32,
    /// 系统二入场突破周期。
    pub entry2: u32,
    /// 系统二退出突破周期。
    pub exit2: u32,
    /// 系统一上一笔盈利后跳过下一次入场信号。
    pub profit_filter: bool,
}

impl Default for TurtleSystems {
    fn default() -> Self {
        Self {
            system1_enabled: true,
            system2_enabled: true,
            entry1: 20,
            exit1: 10,
            entry2: 55,
            exit2: 20,
            profit_filter: true,
        }
    }
}

/// 订单类型；限价单当前只持久化，执行链路仍按市价成交。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrderType {
    #[default]
    Market,
    Limit,
}

/// 网格档位的间距方式，对应交易所的「等差 / 等比」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridSpacingMode {
    /// 等差：相邻档位的价差相同。
    #[default]
    Arithmetic,
    /// 等比：相邻档位的涨跌幅相同。
    Geometric,
}

/// 价格突破网格区间后的跟随方式，对应交易所的「上移 / 下移」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridShift {
    /// 区间固定，突破后不再跟随。
    #[default]
    None,
    /// 价格向上突破时整体上移区间。
    Up,
    /// 价格向下突破时整体下移区间。
    Down,
    /// 向上、向下都跟随。
    Both,
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
    /// 参与计算的均线周期集合（去重升序）；为空时沿用 `fast` 与 `slow`。
    ///
    /// 这里只声明「要算哪几条均线」，哪条均线用于入场/出场由 `entries` / `exits` 决定。
    pub ma_periods: Vec<u32>,
    /// 均线入场条件；全部满足才开仓，为空时沿用旧的快慢均线金叉逻辑。
    pub entries: Vec<MaEntry>,
    /// 均线出场条件；任一满足即离场，为空时沿用旧的均线反转逻辑。
    pub exits: Vec<MaExit>,
    /// ATR 周期。
    pub atr: u32,
    /// 唐奇安通道周期。
    pub channel: u32,
    /// 止损距离 = ATR × 该倍数；0 表示不启用 ATR 止损。
    pub atr_multiple: Decimal,
    /// 海龟双系统参数；仅 `breakout` 使用。
    pub turtle: TurtleSystems,
}

/// 均线入场条件：只引用指标周期，不重复实现指标算法。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MaEntry {
    /// 多头/空头排列：按周期从短到长，短均线依次高于长均线做多，反之做空。
    Alignment { periods: Vec<u32> },
    /// 价格站上/跌破指定均线。
    PriceAbove { ma: u32 },
    /// 回踩指定均线：价格距均线不超过 1 倍 ATR 时顺势入场。
    Pullback { ma: u32 },
}

/// 均线出场条件：任一满足即离场。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MaExit {
    /// 快慢均线反向（死叉/金叉）。
    Cross { fast: u32, slow: u32 },
    /// 收盘价跌破/升破指定均线。
    BreakMa { ma: u32 },
    /// ATR 止损，倍数取 `trigger.atr_multiple`。
    AtrStop,
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
            ma_periods: Vec::new(),
            entries: Vec::new(),
            exits: Vec::new(),
            atr: 20,
            channel: 20,
            atr_multiple: Decimal::from(2),
            turtle: TurtleSystems::default(),
        }
    }
}

/// 实际参与计算的均线周期：去重升序；旧配置没有新字段时回落到 `fast` / `slow`。
pub fn resolved_ma_periods(trigger: &SkillTrigger) -> Vec<u32> {
    let mut periods: Vec<u32> = trigger
        .ma_periods
        .iter()
        .copied()
        .filter(|period| *period >= 2)
        .collect();
    if periods.is_empty() {
        periods.push(trigger.fast.max(2));
        periods.push(trigger.slow.max(2));
    }
    periods.sort_unstable();
    periods.dedup();
    periods
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
    /// 订单类型；限价单当前只保存配置，实际仍按市价执行。
    pub order_type: SkillOrderType,
}

impl Default for SkillSteps {
    fn default() -> Self {
        Self {
            execution_mode: StrategyExecutionMode::Real,
            execution_venues: Vec::new(),
            direction: SkillDirection::Long,
            order_type: SkillOrderType::Market,
        }
    }
}

/// 「业务规则」：金额、杠杆、止盈止损、风险回报与网格档位。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillBusinessRules {
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    /// 风险回报比（1:2 时填 2）；0 表示不按 ATR 止损距离推导止盈。
    pub risk_reward: Decimal,
    /// 止盈百分比（20 表示 +20%）；0 表示不启用。
    pub take_profit_pct: Decimal,
    /// 止损百分比（10 表示 -10%）；0 表示不启用。
    pub stop_loss_pct: Decimal,
    /// 旧版网格单边档位数；`grid_count` 为空时按 `grid_levels × 2` 迁移。
    pub grid_levels: u32,
    /// 网格相邻档位间距（百分比，0.5 表示 0.5%）；未配置价格区间时用它推导区间。
    pub grid_spacing_pct: Decimal,
    /// 旧版每档金额占比；现在按网格数量自动均分，仅保留字段兼容历史配置。
    pub grid_share_pct: Decimal,
    /// 网格数量（区间内的档位数，2..=MAX_GRID_LEVELS）；0 表示沿用旧配置。
    pub grid_count: u32,
    /// 网格区间下限；为 0 时按现价与档位间距自动推导。
    pub grid_lower_price: Decimal,
    /// 网格区间上限；为 0 时按现价与档位间距自动推导。
    pub grid_upper_price: Decimal,
    /// 档位间距方式：等差或等比。
    pub grid_spacing_mode: GridSpacingMode,
    /// 网格触发价；为 0 表示立即启动。
    pub grid_trigger_price: Decimal,
    /// 价格突破区间后的跟随方式。
    pub grid_shift: GridShift,
    /// 触发风控或下单失败时，平掉该网格已有的仓位。
    pub grid_close_on_fail: bool,
    /// 同一币种双系统共享的最大 Unit 数。
    pub max_units: u32,
    /// 金字塔加仓间距（N 的倍数）。
    pub pyramid_step_n: Decimal,
    /// 最大加仓次数。
    pub max_pyramid_adds: u32,
    /// 同一规则最大持仓币种数；0 表示不限。
    pub max_symbols: u32,
    /// 海龟有效杠杆上限。
    pub max_leverage: Decimal,
    /// 最长持有小时；0 表示不启用。
    pub max_hold_hours: Decimal,
}

impl Default for SkillBusinessRules {
    fn default() -> Self {
        Self {
            notional_usd: Decimal::from(100),
            leverage: Decimal::ONE,
            risk_reward: Decimal::ZERO,
            take_profit_pct: Decimal::from(20),
            stop_loss_pct: Decimal::from(10),
            grid_levels: 3,
            grid_spacing_pct: Decimal::new(5, 1),
            grid_share_pct: Decimal::from(25),
            grid_count: 0,
            grid_lower_price: Decimal::ZERO,
            grid_upper_price: Decimal::ZERO,
            grid_spacing_mode: GridSpacingMode::Arithmetic,
            grid_trigger_price: Decimal::ZERO,
            grid_shift: GridShift::None,
            grid_close_on_fail: false,
            max_units: 4,
            pyramid_step_n: Decimal::new(5, 1),
            max_pyramid_adds: 3,
            max_symbols: 3,
            max_leverage: Decimal::from(3),
            max_hold_hours: Decimal::ZERO,
        }
    }
}

impl SkillBusinessRules {
    /// 网格实际档位数：新配置用 `grid_count`，旧配置回落到 `grid_levels × 2`。
    pub fn grid_count(&self) -> u32 {
        let count = if self.grid_count >= 2 {
            self.grid_count
        } else {
            self.grid_levels.saturating_mul(2).max(2)
        };
        count.clamp(2, MAX_GRID_LEVELS)
    }

    /// 每档名义金额占比：总投资额按档位数均分。
    pub fn grid_share(&self) -> Decimal {
        Decimal::ONE / Decimal::from(self.grid_count())
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
    /// 单笔风险占账户权益的百分比（1 表示 1%）；0 表示按 `notional_usd` 下单。
    pub risk_pct: Decimal,
    /// 同一规则最多同时持有的仓位数；0 表示不限。
    pub max_positions: u32,
    /// 同一规则同一交易对的冷却小时数；0 表示不冷却。
    pub cooldown_hours: Decimal,
}

impl Default for SkillCautions {
    fn default() -> Self {
        Self {
            max_notional_usd: Decimal::ZERO,
            max_daily_opens: 0,
            risk_pct: Decimal::ZERO,
            max_positions: 0,
            cooldown_hours: Decimal::ZERO,
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
    /// 触发方式：公告、通道突破、均线趋势、网格或通用表达式。
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
    /// 来源 Skill；从技能库发布或导入时写入，本地新建规则为空。
    #[serde(default)]
    pub source_skill_id: Option<Uuid>,
    /// 来源 Skill 版本号。
    #[serde(default)]
    pub source_skill_version: Option<u32>,
    /// 来源版本里的原始规则 ID，用于升级时对齐规则并保留本地启用状态。
    #[serde(default)]
    pub source_rule_id: Option<Uuid>,
    /// 通用表达式程序；`kind == Program` 时必填。
    #[serde(default)]
    pub program: Option<arb_core::program::StrategyProgram>,
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
            source_skill_id: None,
            source_skill_version: None,
            source_rule_id: None,
            program: None,
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
            // 均线周期以指标集合为准：去重升序，旧配置回落到 fast/slow。
            if rule.kind == SkillKind::MaTrend {
                rule.trigger.ma_periods = resolved_ma_periods(&rule.trigger);
            }
            // 海龟参数兜底：周期、Unit 与加仓次数保持可用。
            if rule.kind == SkillKind::Breakout {
                let turtle = &mut rule.trigger.turtle;
                if turtle.entry1 < 2 {
                    turtle.entry1 = 20;
                }
                if turtle.exit1 < 2 {
                    turtle.exit1 = 10;
                }
                if turtle.entry2 < 2 {
                    turtle.entry2 = 55;
                }
                if turtle.exit2 < 2 {
                    turtle.exit2 = 20;
                }
                if rule.business.max_units < 1 {
                    rule.business.max_units = 4;
                }
                let max_adds = rule.business.max_units.saturating_sub(1);
                if rule.business.max_pyramid_adds > max_adds {
                    rule.business.max_pyramid_adds = max_adds;
                }
                if rule.business.pyramid_step_n <= Decimal::ZERO {
                    rule.business.pyramid_step_n = Decimal::new(5, 1);
                }
                if rule.business.max_leverage <= Decimal::ZERO {
                    rule.business.max_leverage = rule.business.leverage.max(Decimal::ONE);
                }
            }
            // 通用表达式程序：参数夹紧后把风控同步到技能业务规则，
            // 这样校验、展示与统一风控网关看到的是同一套数值。
            if rule.kind == SkillKind::Program {
                if rule.program.is_none() {
                    rule.program = Some(arb_core::program::StrategyProgram::example_ma_cross());
                }
                if let Some(program) = rule.program.as_mut() {
                    program.normalize();
                    rule.trigger.timeframe = program.data.timeframe.clone();
                    rule.trigger.lookback = program.data.lookback;
                    rule.business.stop_loss_pct = program.risk.stop_loss_pct;
                    rule.business.take_profit_pct = program.risk.take_profit_pct;
                    rule.business.max_hold_hours = program.risk.max_hold_hours;
                    rule.cautions.cooldown_hours = program.risk.cooldown_hours;
                    if program.sizing.notional_usd > Decimal::ZERO {
                        rule.business.notional_usd = program.sizing.notional_usd;
                    }
                    if program.sizing.risk_pct > Decimal::ZERO {
                        rule.cautions.risk_pct = program.sizing.risk_pct;
                    }
                }
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
        "program" | "expression" => Some(SkillKind::Program),
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
        "通用表达式" | "策略程序" | "通用均线交叉" => Some(SkillKind::Program),
        _ => None,
    }
}

/// 记录的技能触发方式：优先模板标识，兼容未回填的历史记录。
fn kind_for(record: &StrategyRecord) -> Option<SkillKind> {
    // 技能库版本使用通用 `skill` 标识，真实触发方式写在冻结规则里。
    let from_library = record
        .skill
        .as_ref()
        .map(|skill| skill.skill_id.is_some() || skill.template_key == "skill")
        .unwrap_or(false);
    if from_library {
        if let Some(kind) = frozen_settings(record)
            .and_then(|settings| settings.rules.first().map(|rule| rule.kind))
        {
            return Some(kind);
        }
    }
    record
        .skill
        .as_ref()
        .and_then(|skill| kind_from_template(&skill.template_key))
        .or_else(|| kind_from_name(&record.definition.name))
}

/// 发布时冻结在绑定里的技能参数；没有参数或参数为空时返回 None。
///
/// 第三方技能没有内置模板标识，执行时完全以这份冻结参数为准。
fn frozen_settings(record: &StrategyRecord) -> Option<SkillSettings> {
    record
        .skill
        .as_ref()
        .and_then(|skill| skill.settings.clone())
        .and_then(|value| serde_json::from_value::<SkillSettings>(value).ok())
        .filter(|settings| !settings.rules.is_empty())
}

/// 是否是技能运行时接管的策略；沙箱模块对它们不参与运行。
///
/// 除内置模板外，任何携带冻结规则参数的技能绑定都由技能运行时接管，
/// 这样第三方技能无需新增模板标识即可执行。
pub fn manages(record: &StrategyRecord) -> bool {
    frozen_settings(record).is_some() || kind_for(record).is_some()
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
            rule.business.max_units = 4;
            rule.business.pyramid_step_n = Decimal::new(5, 1);
            rule.business.max_pyramid_adds = 3;
            rule.business.max_symbols = 3;
            rule.business.max_leverage = Decimal::from(3);
            rule.business.max_hold_hours = Decimal::ZERO;
            rule.cautions.risk_pct = Decimal::new(5, 1);
            rule.cautions.cooldown_hours = Decimal::from(24);
        }
        SkillKind::MaTrend => {
            rule.name = "均线趋势".to_string();
            rule.purpose =
                "均线多头排列且回踩 MA20 做多、空头排列同理做空，跌破 MA20 或 ATR 止损离场。"
                    .to_string();
            rule.trigger.timeframe = "1h".to_string();
            rule.trigger.lookback = 300;
            rule.trigger.fast = 5;
            rule.trigger.slow = 20;
            rule.trigger.atr = 14;
            rule.trigger.atr_multiple = Decimal::from(2);
            rule.trigger.ma_periods = vec![5, 10, 20, 50];
            rule.trigger.entries = vec![
                MaEntry::Alignment {
                    periods: vec![5, 10, 20, 50],
                },
                MaEntry::PriceAbove { ma: 20 },
                MaEntry::Pullback { ma: 20 },
            ];
            rule.trigger.exits = vec![MaExit::BreakMa { ma: 20 }, MaExit::AtrStop];
            rule.cautions.risk_pct = Decimal::ONE;
            rule.business.risk_reward = Decimal::from(2);
            rule.cautions.max_positions = 3;
            rule.cautions.cooldown_hours = Decimal::from(4);
            rule.business.take_profit_pct = Decimal::ZERO;
            rule.business.stop_loss_pct = Decimal::ZERO;
        }
        SkillKind::Grid => {
            rule.name = "网格".to_string();
            rule.purpose =
                "在价格区间内按等差或等比布置档位，价格触及后单腿开仓，到达相邻档位止盈。"
                    .to_string();
            rule.trigger.timeframe = "5m".to_string();
            rule.trigger.lookback = 200;
            // 网格默认中性：区间下方买入、上方卖出。
            rule.steps.direction = SkillDirection::Both;
            rule.business.take_profit_pct = Decimal::ZERO;
            rule.business.stop_loss_pct = Decimal::ZERO;
            rule.business.grid_count = 10;
            rule.business.grid_levels = 5;
            rule.business.grid_spacing_pct = Decimal::new(5, 1);
            rule.business.grid_share_pct = Decimal::from(10);
            rule.business.grid_spacing_mode = GridSpacingMode::Arithmetic;
        }
        SkillKind::Program => {
            rule.name = "通用表达式".to_string();
            rule.purpose =
                "用指标、比较与逻辑表达式组合入场/离场条件，参数空间由程序自动生成。".to_string();
            let program = arb_core::program::StrategyProgram::example_ma_cross();
            rule.trigger.timeframe = program.data.timeframe.clone();
            rule.trigger.lookback = program.data.lookback;
            rule.business.notional_usd = program.sizing.notional_usd;
            rule.business.stop_loss_pct = program.risk.stop_loss_pct;
            rule.business.take_profit_pct = program.risk.take_profit_pct;
            rule.business.max_hold_hours = program.risk.max_hold_hours;
            rule.cautions.cooldown_hours = program.risk.cooldown_hours;
            rule.program = Some(program);
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
                    ..SkillSteps::default()
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
                source_skill_id: None,
                source_skill_version: None,
                source_rule_id: None,
                program: None,
            })
            .collect(),
    }
}

/// 网格档位状态：区间、档位方向与成交标记。
struct GridState {
    /// 当前生效的档位数，配置变化时据此重建。
    count: u32,
    lower: Decimal,
    upper: Decimal,
    /// 跟随突破后锁定的区间；重启或档位数变化时清空。
    range_override: Option<(Decimal, Decimal)>,
    /// 升序档位价。
    levels: Vec<Decimal>,
    /// 每个档位的开仓方向；`None` 表示该档位在当前方向设置下不交易。
    sides: Vec<Option<Side>>,
    filled: Vec<bool>,
    /// 是否已到达触发价。
    armed: bool,
    /// 到达触发价前的等待方向：Buy 等价格跌到触发价，Sell 等涨到。
    arm_on: Option<Side>,
}

impl GridState {
    /// 首次评估：按配置决定是否等待触发价，未等待时立即布置档位。
    fn new(rule: &SkillRule, price: Decimal) -> Self {
        let trigger = rule.business.grid_trigger_price;
        let (armed, arm_on) = if trigger > Decimal::ZERO {
            if trigger < price {
                (false, Some(Side::Buy))
            } else if trigger > price {
                (false, Some(Side::Sell))
            } else {
                (true, None)
            }
        } else {
            (true, None)
        };
        let mut state = Self {
            count: rule.business.grid_count(),
            lower: Decimal::ZERO,
            upper: Decimal::ZERO,
            range_override: None,
            levels: Vec::new(),
            sides: Vec::new(),
            filled: Vec::new(),
            armed,
            arm_on,
        };
        if armed {
            state.fill(rule, price);
        }
        state
    }

    /// 按配置区间（缺失时按现价推导）重新布置档位，并清空成交标记。
    fn fill(&mut self, rule: &SkillRule, price: Decimal) {
        let range = self.range_override.or_else(|| grid_range(price, rule));
        let Some((lower, upper)) = range else {
            self.levels.clear();
            self.sides.clear();
            self.filled.clear();
            return;
        };
        self.apply(rule, lower, upper, price);
    }

    /// 用给定区间重建档位；价格上涨/下跌跟随也走这条路径。
    fn apply(&mut self, rule: &SkillRule, lower: Decimal, upper: Decimal, price: Decimal) {
        self.lower = lower;
        self.upper = upper;
        self.count = rule.business.grid_count();
        self.levels = grid_price_levels(lower, upper, self.count, rule.business.grid_spacing_mode);
        self.sides = self
            .levels
            .iter()
            .map(|level| {
                if *level < price && rule.steps.direction != SkillDirection::Short {
                    Some(Side::Buy)
                } else if *level > price && rule.steps.direction != SkillDirection::Long {
                    Some(Side::Sell)
                } else {
                    None
                }
            })
            .collect();
        // 不可交易的档位直接视为已处理，避免「全部成交」判断永远不成立。
        self.filled = self.sides.iter().map(|side| side.is_none()).collect();
    }

    /// 到达触发价：立即按当前价布置档位。
    fn arm(&mut self, rule: &SkillRule, price: Decimal) {
        self.armed = true;
        self.arm_on = None;
        self.fill(rule, price);
    }

    /// 价格突破区间时按方向整体平移，保留区间宽度与档位数。
    fn follow(&mut self, rule: &SkillRule, price: Decimal) {
        if self.levels.is_empty() {
            return;
        }
        let up = matches!(rule.business.grid_shift, GridShift::Up | GridShift::Both)
            && price > self.upper;
        let down = matches!(rule.business.grid_shift, GridShift::Down | GridShift::Both)
            && price < self.lower;
        if !up && !down {
            return;
        }
        let delta = if up {
            price - self.upper
        } else {
            price - self.lower
        };
        let lower = self.lower + delta;
        let upper = self.upper + delta;
        self.range_override = Some((lower, upper));
        self.apply(rule, lower, upper, price);
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
    /// 冷却时间：键为「策略 + 规则 + 交易对」，值为上次开仓时间。
    cooldown: Mutex<HashMap<String, chrono::DateTime<Utc>>>,
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
            cooldown: Mutex::new(HashMap::new()),
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

    /// 是否仍在冷却时间内：`hours` 为 0 表示不冷却。
    fn in_cooldown(&self, key: &str, hours: Decimal) -> bool {
        if hours <= Decimal::ZERO {
            return false;
        }
        let Some(minutes) = (hours * Decimal::from(60)).to_i64() else {
            return false;
        };
        if minutes <= 0 {
            return false;
        }
        let cooldown = self.cooldown.lock().expect("技能冷却锁不可用");
        cooldown
            .get(key)
            .is_some_and(|last| Utc::now().signed_duration_since(*last).num_minutes() < minutes)
    }

    /// 记录一次成功开仓，用于计算冷却时间。
    fn mark_open(&self, key: &str) {
        self.cooldown
            .lock()
            .expect("技能冷却锁不可用")
            .insert(key.to_string(), Utc::now());
    }

    /// 计算本轮触发的网格档位：返回（方向、档位价、到相邻档位的止盈百分比）。
    ///
    /// 一轮最多触发一个档位，避免单次价格跳变时同时打出大量市价单。
    fn grid_triggers(
        &self,
        key: &str,
        price: Decimal,
        rule: &SkillRule,
    ) -> Vec<(Side, Decimal, Decimal)> {
        if price <= Decimal::ZERO {
            return Vec::new();
        }
        let count = rule.business.grid_count();
        let mut grids = self.grids.lock().expect("技能网格状态锁不可用");
        let state = grids
            .entry(key.to_string())
            .or_insert_with(|| GridState::new(rule, price));
        if state.count != count {
            *state = GridState::new(rule, price);
        }
        // 等待触发价：未到达前不布置档位。
        if !state.armed {
            let reached = match state.arm_on {
                Some(Side::Buy) => price <= rule.business.grid_trigger_price,
                Some(Side::Sell) => price >= rule.business.grid_trigger_price,
                None => true,
            };
            if !reached {
                return Vec::new();
            }
            state.arm(rule, price);
        }
        // 价格突破区间时按配置跟随。
        state.follow(rule, price);
        // 全部档位成交后按现价重建，让网格继续滚动。
        if state.filled.iter().all(|filled| *filled) {
            state.arm(rule, price);
        }
        if state.levels.is_empty() {
            return Vec::new();
        }
        let mut best: Option<(usize, Side, Decimal, Decimal)> = None;
        for (index, level) in state.levels.iter().enumerate() {
            if state.filled[index] {
                continue;
            }
            let Some(side) = state.sides[index] else {
                continue;
            };
            let hit = match side {
                Side::Buy => price <= *level,
                Side::Sell => price >= *level,
            };
            if !hit {
                continue;
            }
            let take_profit = grid_take_profit(rule, &state.levels, index, side);
            let distance = (*level - price).abs();
            if best.is_none_or(|(_, _, _, best_distance)| distance < best_distance) {
                best = Some((index, side, *level, take_profit));
            }
        }
        let Some((index, side, level, take_profit)) = best else {
            return Vec::new();
        };
        state.filled[index] = true;
        vec![(side, level, take_profit)]
    }
}

/// 到相邻档位的止盈百分比：买档看上一档，卖档看下一档。
fn grid_take_profit(rule: &SkillRule, levels: &[Decimal], index: usize, side: Side) -> Decimal {
    let target = match side {
        Side::Buy => levels.get(index + 1),
        Side::Sell => index.checked_sub(1).and_then(|prev| levels.get(prev)),
    };
    match (target, levels.get(index)) {
        (Some(target), Some(level)) if *level > Decimal::ZERO => {
            let change = if side == Side::Buy {
                *target - *level
            } else {
                *level - *target
            };
            (change / *level * Decimal::from(100)).max(Decimal::ZERO)
        }
        _ => rule.business.grid_spacing_pct,
    }
}

/// 网格区间：配置了上下限就用配置值，否则按现价与档位间距推导。
pub fn grid_range(price: Decimal, rule: &SkillRule) -> Option<(Decimal, Decimal)> {
    let business = &rule.business;
    if business.grid_lower_price > Decimal::ZERO
        && business.grid_upper_price > business.grid_lower_price
    {
        return Some((business.grid_lower_price, business.grid_upper_price));
    }
    if price <= Decimal::ZERO {
        return None;
    }
    let step = business.grid_spacing_pct / Decimal::from(100);
    if step <= Decimal::ZERO {
        return None;
    }
    let half = step * Decimal::from(business.grid_count()) / Decimal::from(2);
    let lower = price * (Decimal::ONE - half);
    let upper = price * (Decimal::ONE + half);
    (lower > Decimal::ZERO && upper > lower).then_some((lower, upper))
}

/// 在区间内按等差或等比生成升序档位。
///
/// 抽成纯函数便于校验器用固定输入核对档位计算，不必启动行情与下单链路。
pub fn grid_price_levels(
    lower: Decimal,
    upper: Decimal,
    count: u32,
    mode: GridSpacingMode,
) -> Vec<Decimal> {
    if lower <= Decimal::ZERO || upper <= lower {
        return Vec::new();
    }
    let count = count.clamp(2, MAX_GRID_LEVELS) as usize;
    let span = upper - lower;
    let slots = Decimal::from(count - 1);
    match mode {
        GridSpacingMode::Arithmetic => (0..count)
            .map(|index| lower + span * Decimal::from(index) / slots)
            .collect(),
        GridSpacingMode::Geometric => {
            // 等比档位用浮点求公比，再逐档复利；首尾强制等于区间端点。
            let lower_f64 = lower.to_f64().unwrap_or(1.0);
            let upper_f64 = upper.to_f64().unwrap_or(1.0);
            let ratio = Decimal::try_from((upper_f64 / lower_f64).powf(1.0 / (count - 1) as f64))
                .unwrap_or(Decimal::ONE);
            let mut levels = Vec::with_capacity(count);
            let mut value = lower;
            for index in 0..count {
                if index == count - 1 {
                    levels.push(upper);
                } else {
                    levels.push(value);
                    value *= ratio;
                }
            }
            levels
        }
    }
}

/// 正在运行的技能策略快照。
struct SkillStrategy {
    id: Uuid,
    venues: Vec<String>,
    symbols: Vec<String>,
    settings: SkillSettings,
}

/// 策略没有保存过技能参数时，按触发方式给出一份默认设置。
///
/// 导入的策略没有本地保存过参数，此时必须使用发布时冻结的那份，
/// 否则订阅者会按默认参数而不是作者发布的规则运行。
fn fallback_settings(state: &AppState, record: &StrategyRecord) -> SkillSettings {
    if let Some(settings) = frozen_settings(record) {
        return settings;
    }
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
            system: None,
            group_id: None,
            unit_index: 0,
            n_at_entry: None,
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
        ma_periods: resolved_ma_periods(&rule.trigger),
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
            let channels = turtle_channels(&snapshots, rule);
            evaluate_breakout(
                state,
                strategy,
                rule,
                &venue,
                symbol,
                last,
                &indicators,
                &channels,
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
        SkillKind::Program => {
            evaluate_program(state, strategy, rule, &venue, symbol, &snapshots, &open).await
        }
        SkillKind::Event => Ok(()),
    }
}

/// 通用表达式程序：由作者组合入场/离场/过滤条件，执行侧只负责拉行情与下单。
async fn evaluate_program(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    snapshots: &[SandboxCandle],
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let Some(program) = rule.program.as_ref() else {
        return Err("通用表达式规则缺少程序定义".to_string());
    };
    let funding_rate = state
        .funding
        .status()
        .await
        .into_iter()
        .find(|status| status.venue == venue)
        .and_then(|status| {
            status
                .symbols
                .into_iter()
                .find(|item| item.symbol == symbol)
        })
        .and_then(|item| item.predicted_rate.or(item.last_funding_rate));
    let position = open.first().map(|trade| {
        let since: Vec<&SandboxCandle> = snapshots
            .iter()
            .filter(|candle| candle.open_time >= trade.opened_at)
            .collect();
        arb_core::program::ProgramPosition {
            side: trade.side,
            entry_price: trade.entry_price,
            opened_at: trade.opened_at,
            highest_since_entry: since
                .iter()
                .map(|candle| candle.high)
                .max()
                .unwrap_or(trade.entry_price),
            lowest_since_entry: since
                .iter()
                .map(|candle| candle.low)
                .min()
                .unwrap_or(trade.entry_price),
        }
    });
    let decision = arb_core::program::evaluate(
        program,
        &arb_core::program::ProgramContext {
            candles: snapshots,
            funding_rate,
            position,
            now: Utc::now(),
        },
    );
    match decision.action {
        arb_core::program::ProgramAction::Hold => Ok(()),
        arb_core::program::ProgramAction::Close => {
            for trade in open {
                close_signal(state, trade.id).await;
            }
            Ok(())
        }
        arb_core::program::ProgramAction::Open(side) => {
            if !open.is_empty() {
                return Ok(());
            }
            if !direction_allows(side, rule.steps.direction)
                || max_symbols_reached(state, strategy, rule)
            {
                return Ok(());
            }
            try_open(
                state,
                strategy,
                rule,
                venue,
                symbol,
                side,
                snapshots
                    .last()
                    .map(|candle| candle.close)
                    .unwrap_or_default(),
                None,
                None,
                None,
            )
            .await;
            Ok(())
        }
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

/// 方向设置是否允许该信号。
pub(crate) fn direction_allows(side: Side, direction: SkillDirection) -> bool {
    match side {
        Side::Buy => direction != SkillDirection::Short,
        Side::Sell => direction != SkillDirection::Long,
    }
}

/// 取已计算的均线；缺失或非正数视作不可用。
fn ma_value(indicators: &arb_core::SandboxIndicators, period: u32) -> Option<Decimal> {
    indicators
        .sma(period)
        .filter(|value| *value > Decimal::ZERO)
}

/// 均线排列方向：周期从短到长，短均线依次更高且价格站上最短均线做多，反之做空。
fn alignment_side(
    indicators: &arb_core::SandboxIndicators,
    periods: &[u32],
    last: Decimal,
) -> Option<Side> {
    let mut ordered: Vec<u32> = periods
        .iter()
        .copied()
        .filter(|period| *period >= 2)
        .collect();
    ordered.sort_unstable();
    ordered.dedup();
    if ordered.len() < 2 {
        return None;
    }
    let values: Vec<Decimal> = ordered
        .iter()
        .map(|period| ma_value(indicators, *period))
        .collect::<Option<_>>()?;
    let descending = values.windows(2).all(|pair| pair[0] > pair[1]);
    let ascending = values.windows(2).all(|pair| pair[0] < pair[1]);
    if descending && last > values[0] {
        Some(Side::Buy)
    } else if ascending && last < values[0] {
        Some(Side::Sell)
    } else {
        None
    }
}

/// 均线入场信号：所有入场条件必须指向同一方向。
///
/// 条件为空时回落到旧的「快线在慢线上方 + 价格站上快线」逻辑，保证历史配置不变。
pub fn ma_entry_signal(
    last: Decimal,
    indicators: &arb_core::SandboxIndicators,
    trigger: &SkillTrigger,
) -> Option<Side> {
    if trigger.entries.is_empty() {
        let (Some(fast), Some(slow)) = (indicators.sma_fast, indicators.sma_slow) else {
            return None;
        };
        return ma_trend_signal(last, fast, slow, SkillDirection::Both);
    }
    let mut side: Option<Side> = None;
    for entry in &trigger.entries {
        let current = match entry {
            MaEntry::Alignment { periods } => alignment_side(indicators, periods, last),
            MaEntry::PriceAbove { ma } => {
                let value = ma_value(indicators, *ma)?;
                if last > value {
                    Some(Side::Buy)
                } else if last < value {
                    Some(Side::Sell)
                } else {
                    None
                }
            }
            MaEntry::Pullback { ma } => {
                let value = ma_value(indicators, *ma)?;
                let atr = indicators.atr.filter(|atr| *atr > Decimal::ZERO)?;
                if (last - value).abs() > atr {
                    return None;
                }
                if last >= value {
                    Some(Side::Buy)
                } else {
                    Some(Side::Sell)
                }
            }
        };
        let Some(current) = current else {
            return None;
        };
        match side {
            None => side = Some(current),
            Some(existing) if existing == current => {}
            Some(_) => return None,
        }
    }
    side
}

/// 均线出场条件：任一条件满足即离场。
///
/// 条件为空时回落到旧的「均线反转」判断。
pub fn ma_exit_hit(
    side: Side,
    last: Decimal,
    entry: Decimal,
    indicators: &arb_core::SandboxIndicators,
    trigger: &SkillTrigger,
) -> bool {
    if trigger.exits.is_empty() {
        return match (indicators.sma_fast, indicators.sma_slow) {
            (Some(fast), Some(slow)) => {
                if side == Side::Buy {
                    fast < slow
                } else {
                    fast > slow
                }
            }
            _ => false,
        };
    }
    trigger.exits.iter().any(|exit| match exit {
        MaExit::Cross { fast, slow } => {
            match (ma_value(indicators, *fast), ma_value(indicators, *slow)) {
                (Some(fast), Some(slow)) => {
                    if side == Side::Buy {
                        fast < slow
                    } else {
                        fast > slow
                    }
                }
                _ => false,
            }
        }
        MaExit::BreakMa { ma } => match ma_value(indicators, *ma) {
            Some(value) => {
                if side == Side::Buy {
                    last < value
                } else {
                    last > value
                }
            }
            None => false,
        },
        MaExit::AtrStop => atr_stop_hit(side, entry, last, indicators.atr, trigger.atr_multiple),
    })
}

/// 海龟开仓的附加信息：系统、组标识与 Unit 序号。
#[derive(Debug, Clone, Copy)]
struct TurtlePlacement {
    system: u8,
    group_id: Option<Uuid>,
    unit_index: u32,
    is_add: bool,
}

/// 海龟突破：双系统通道突破入场，金字塔加仓，反向退出通道或整组 2N 止损离场。
async fn evaluate_breakout(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    last: Decimal,
    indicators: &arb_core::SandboxIndicators,
    channels: &skill_turtle::TurtleChannels,
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let turtle = &rule.trigger.turtle;
    if open.is_empty() {
        let Some((side, system)) =
            skill_turtle::entry_signal(last, channels, rule.steps.direction, turtle)
        else {
            return Ok(());
        };
        if system == 1 && turtle.profit_filter {
            let trades = state.listing.all_trades();
            if skill_turtle::last_closed_system1_win(&trades, rule.id, symbol) == Some(true) {
                tracing::info!(
                    %symbol,
                    rule = %rule.name,
                    "系统一上一笔盈利，跳过本次入场信号"
                );
                return Ok(());
            }
        }
        if max_symbols_reached(state, strategy, rule) {
            return Ok(());
        }
        // Unit 仓位必须依赖 N（ATR），指标尚未预热时不入场。
        if indicators.atr.unwrap_or(Decimal::ZERO) <= Decimal::ZERO {
            return Ok(());
        }
        try_open(
            state,
            strategy,
            rule,
            venue,
            symbol,
            side,
            last,
            indicators.atr,
            None,
            Some(TurtlePlacement {
                system,
                group_id: None,
                unit_index: 0,
                is_add: false,
            }),
        )
        .await;
        return Ok(());
    }

    let side = open[0].side;
    let system = open[0].system.unwrap_or(1);
    let group_id = open[0].group_id;
    let latest = skill_turtle::latest_unit(open).unwrap_or(&open[0]);
    let n = latest
        .n_at_entry
        .or(indicators.atr)
        .unwrap_or(Decimal::ZERO);
    let stop = skill_turtle::group_stop(side, latest.entry_price, n, rule.trigger.atr_multiple);
    let stopped = stop
        .map(|stop| skill_turtle::stop_hit(side, last, stop))
        .unwrap_or(false);
    if stopped || skill_turtle::exit_signal(side, system, last, channels) {
        for trade in open {
            close_signal(state, trade.id).await;
        }
        return Ok(());
    }
    let units = open.len() as u32;
    let adds = units.saturating_sub(1);
    if skill_turtle::should_add(
        side,
        last,
        latest.entry_price,
        n,
        units,
        adds,
        rule.business.max_units,
        rule.business.max_pyramid_adds,
        rule.business.pyramid_step_n,
    ) {
        try_open(
            state,
            strategy,
            rule,
            venue,
            symbol,
            side,
            last,
            indicators.atr,
            None,
            Some(TurtlePlacement {
                system,
                group_id,
                unit_index: latest.unit_index.saturating_add(1),
                is_add: true,
            }),
        )
        .await;
    }
    Ok(())
}

/// 海龟规则的最大持仓币种数；0 表示不限制。
fn max_symbols_reached(state: &AppState, strategy: &SkillStrategy, rule: &SkillRule) -> bool {
    let limit = rule.business.max_symbols;
    if limit == 0 {
        return false;
    }
    let symbols: HashSet<String> = state
        .listing
        .open_trades()
        .into_iter()
        .filter(|trade| trade.strategy_id == Some(strategy.id) && trade.rule_id == Some(rule.id))
        .map(|trade| trade.symbol.to_ascii_uppercase())
        .collect();
    if symbols.len() < limit as usize {
        return false;
    }
    tracing::info!(
        strategy_id = %strategy.id,
        rule = %rule.name,
        "海龟规则已达到最大持仓币种数，忽略本次信号"
    );
    true
}

/// 按规则周期计算四个唐奇安通道。
fn turtle_channels(snapshots: &[SandboxCandle], rule: &SkillRule) -> skill_turtle::TurtleChannels {
    let period = |value: u32| {
        if value >= 2 {
            indicators::donchian(snapshots, value)
        } else {
            None
        }
    };
    let turtle = &rule.trigger.turtle;
    skill_turtle::TurtleChannels {
        entry1: period(turtle.entry1),
        entry2: period(turtle.entry2),
        exit1: period(turtle.exit1),
        exit2: period(turtle.exit2),
    }
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
    if open.is_empty() {
        if let Some(side) = ma_entry_signal(last, indicators, &rule.trigger) {
            if direction_allows(side, rule.steps.direction) {
                try_open(
                    state,
                    strategy,
                    rule,
                    venue,
                    symbol,
                    side,
                    last,
                    indicators.atr,
                    None,
                    None,
                )
                .await;
            }
        }
        return Ok(());
    }
    for trade in open {
        if ma_exit_hit(
            trade.side,
            last,
            trade.entry_price,
            indicators,
            &rule.trigger,
        ) {
            close_signal(state, trade.id).await;
        }
    }
    Ok(())
}

/// 网格：价格触及档位时单腿开仓，止盈设在相邻档位。
///
/// `take_profit_pct` / `stop_loss_pct` 在网格里表示整组仓位的止盈止损：
/// 按所有在持网格仓位的平均盈亏判断，触发后一起平仓。
async fn evaluate_grid(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    last: Decimal,
    open: &[listing::ListingTrade],
) -> Result<(), String> {
    let count = rule.business.grid_count() as usize;
    if open.len() >= count {
        return Ok(());
    }
    if !open.is_empty() && grid_group_exit(rule, last, open) {
        for trade in open {
            close_signal(state, trade.id).await;
        }
        return Ok(());
    }
    let key = format!("{}:{}:{symbol}", strategy.id, rule.id);
    let hits = state.skill.grid_triggers(&key, last, rule);
    for (side, _level, take_profit) in hits {
        let opened = try_open(
            state,
            strategy,
            rule,
            venue,
            symbol,
            side,
            last,
            None,
            Some(take_profit),
            None,
        )
        .await;
        if !opened && rule.business.grid_close_on_fail {
            // 触发风控或下单失败：按配置平掉该网格已有仓位。
            for trade in open {
                close_signal(state, trade.id).await;
            }
            break;
        }
    }
    Ok(())
}

/// 整组止盈止损：所有在持网格仓位的平均盈亏达到阈值时离场。
fn grid_group_exit(rule: &SkillRule, last: Decimal, open: &[listing::ListingTrade]) -> bool {
    let take_profit = rule.business.take_profit_pct;
    let stop_loss = rule.business.stop_loss_pct;
    if take_profit <= Decimal::ZERO && stop_loss <= Decimal::ZERO {
        return false;
    }
    let total: Decimal = open
        .iter()
        .map(|trade| listing::pnl_pct(trade.side, trade.entry_price, last))
        .sum();
    let average = total / Decimal::from(open.len());
    (take_profit > Decimal::ZERO && average >= take_profit)
        || (stop_loss > Decimal::ZERO && average <= -stop_loss)
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

/// 指标类技能开仓：遵守冷却、每日上限与单笔金额上限。
async fn try_open(
    state: &AppState,
    strategy: &SkillStrategy,
    rule: &SkillRule,
    venue: &str,
    symbol: &str,
    side: Side,
    last: Decimal,
    atr: Option<Decimal>,
    take_profit: Option<Decimal>,
    placement: Option<TurtlePlacement>,
) -> bool {
    let key = format!("{}:{}:{symbol}", strategy.id, rule.id);
    let is_add = placement.map(|p| p.is_add).unwrap_or(false);
    let spot = crate::market::is_spot_venue(venue);
    // 现货腿只能买入：做空信号直接跳过，避免把订单打到交易所才被拒。
    if spot && side == Side::Sell {
        tracing::info!(
            strategy_id = %strategy.id,
            %symbol,
            %venue,
            rule = %rule.name,
            "现货交易所不支持开空，忽略本次做空信号"
        );
        return false;
    }
    // 持仓数量按「策略 + 规则」统计，不区分交易对；加仓不受此限制。
    if !is_add && rule.cautions.max_positions > 0 {
        let held = state
            .listing
            .open_trades()
            .iter()
            .filter(|trade| {
                trade.strategy_id == Some(strategy.id) && trade.rule_id == Some(rule.id)
            })
            .count();
        if held as u32 >= rule.cautions.max_positions {
            tracing::info!(
                strategy_id = %strategy.id,
                %symbol,
                rule = %rule.name,
                "技能规则已达到最大持仓数量，忽略本次信号"
            );
            return false;
        }
    }
    if !is_add && state.skill.in_cooldown(&key, rule.cautions.cooldown_hours) {
        tracing::info!(
            strategy_id = %strategy.id,
            %symbol,
            rule = %rule.name,
            "技能规则仍在冷却时间内，忽略本次信号"
        );
        return false;
    }
    if !is_add && !state.skill.allow_daily(&key, rule.cautions.max_daily_opens) {
        tracing::info!(
            strategy_id = %strategy.id,
            %symbol,
            rule = %rule.name,
            "技能规则已达到当日开仓上限，忽略本次信号"
        );
        return false;
    }
    let paper = rule.steps.execution_mode == StrategyExecutionMode::Paper;
    // ATR 止损距离换算成百分比，供单笔风险定仓与风险回报比止盈共用。
    let atr_stop_pct = atr
        .filter(|value| *value > Decimal::ZERO)
        .filter(|_| rule.trigger.atr_multiple > Decimal::ZERO)
        .filter(|_| last > Decimal::ZERO)
        .map(|atr| atr * rule.trigger.atr_multiple / last * Decimal::from(100));
    // 网格把总投资额按档位数量均分；其余指标技能按整笔金额下单。
    let notional = if rule.kind == SkillKind::Grid {
        capped_notional(rule) * rule.business.grid_share()
    } else if rule.cautions.risk_pct > Decimal::ZERO {
        let equity = state.accounts.total_equity_usd();
        match atr_stop_pct.filter(|pct| *pct > Decimal::ZERO) {
            // 单笔风险 = 权益 × 风险比例，仓位名义价值 = 风险金额 ÷ 止损百分比。
            Some(stop_pct) if equity > Decimal::ZERO => {
                let risk_usd = equity * rule.cautions.risk_pct / Decimal::from(100);
                let size = risk_usd * Decimal::from(100) / stop_pct;
                if rule.kind != SkillKind::Breakout
                    && rule.cautions.max_notional_usd > Decimal::ZERO
                {
                    size.min(rule.cautions.max_notional_usd)
                } else {
                    size
                }
            }
            // 缺少 ATR 或权益时无法换算风险，回落到固定金额。
            _ => capped_notional(rule),
        }
    } else {
        capped_notional(rule)
    };
    if notional <= Decimal::ZERO {
        return false;
    }
    // 海龟按整组名义价值约束最大杠杆，避免加仓后超过配置上限。
    if rule.kind == SkillKind::Breakout {
        let equity = state.accounts.total_equity_usd();
        let max_leverage = if spot {
            Decimal::ONE
        } else {
            rule.business.max_leverage.max(Decimal::ONE)
        };
        if equity > Decimal::ZERO {
            let held_notional: Decimal = state
                .listing
                .open_trades()
                .into_iter()
                .filter(|trade| {
                    trade.strategy_id == Some(strategy.id)
                        && trade.rule_id == Some(rule.id)
                        && trade.symbol.eq_ignore_ascii_case(symbol)
                })
                .map(|trade| trade.notional_usd)
                .sum();
            if held_notional + notional > equity * max_leverage {
                tracing::info!(
                    %symbol,
                    rule = %rule.name,
                    "海龟整组名义价值将超过最大杠杆，忽略本次加仓"
                );
                return false;
            }
        }
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
    // 网格的止损按整组仓位判断，单笔止损交给相邻档位止盈逻辑；
    // 海龟的止损同样按整组判断，单笔止损保持 0，避免旧 Unit 先被单独止损。
    let stop_loss = if matches!(rule.kind, SkillKind::Grid | SkillKind::Breakout) {
        Decimal::ZERO
    } else {
        rule.business.stop_loss_pct
    };
    // 风险回报比以 ATR 止损距离为 1R 推导止盈；未配置时沿用固定止盈。
    let take_profit_pct = take_profit.unwrap_or_else(|| {
        if rule.business.risk_reward > Decimal::ZERO {
            atr_stop_pct
                .map(|stop_pct| stop_pct * rule.business.risk_reward)
                .unwrap_or(rule.business.take_profit_pct)
        } else {
            rule.business.take_profit_pct
        }
    });
    let leverage = if spot {
        // 现货没有杠杆，整组敞口也不能超过权益。
        Decimal::ONE
    } else if rule.kind == SkillKind::Breakout {
        rule.business.max_leverage.max(Decimal::ONE)
    } else {
        rule.business.leverage
    };
    let max_hold_secs = if matches!(rule.kind, SkillKind::Breakout | SkillKind::Program)
        && rule.business.max_hold_hours > Decimal::ZERO
    {
        (rule.business.max_hold_hours * Decimal::from(3600))
            .to_i64()
            .unwrap_or(0)
    } else {
        0
    };
    // 首仓生成组标识，后续加仓沿用，便于整组平仓与系统一盈利过滤。
    let group_id = placement.map(|p| p.group_id.unwrap_or_else(Uuid::new_v4));
    let request = OpenListingRequest {
        token: symbol.trim_end_matches("USDT").to_string(),
        headline,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        notional_usd: notional,
        leverage,
        take_profit_pct,
        stop_loss_pct: stop_loss,
        strategy_id: Some(strategy.id),
        rule_id: Some(rule.id),
        rule_name: Some(rule.name.clone()),
        system: placement.map(|p| p.system),
        group_id,
        unit_index: placement.map(|p| p.unit_index).unwrap_or(0),
        n_at_entry: if rule.kind == SkillKind::Breakout {
            atr
        } else {
            None
        },
        paper,
        skill: true,
        max_hold_secs,
        notify: rule.output.notify,
    };
    match listing::open_position(state, request).await {
        Ok(_) => {
            state.skill.mark_open(&key);
            true
        }
        Err(error) => {
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
            false
        }
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
                system: None,
                group_id: None,
                unit_index: 0,
                n_at_entry: None,
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
    fn skill_runtime_manages_templates_and_frozen_settings() {
        let mut definition = crate::templates::builtin().remove(0).definition;
        definition.name = "自定义策略".to_string();

        // 没有技能绑定的自定义策略按沙箱执行。
        let custom = StrategyRecord::new(
            definition.clone(),
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        assert!(!manages(&custom));

        // 绑定内置模板的策略由技能运行时接管。
        let mut bound = StrategyRecord::new(
            definition.clone(),
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        bound.skill = Some(arb_core::SkillBinding {
            template_key: "grid".to_string(),
            ..arb_core::SkillBinding::default()
        });
        assert!(manages(&bound));

        // 未知模板标识但没有参数：无法确定触发方式，不接管。
        let mut unknown = StrategyRecord::new(
            definition.clone(),
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        unknown.skill = Some(arb_core::SkillBinding {
            template_key: "third-party".to_string(),
            ..arb_core::SkillBinding::default()
        });
        assert!(!manages(&unknown));

        // 第三方技能携带冻结规则参数：无需新增模板标识即可接管。
        let mut third_party = StrategyRecord::new(
            definition,
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        third_party.skill = Some(arb_core::SkillBinding {
            template_key: "third-party".to_string(),
            settings: Some(serde_json::to_value(frozen_fixture()).expect("技能参数可序列化")),
            ..arb_core::SkillBinding::default()
        });
        assert!(manages(&third_party));
    }

    #[test]
    fn frozen_settings_roundtrip_keeps_authored_rules() {
        let authored = frozen_fixture();
        let mut definition = crate::templates::builtin().remove(0).definition;
        definition.name = "第三方策略".to_string();
        let mut record = StrategyRecord::new(
            definition,
            arb_core::StrategySource::Manual,
            StrategyStatus::Running,
        );
        record.skill = Some(arb_core::SkillBinding {
            template_key: "third-party-news".to_string(),
            settings: Some(serde_json::to_value(&authored).expect("技能参数可序列化")),
            ..arb_core::SkillBinding::default()
        });

        // 接管不依赖模板白名单；参数从绑定解析并补齐默认值。
        assert!(manages(&record));
        assert!(kind_for(&record).is_none());
        let restored = frozen_settings(&record)
            .expect("冻结参数可解析")
            .normalized(Decimal::ONE);

        let mut actual = restored;
        let mut expected = authored.normalized(Decimal::ONE);
        for rule in actual.rules.iter_mut().chain(expected.rules.iter_mut()) {
            rule.id = Uuid::nil();
        }
        assert_eq!(
            serde_json::to_value(&actual).expect("序列化"),
            serde_json::to_value(&expected).expect("序列化")
        );
        // 启动后按规则自身的类型扫描，而不是模板标识。
        let rule = actual.enabled_rules().next().expect("至少一条启用规则");
        assert_eq!(rule.kind, SkillKind::Event);
        assert_eq!(rule.trigger.event_keywords, vec!["will list".to_string()]);
        assert_eq!(rule.business.notional_usd, Decimal::from(50));
    }

    #[test]
    fn program_rule_normalizes_and_syncs_risk() {
        let mut settings = default_settings(SkillKind::Program, Decimal::from(2));
        let rule = settings.rules.first_mut().expect("默认程序规则");
        let program = rule.program.as_mut().expect("默认程序");
        program.data.timeframe = "4h".to_string();
        program.data.lookback = 120;
        program.risk.stop_loss_pct = Decimal::from(8);
        program.risk.take_profit_pct = Decimal::from(16);
        program.risk.max_hold_hours = Decimal::from(12);
        program.risk.cooldown_hours = Decimal::from(3);
        program.sizing.notional_usd = Decimal::from(250);
        program.sizing.risk_pct = Decimal::from(2);

        let normalized = settings.normalized(Decimal::from(2));
        let rule = &normalized.rules[0];
        assert_eq!(rule.kind, SkillKind::Program);
        assert_eq!(rule.trigger.timeframe, "4h");
        assert_eq!(rule.trigger.lookback, 120);
        assert_eq!(rule.business.stop_loss_pct, Decimal::from(8));
        assert_eq!(rule.business.take_profit_pct, Decimal::from(16));
        assert_eq!(rule.business.max_hold_hours, Decimal::from(12));
        assert_eq!(rule.cautions.cooldown_hours, Decimal::from(3));
        assert_eq!(rule.business.notional_usd, Decimal::from(250));
        assert_eq!(rule.cautions.risk_pct, Decimal::from(2));
        assert!(rule.program.is_some());
    }

    fn frozen_fixture() -> SkillSettings {
        serde_json::from_str(
            r#"{"rules":[{
                "name":"第三方公告",
                "purpose":"上币公告后短线做多",
                "trigger":{"event_sources":["binance"],"event_keywords":["will list"],"event_window_secs":900},
                "steps":{"execution_mode":"paper","execution_venues":["okx"]},
                "business":{"notional_usd":"50","leverage":"2","take_profit_pct":"15","stop_loss_pct":"8"},
                "output":{"action":"open_long","notify":true},
                "cautions":{"max_notional_usd":"40","max_daily_opens":3}
            }]}"#,
        )
        .expect("技能参数 JSON 合法")
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
        assert_eq!(rule.business.grid_count, 10);
        assert_eq!(rule.business.grid_count(), 10);
        assert_eq!(rule.steps.direction, SkillDirection::Both);
        assert_eq!(rule.business.grid_spacing_mode, GridSpacingMode::Arithmetic);
    }

    #[test]
    fn legacy_grid_levels_migrate_to_grid_count() {
        let mut rules = SkillBusinessRules::default();
        rules.grid_count = 0;
        rules.grid_levels = 3;
        assert_eq!(rules.grid_count(), 6);
        rules.grid_count = 12;
        assert_eq!(rules.grid_count(), 12);
    }

    #[test]
    fn grid_levels_follow_range_and_mode() {
        let arithmetic = grid_price_levels(
            Decimal::from(100),
            Decimal::from(110),
            3,
            GridSpacingMode::Arithmetic,
        );
        assert_eq!(
            arithmetic,
            vec![Decimal::from(100), Decimal::from(105), Decimal::from(110)]
        );
        let geometric = grid_price_levels(
            Decimal::from(100),
            Decimal::from(110),
            3,
            GridSpacingMode::Geometric,
        );
        assert_eq!(geometric.first(), Some(&Decimal::from(100)));
        assert_eq!(geometric.last(), Some(&Decimal::from(110)));
        assert!(geometric[0] < geometric[1] && geometric[1] < geometric[2]);
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
