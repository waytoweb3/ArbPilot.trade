use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{risk::RiskLimits, types::PositionSide};

/// 策略运行状态。草稿与已停止的策略不会产生任何订单意图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyStatus {
    Draft,
    Running,
    Paused,
    Stopped,
}

/// 策略来源，用于审计 AI 生成与用户手写的差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategySource {
    BuiltIn,
    Manual,
    Ai,
}

/// 策略类型。内置类型由执行器直接实现，其他类型由沙箱代码定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyKind {
    /// 内置资金费率套利：现货多 + 永续空。
    FundingRate,
    /// 沙箱代码策略：交易范围、入场与出场都由策略代码决定。
    Custom,
}

impl Default for StrategyKind {
    fn default() -> Self {
        Self::FundingRate
    }
}

/// 策略运行位置：未发布的策略跑在用户自己的执行器，发布到市场后由平台托管执行器运行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyRunScope {
    /// 已发布到市场，归平台托管执行器运行。
    Platform,
    /// 默认：运行在用户自己的执行器。
    #[default]
    SelfHosted,
}

/// 下单模式：默认真实下单，也可切到模拟，只记录信号与成交事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyExecutionMode {
    /// 默认：通过统一风控网关把订单提交到交易所。
    #[default]
    Real,
    /// 模拟下单：不向交易所提交任何委托，只在策略事件里记录成交。
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instrument {
    Spot,
    Perp,
}

fn default_instruments() -> Vec<Instrument> {
    vec![Instrument::Spot, Instrument::Perp]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyUniverse {
    /// 双腿所在交易所，例如 `binance-spot` + `binance`。
    pub venues: Vec<String>,
    pub symbols: Vec<String>,
    #[serde(default = "default_instruments")]
    pub instruments: Vec<Instrument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRules {
    /// 资金费率下限（小数，如 0.0003 表示 3 bps）。
    pub funding_spread_min: Decimal,
    /// 扣费后单期净收益下限（小数）。
    pub net_edge_min: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitRules {
    /// 资金费率低于该值时平仓（小数）。
    pub spread_below: Decimal,
    pub max_holding_hours: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRisk {
    /// 单笔名义价值上限，必须小于等于执行器级风控。
    pub max_notional_usd: Decimal,
    /// 双腿未完全成交时允许的最长裸露时间（秒）。
    #[serde(default = "default_unhedged_seconds")]
    pub max_unhedged_seconds: u64,
}

fn default_unhedged_seconds() -> u64 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRules {
    #[serde(default = "default_leverage")]
    pub leverage: Decimal,
    #[serde(default = "default_slippage_bps")]
    pub max_slippage_bps: Decimal,
    /// 两次开仓之间的最小间隔，避免行情抖动时连续下单。
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

fn default_leverage() -> Decimal {
    Decimal::ONE
}

fn default_slippage_bps() -> Decimal {
    Decimal::from(10)
}

fn default_cooldown_secs() -> u64 {
    300
}

/// 策略可调参数：值随定义冻结，沙箱代码通过 `param("key")` 读取。
///
/// 每项同时携带前端渲染所需的取值范围，模板与 AI 生成策略都按这套结构声明参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyParam {
    /// 代码里的读取键，只允许字母、数字与下划线。
    pub key: String,
    /// 前端展示名。
    pub label: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    #[serde(default = "default_param_step")]
    pub step: f64,
    /// 展示单位，如 `%`、`根`、`倍`。
    #[serde(default)]
    pub unit: Option<String>,
}

fn default_param_step() -> f64 {
    1.0
}

impl StrategyParam {
    /// 键名是否合法：小写字母、数字与下划线，长度不超过 32。
    pub fn valid_key(&self) -> bool {
        !self.key.is_empty()
            && self.key.chars().count() <= 32
            && self
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// 取值范围是否自洽。
    pub fn valid_range(&self) -> bool {
        self.value.is_finite()
            && self.min.is_finite()
            && self.max.is_finite()
            && self.step.is_finite()
            && self.min < self.max
            && self.step > 0.0
            && self.value >= self.min
            && self.value <= self.max
    }
}

/// 沙箱策略的数据窗口：主机据此拉取 K 线并计算指标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataRules {
    /// K 线周期，见 `SUPPORTED_TIMEFRAMES`。
    #[serde(default = "default_timeframe")]
    pub timeframe: String,
    /// 拉取的 K 线根数，必须覆盖最长的指标窗口。
    #[serde(default = "default_lookback")]
    pub lookback: u32,
    /// 快均线周期。
    #[serde(default = "default_fast")]
    pub fast: u32,
    /// 慢均线周期。
    #[serde(default = "default_slow")]
    pub slow: u32,
    /// 参与计算的均线周期集合（去重升序）；为空表示只用 `fast` 与 `slow`。
    ///
    /// 指标层只负责算出这些均线，具体哪条均线参与入场/出场由策略规则决定。
    #[serde(default)]
    pub ma_periods: Vec<u32>,
    /// ATR 周期。
    #[serde(default = "default_atr")]
    pub atr: u32,
    /// 突破/通道周期。
    #[serde(default = "default_channel")]
    pub channel: u32,
    /// 事件来源白名单（如 binance / okx）；为空表示接受全部来源。
    #[serde(default)]
    pub event_sources: Vec<String>,
    /// 事件关键词：标题包含任一关键词才会进入沙箱；为空表示不按关键词过滤。
    #[serde(default)]
    pub event_keywords: Vec<String>,
    /// 事件窗口（秒）：只注入最近这段时间内发布的公告。
    #[serde(default = "default_event_window_secs")]
    pub event_window_secs: u64,
    /// 策略自己的可调参数，代码通过 `param("key")` 读取。
    #[serde(default)]
    pub params: Vec<StrategyParam>,
}

/// 主机支持的 K 线周期，与行情采集保持一致。
pub const SUPPORTED_TIMEFRAMES: [&str; 8] = ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

fn default_timeframe() -> String {
    "5m".to_string()
}

fn default_lookback() -> u32 {
    200
}

fn default_fast() -> u32 {
    20
}

fn default_slow() -> u32 {
    55
}

fn default_atr() -> u32 {
    20
}

fn default_channel() -> u32 {
    20
}

fn default_event_window_secs() -> u64 {
    86_400
}

impl Default for DataRules {
    fn default() -> Self {
        Self {
            timeframe: default_timeframe(),
            lookback: default_lookback(),
            fast: default_fast(),
            slow: default_slow(),
            ma_periods: Vec::new(),
            atr: default_atr(),
            channel: default_channel(),
            event_sources: Vec::new(),
            event_keywords: Vec::new(),
            event_window_secs: default_event_window_secs(),
            params: Vec::new(),
        }
    }
}

/// 回测使用的成本假设，与实盘适配器的手续费/滑点口径保持一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostAssumptions {
    /// 单腿单边手续费（bps）。
    pub fee_bps_per_leg: Decimal,
    /// 单腿滑点（bps）。
    pub slippage_bps: Decimal,
}

impl Default for CostAssumptions {
    fn default() -> Self {
        Self {
            fee_bps_per_leg: Decimal::from(4),
            slippage_bps: Decimal::ONE,
        }
    }
}

/// 版本化、可校验的策略定义；回测与实盘共用同一份结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDefinition {
    pub name: String,
    #[serde(default)]
    pub kind: StrategyKind,
    pub universe: StrategyUniverse,
    pub entry: EntryRules,
    pub exit: ExitRules,
    pub risk: StrategyRisk,
    pub execution: ExecutionRules,
    /// 沙箱策略的 K 线与指标窗口；内置套利不读取。
    #[serde(default)]
    pub data: DataRules,
    #[serde(default)]
    pub costs: CostAssumptions,
    /// 自然语言说明：解释入场、退出、阻止交易的条件与未决假设。
    #[serde(default)]
    pub notes: Option<String>,
}

impl StrategyDefinition {
    /// 现货腿（约定以 `-spot` 结尾）。
    pub fn spot_venue(&self) -> Option<&str> {
        self.universe
            .venues
            .iter()
            .find(|venue| venue.ends_with("-spot"))
            .map(String::as_str)
    }

    /// 永续腿：现货腿之外的另一家交易所。
    pub fn perp_venue(&self) -> Option<&str> {
        self.universe
            .venues
            .iter()
            .find(|venue| !venue.ends_with("-spot"))
            .map(String::as_str)
    }

    /// 单腿策略的默认交易所：优先永续，其次第一家。
    pub fn trade_venue(&self) -> Option<&str> {
        self.perp_venue()
            .or_else(|| self.universe.venues.first().map(String::as_str))
    }

    /// 语义校验。结构合法不代表策略正确，仍需回测验证。
    pub fn validate(&self, limits: &RiskLimits) -> StrategyValidation {
        // 用 RefCell 让两个闭包共享同一个问题列表，避免重复样板代码。
        let issues = std::cell::RefCell::new(Vec::new());
        let error = |path: &str, message: String| {
            issues.borrow_mut().push(ValidationIssue {
                path: path.to_string(),
                severity: ValidationSeverity::Error,
                message,
            });
        };
        let warning = |path: &str, message: String| {
            issues.borrow_mut().push(ValidationIssue {
                path: path.to_string(),
                severity: ValidationSeverity::Warning,
                message,
            });
        };

        if self.name.trim().is_empty() {
            error("name", "策略名称不能为空".to_string());
        } else if self.name.chars().count() > 80 {
            error("name", "策略名称不能超过 80 个字符".to_string());
        }

        if self.universe.symbols.is_empty() {
            error("universe.symbols", "至少需要一个交易对".to_string());
        }
        for symbol in &self.universe.symbols {
            if symbol.trim().is_empty() || !symbol.chars().all(|c| c.is_ascii_alphanumeric()) {
                error("universe.symbols", format!("交易对名称不合法: {symbol}"));
            }
            if !limits.allowed_symbols.is_empty() && !limits.allowed_symbols.contains(symbol) {
                warning(
                    "universe.symbols",
                    format!("{symbol} 不在执行器允许列表内，开仓会被风控拒绝"),
                );
            }
        }

        // 交易所与入场/出场规则按策略类型校验：
        // 内置套利固定「一现货 + 一永续」；沙箱策略的交易范围由代码自行决定。
        match self.kind {
            StrategyKind::FundingRate => {
                let mut venues = self.universe.venues.clone();
                venues.sort();
                venues.dedup();
                if self.universe.venues.len() != 2 || venues.len() != 2 {
                    error(
                        "universe.venues",
                        "资金费率策略必须使用两个不同交易所（现货腿 + 永续腿）".to_string(),
                    );
                }
                if self.spot_venue().is_none() {
                    error(
                        "universe.venues",
                        "缺少现货腿：请使用以 -spot 结尾的交易所标识".to_string(),
                    );
                }
                if self.perp_venue().is_none() {
                    error(
                        "universe.venues",
                        "缺少永续腿：请提供现货腿之外的交易所标识".to_string(),
                    );
                }
                if !self.universe.instruments.contains(&Instrument::Spot)
                    || !self.universe.instruments.contains(&Instrument::Perp)
                {
                    error(
                        "universe.instruments",
                        "资金费率策略需要同时包含现货与永续合约".to_string(),
                    );
                }

                if self.entry.funding_spread_min < Decimal::ZERO {
                    error(
                        "entry.funding_spread_min",
                        "资金费率下限不能为负".to_string(),
                    );
                } else if self.entry.funding_spread_min == Decimal::ZERO {
                    warning(
                        "entry.funding_spread_min",
                        "资金费率下限为 0，任何非负费率都会触发入场".to_string(),
                    );
                }
                if self.entry.net_edge_min < Decimal::ZERO {
                    error("entry.net_edge_min", "净收益下限不能为负".to_string());
                }

                if self.exit.spread_below < Decimal::ZERO {
                    error("exit.spread_below", "退出阈值不能为负".to_string());
                }
                if self.exit.spread_below >= self.entry.funding_spread_min {
                    error(
                        "exit.spread_below",
                        "退出阈值必须小于入场阈值，否则开仓后立即离场".to_string(),
                    );
                }
                if self.exit.max_holding_hours <= Decimal::ZERO {
                    error(
                        "exit.max_holding_hours",
                        "最长持仓时间必须大于 0".to_string(),
                    );
                }
            }
            StrategyKind::Custom => {
                if self.universe.venues.is_empty() {
                    error("universe.venues", "至少需要一个交易所".to_string());
                }
                if self.universe.instruments.is_empty() {
                    error("universe.instruments", "至少需要一种交易品种".to_string());
                }

                if !SUPPORTED_TIMEFRAMES.contains(&self.data.timeframe.trim()) {
                    error(
                        "data.timeframe",
                        format!("不支持的 K 线周期: {}", self.data.timeframe),
                    );
                }
                if !(20..=1_000).contains(&self.data.lookback) {
                    error(
                        "data.lookback",
                        "K 线根数必须在 20 到 1000 之间".to_string(),
                    );
                }
                if !(60..=604_800).contains(&self.data.event_window_secs) {
                    error(
                        "data.event_window_secs",
                        "事件窗口必须在 60 到 604800 秒之间".to_string(),
                    );
                }
                for (path, window) in [
                    ("data.fast", self.data.fast),
                    ("data.slow", self.data.slow),
                    ("data.atr", self.data.atr),
                    ("data.channel", self.data.channel),
                ] {
                    if !(2..=200).contains(&window) {
                        error(path, "指标周期必须在 2 到 200 之间".to_string());
                    } else if window > self.data.lookback {
                        error(path, "指标周期不能超过 K 线根数".to_string());
                    }
                }
                for period in &self.data.ma_periods {
                    if !(2..=1_000).contains(period) {
                        error(
                            "data.ma_periods",
                            "均线周期必须在 2 到 1000 之间".to_string(),
                        );
                    } else if *period > self.data.lookback {
                        error("data.ma_periods", "均线周期不能超过 K 线根数".to_string());
                    }
                }

                if self.data.params.len() > 24 {
                    error("data.params", "可调参数最多 24 个".to_string());
                }
                let mut keys = std::collections::HashSet::new();
                for param in &self.data.params {
                    if !param.valid_key() {
                        error(
                            "data.params",
                            format!("参数键 {} 只能包含字母、数字与下划线", param.key),
                        );
                    } else if !keys.insert(param.key.as_str()) {
                        error("data.params", format!("参数键重复: {}", param.key));
                    }
                    if param.label.trim().is_empty() {
                        error("data.params", format!("参数 {} 缺少展示名", param.key));
                    }
                    if !param.valid_range() {
                        error(
                            "data.params",
                            format!(
                                "参数 {} 的取值范围不合法：{} 不在 [{} , {}] 内或步长为正",
                                param.key, param.value, param.min, param.max
                            ),
                        );
                    }
                }
            }
        }

        for venue in &self.universe.venues {
            if !limits.allowed_venues.is_empty() && !limits.allowed_venues.contains(venue) {
                warning(
                    "universe.venues",
                    format!("{venue} 不在执行器允许列表内，开仓会被风控拒绝"),
                );
            }
        }

        if self.risk.max_notional_usd <= Decimal::ZERO {
            error(
                "risk.max_notional_usd",
                "单笔名义价值必须大于 0".to_string(),
            );
        } else if self.risk.max_notional_usd > limits.max_order_notional_usd {
            error(
                "risk.max_notional_usd",
                format!(
                    "单笔名义价值 {} 超过执行器上限 {}",
                    self.risk.max_notional_usd, limits.max_order_notional_usd
                ),
            );
        }
        // 单腿策略没有双腿裸露概念，该项只对内置套利生效。
        if self.kind == StrategyKind::FundingRate {
            if self.risk.max_unhedged_seconds > 60 {
                error(
                    "risk.max_unhedged_seconds",
                    "双腿裸露时间上限不能超过 60 秒".to_string(),
                );
            } else if self.risk.max_unhedged_seconds > 10 {
                warning(
                    "risk.max_unhedged_seconds",
                    "裸露时间超过 10 秒，单腿失败时风险较高".to_string(),
                );
            }
        }

        if self.execution.leverage < Decimal::ONE {
            error("execution.leverage", "杠杆不能小于 1".to_string());
        } else if self.execution.leverage > limits.max_leverage {
            error(
                "execution.leverage",
                format!(
                    "杠杆 {} 超过执行器上限 {}",
                    self.execution.leverage, limits.max_leverage
                ),
            );
        }
        if self.execution.max_slippage_bps < Decimal::ZERO {
            error("execution.max_slippage_bps", "最大滑点不能为负".to_string());
        } else if self.execution.max_slippage_bps > limits.max_slippage_bps {
            error(
                "execution.max_slippage_bps",
                format!(
                    "最大滑点 {} bps 超过执行器上限 {} bps",
                    self.execution.max_slippage_bps, limits.max_slippage_bps
                ),
            );
        }
        if self.execution.cooldown_secs < 10 {
            warning(
                "execution.cooldown_secs",
                "冷却时间低于 10 秒，可能产生重复开仓".to_string(),
            );
        }

        if self.costs.fee_bps_per_leg < Decimal::ZERO {
            error("costs.fee_bps_per_leg", "手续费假设不能为负".to_string());
        }
        if self.costs.slippage_bps < Decimal::ZERO {
            error("costs.slippage_bps", "滑点假设不能为负".to_string());
        }
        if self.costs.fee_bps_per_leg == Decimal::ZERO && self.costs.slippage_bps == Decimal::ZERO {
            warning("costs", "成本假设为 0，回测结果会明显偏乐观".to_string());
        }

        if self.notes.as_deref().unwrap_or("").trim().is_empty() {
            warning(
                "notes",
                "建议补充策略说明，便于复核入场、退出与未决假设".to_string(),
            );
        }

        let mut issues = issues.into_inner();
        issues.sort_by(|a, b| {
            a.severity
                .rank()
                .cmp(&b.severity.rank())
                .then_with(|| a.path.cmp(&b.path))
        });

        StrategyValidation {
            issues,
            validated_at: Some(Utc::now()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Error,
    Warning,
}

impl ValidationSeverity {
    fn rank(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Warning => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// 定义中的字段路径，如 `entry.funding_spread_min`。
    pub path: String,
    pub severity: ValidationSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyValidation {
    pub issues: Vec<ValidationIssue>,
    pub validated_at: Option<DateTime<Utc>>,
}

impl StrategyValidation {
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
            .count()
    }
}

/// 回测/回放使用的一条资金费率结算记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingSample {
    pub symbol: String,
    pub at: DateTime<Utc>,
    pub rate: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub at: DateTime<Utc>,
    pub cumulative_pnl_usd: Decimal,
    pub symbol: String,
}

/// 回测采用的简化假设，由前端翻译展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacktestAssumption {
    /// 现货与永续基差按 0 处理，只计算资金费率与成本。
    ZeroBasis,
    /// 按结算价（mark price）成交，不考虑部分成交。
    MarkPriceFills,
    /// 开平仓各两条腿，合计四条腿手续费。
    RoundTripFees,
    /// 开平仓各两条腿滑点。
    RoundTripSlippage,
    /// 使用历史已结算资金费率，不含预测费率。
    SettledFundingOnly,
    /// net_edge_min 按单期净费率下限解释。
    NetEdgePerSettlement,
    /// 逐根 K 线重放沙箱模块，模块每根只能看到窗口内的行情。
    WasmBarReplay,
    /// 按每根 K 线收盘价成交，未模拟盘中价格路径。
    BarCloseFills,
    /// 影子挂单在 K 线高低点触及时按挂单价成交。
    LimitFillOnTouch,
    /// 回测不重放历史公告，事件输入始终为空；依赖事件的策略只能实测。
    NoEventReplay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub strategy_name: String,
    pub definition_version: u32,
    pub generated_at: DateTime<Utc>,
    pub sample_start: Option<DateTime<Utc>>,
    pub sample_end: Option<DateTime<Utc>>,
    pub settlements: usize,
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub gross_funding_usd: Decimal,
    pub fees_usd: Decimal,
    pub slippage_usd: Decimal,
    pub net_pnl_usd: Decimal,
    /// 相对单笔名义价值的收益率（百分数）。
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub in_sample_net_pnl_usd: Decimal,
    pub out_of_sample_net_pnl_usd: Decimal,
    pub assumptions: Vec<BacktestAssumption>,
    pub equity_curve: Vec<EquityPoint>,
}

struct OpenTrade {
    opened_at: DateTime<Utc>,
    funding_usd: Decimal,
}

/// 资金费率策略回测：同一份定义用于回测与实盘。
///
/// 只使用已结算历史费率，按「现货多 + 永续空」收取正资金费；
/// 收益 = 累计资金费 - 开平仓手续费 - 滑点。
pub fn run_backtest(
    definition: &StrategyDefinition,
    samples: &[FundingSample],
    definition_version: u32,
) -> BacktestReport {
    let mut ordered = samples.to_vec();
    ordered.sort_by(|a, b| a.at.cmp(&b.at));

    let notional = definition.risk.max_notional_usd;
    let round_trip_fee =
        notional * (definition.costs.fee_bps_per_leg * Decimal::from(4)) / Decimal::from(10_000);
    let round_trip_slippage =
        notional * (definition.costs.slippage_bps * Decimal::from(2)) / Decimal::from(10_000);
    let entry_threshold = definition
        .entry
        .funding_spread_min
        .max(definition.entry.net_edge_min);

    let mut open: HashMap<String, OpenTrade> = HashMap::new();
    let mut gross = Decimal::ZERO;
    let mut fees = Decimal::ZERO;
    let mut slippage = Decimal::ZERO;
    let mut trades = 0usize;
    let mut wins = 0usize;
    let mut equity = Decimal::ZERO;
    let mut peak = Decimal::ZERO;
    let mut max_drawdown_usd = Decimal::ZERO;
    let mut curve = Vec::new();
    let mut closed: Vec<(DateTime<Utc>, Decimal)> = Vec::new();

    let split_at = ordered.get(ordered.len() * 7 / 10).map(|sample| sample.at);

    for sample in &ordered {
        if !open.contains_key(&sample.symbol) {
            if sample.rate < entry_threshold {
                continue;
            }
            open.insert(
                sample.symbol.clone(),
                OpenTrade {
                    opened_at: sample.at,
                    funding_usd: Decimal::ZERO,
                },
            );
        }

        let Some(trade) = open.get_mut(&sample.symbol) else {
            continue;
        };
        // 永续空头收取正资金费，负费率时支付。
        trade.funding_usd += sample.rate * notional;

        let held_hours =
            Decimal::from((sample.at - trade.opened_at).num_minutes()) / Decimal::from(60);
        if sample.rate >= definition.exit.spread_below
            && held_hours < definition.exit.max_holding_hours
        {
            continue;
        }

        let trade = open.remove(&sample.symbol).expect("持仓状态缺失");
        gross += trade.funding_usd;
        fees += round_trip_fee;
        slippage += round_trip_slippage;
        let pnl = trade.funding_usd - round_trip_fee - round_trip_slippage;
        trades += 1;
        if pnl > Decimal::ZERO {
            wins += 1;
        }
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let drawdown = peak - equity;
        if drawdown > max_drawdown_usd {
            max_drawdown_usd = drawdown;
        }
        closed.push((trade.opened_at, pnl));
        curve.push(EquityPoint {
            at: sample.at,
            cumulative_pnl_usd: equity,
            symbol: sample.symbol.clone(),
        });
    }

    // 样本结束时强制平仓，避免未实现收益被忽略。
    let last_at = ordered.last().map(|sample| sample.at);
    let mut remaining: Vec<_> = open.into_iter().collect();
    remaining.sort_by(|a, b| a.1.opened_at.cmp(&b.1.opened_at));
    if let Some(at) = last_at {
        for (symbol, trade) in remaining {
            gross += trade.funding_usd;
            fees += round_trip_fee;
            slippage += round_trip_slippage;
            let pnl = trade.funding_usd - round_trip_fee - round_trip_slippage;
            trades += 1;
            if pnl > Decimal::ZERO {
                wins += 1;
            }
            equity += pnl;
            if equity > peak {
                peak = equity;
            }
            let drawdown = peak - equity;
            if drawdown > max_drawdown_usd {
                max_drawdown_usd = drawdown;
            }
            closed.push((trade.opened_at, pnl));
            curve.push(EquityPoint {
                at,
                cumulative_pnl_usd: equity,
                symbol,
            });
        }
    }

    let mut in_sample = Decimal::ZERO;
    let mut out_of_sample = Decimal::ZERO;
    if let Some(split_at) = split_at {
        for (opened_at, pnl) in &closed {
            if *opened_at <= split_at {
                in_sample += *pnl;
            } else {
                out_of_sample += *pnl;
            }
        }
    } else {
        in_sample = equity;
    }

    let return_pct = if notional > Decimal::ZERO {
        (equity / notional * Decimal::from(100)).round_dp(4)
    } else {
        Decimal::ZERO
    };
    let max_drawdown_pct = if notional > Decimal::ZERO {
        (max_drawdown_usd / notional * Decimal::from(100)).round_dp(4)
    } else {
        Decimal::ZERO
    };

    BacktestReport {
        strategy_name: definition.name.clone(),
        definition_version,
        generated_at: Utc::now(),
        sample_start: ordered.first().map(|sample| sample.at),
        sample_end: last_at,
        settlements: ordered.len(),
        trades,
        wins,
        losses: trades.saturating_sub(wins),
        gross_funding_usd: gross.round_dp(4),
        fees_usd: fees.round_dp(4),
        slippage_usd: slippage.round_dp(4),
        net_pnl_usd: equity.round_dp(4),
        return_pct,
        max_drawdown_pct,
        in_sample_net_pnl_usd: in_sample.round_dp(4),
        out_of_sample_net_pnl_usd: out_of_sample.round_dp(4),
        assumptions: vec![
            BacktestAssumption::ZeroBasis,
            BacktestAssumption::MarkPriceFills,
            BacktestAssumption::RoundTripFees,
            BacktestAssumption::RoundTripSlippage,
            BacktestAssumption::SettledFundingOnly,
            BacktestAssumption::NetEdgePerSettlement,
        ],
        equity_curve: downsample(curve, 240),
    }
}

/// 回测曲线最多保留 240 个点，避免接口与前端负担过大。
fn downsample(points: Vec<EquityPoint>, limit: usize) -> Vec<EquityPoint> {
    if points.len() <= limit {
        return points;
    }
    let step = (points.len() as f64 / limit as f64).ceil() as usize;
    let mut sampled: Vec<EquityPoint> = points.iter().step_by(step.max(1)).cloned().collect();
    if let Some(last) = points.last() {
        if sampled.last().map(|point| point.at) != Some(last.at) {
            sampled.push(last.clone());
        }
    }
    sampled
}

/// 单条策略的运行时状态（不持久化，重启后由执行器重新对账）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyRuntime {
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_signal_at: Option<DateTime<Utc>>,
    pub last_order_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub signals: u64,
    pub orders: u64,
    /// 当前运行实例；重启后由运行器重新建立。
    #[serde(default)]
    pub run_id: Option<Uuid>,
    pub open: Option<StrategyOpenPosition>,
    /// 尚未触发的限价意图（网格挂单），价格触及时才下单。
    #[serde(default)]
    pub pending: Vec<StrategyPendingOrder>,
    pub events: Vec<StrategyEvent>,
}

/// 策略侧登记的一笔待触发限价意图。
///
/// 它只是执行器内存/状态里的影子挂单，不会提交到交易所：价格触及时执行器才通过
/// 统一风控网关下真实订单，因此不需要交易所撤单接口，也不会遗留无人管理的挂单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyPendingOrder {
    pub symbol: String,
    /// 挂单所在交易所；为空时使用策略的主交易所。
    #[serde(default)]
    pub venue: Option<String>,
    pub side: PositionSide,
    /// 触发价：买单在卖一跌破该价时触发，卖单在买一涨过该价时触发。
    pub price: Decimal,
    pub notional_usd: Decimal,
    pub created_at: DateTime<Utc>,
}

impl StrategyPendingOrder {
    /// 同一交易对、同一方向且价格几乎相同的挂单视为同一个网格档位。
    pub fn same_level(&self, other: &Self) -> bool {
        self.symbol == other.symbol
            && self.side == other.side
            && self.venue == other.venue
            && (self.price - other.price).abs() <= self.price.abs() * Decimal::new(1, 4)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyOpenPosition {
    pub symbol: String,
    /// 配对套利的现货腿；单腿持仓为空。
    #[serde(default)]
    pub long_venue: String,
    /// 配对套利的永续腿；单腿持仓为空。
    #[serde(default)]
    pub short_venue: String,
    /// 单腿持仓的交易所。
    #[serde(default)]
    pub venue: Option<String>,
    /// 单腿持仓方向；配对套利为空。
    #[serde(default)]
    pub side: Option<PositionSide>,
    /// 单腿持仓的开仓价。
    #[serde(default)]
    pub entry_price: Option<Decimal>,
    pub notional_usd: Decimal,
    pub opened_at: DateTime<Utc>,
    pub entry_net_edge_bps: Decimal,
}

impl StrategyOpenPosition {
    /// 配对套利持仓需要两条腿；单腿持仓只看 `venue` + `side`。
    pub fn is_pair(&self) -> bool {
        !self.long_venue.is_empty() && !self.short_venue.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyEventLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvent {
    pub at: DateTime<Utc>,
    pub level: StrategyEventLevel,
    pub message: String,
    pub order_id: Option<Uuid>,
}

impl StrategyRuntime {
    const MAX_EVENTS: usize = 20;
    /// 单条策略最多保留的待触发挂单数，避免网格无限累积。
    pub const MAX_PENDING: usize = 32;

    /// 登记一笔待触发限价意图；同档位已存在时只更新名义价值。
    ///
    /// 返回 `true` 表示运行状态发生了变化，调用方需要落库。
    pub fn upsert_pending(&mut self, order: StrategyPendingOrder) -> bool {
        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|existing| existing.same_level(&order))
        {
            if existing.notional_usd == order.notional_usd {
                return false;
            }
            existing.notional_usd = order.notional_usd;
            return true;
        }
        if self.pending.len() >= Self::MAX_PENDING {
            return false;
        }
        self.pending.push(order);
        true
    }

    /// 撤销某交易对的全部待触发挂单，返回撤销条数。
    pub fn clear_pending(&mut self, symbol: &str) -> usize {
        let before = self.pending.len();
        self.pending.retain(|order| order.symbol != symbol);
        before - self.pending.len()
    }

    pub fn record(
        &mut self,
        level: StrategyEventLevel,
        message: impl Into<String>,
        order_id: Option<Uuid>,
    ) {
        let message = message.into();
        if level != StrategyEventLevel::Info {
            self.last_error = Some(message.clone());
        }
        self.events.push(StrategyEvent {
            at: Utc::now(),
            level,
            message,
            order_id,
        });
        if self.events.len() > Self::MAX_EVENTS {
            let excess = self.events.len() - Self::MAX_EVENTS;
            self.events.drain(0..excess);
        }
    }
}

/// 技能参数运行时绑定：用稳定的模板标识取代按策略名称识别。
///
/// `settings` 是发布或导入时冻结的技能参数；为空表示按模板补默认值，
/// 因此模板实例无需再编译沙箱模块。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillBinding {
    /// 内置模板标识，如 `turtle-breakout`；来自技能库时写 `skill`。
    pub template_key: String,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    /// 技能库里的 Skill ID；仅新版发布/导入携带。
    #[serde(default)]
    pub skill_id: Option<Uuid>,
    /// 绑定的 Skill 版本号。
    #[serde(default)]
    pub skill_version: Option<u32>,
    /// 冻结规则的哈希，导入时据此校验交付物未被篡改。
    #[serde(default)]
    pub skill_sha256: Option<String>,
    /// 参数空间：买家可改哪些参数、范围多少。
    #[serde(default)]
    pub params_schema: Option<serde_json::Value>,
    /// 作者在发布时给出的更严格约束。
    #[serde(default)]
    pub param_constraints: Option<serde_json::Value>,
}

/// 策略记录：定义 + 版本 + 校验结果 + 最近回测 + 运行时状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecord {
    pub id: Uuid,
    pub status: StrategyStatus,
    pub source: StrategySource,
    /// 从策略市场导入时的来源发布 ID；退订前据此清理本机导入的策略。
    #[serde(default)]
    pub source_publication_id: Option<Uuid>,
    pub version: u32,
    pub definition: StrategyDefinition,
    pub validation: StrategyValidation,
    pub last_backtest: Option<BacktestReport>,
    pub runtime: StrategyRuntime,
    /// 运行位置：默认自托管，发布到市场后改为平台托管。
    #[serde(default)]
    pub run_scope: StrategyRunScope,
    /// 下单模式：真实下单或模拟下单，默认真实。
    #[serde(default)]
    pub execution_mode: StrategyExecutionMode,
    /// 可选 WASM 沙箱绑定。存在时由沙箱模块替代内置入场/出场判断，
    /// 但输出仍只能通过统一风控网关下单。
    #[serde(default)]
    pub sandbox: Option<crate::sandbox::SandboxBinding>,
    /// 技能参数运行时绑定；存在时由执行器的技能运行时解释参数，不依赖沙箱模块。
    #[serde(default)]
    pub skill: Option<SkillBinding>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl StrategyRecord {
    pub fn new(
        definition: StrategyDefinition,
        source: StrategySource,
        status: StrategyStatus,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            status,
            source,
            source_publication_id: None,
            version: 1,
            definition,
            validation: StrategyValidation::default(),
            last_backtest: None,
            runtime: StrategyRuntime::default(),
            run_scope: StrategyRunScope::SelfHosted,
            execution_mode: StrategyExecutionMode::Real,
            sandbox: None,
            skill: None,
            last_heartbeat_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn name(&self) -> &str {
        &self.definition.name
    }

    /// 更新定义即产生新版本；旧版本的回测结果不再代表新定义。
    pub fn apply_definition(&mut self, definition: StrategyDefinition) {
        self.version += 1;
        self.definition = definition;
        self.validation = StrategyValidation::default();
        self.last_backtest = None;
        self.status = StrategyStatus::Draft;
        self.updated_at = Utc::now();
    }

    pub fn can_run(&self) -> bool {
        !self.validation.has_errors()
    }
}

/// 策略代码语言。当前只接受可在执行器进程内编译的 WebAssembly Text。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyCodeLanguage {
    Wat,
}

/// 代码来源，用于区分 AI 生成与用户手改，便于复现与审计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyCodeOrigin {
    Ai,
    User,
    Template,
}

/// 与某个策略绑定的沙箱模块源码。
///
/// 代码只描述「看到行情后输出什么意图」：没有文件、网络、环境变量或密钥能力，
/// 编译产物也必须通过沙箱 ABI 检查后才能绑定。源码可独立查看、修改、编译与部署。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyCode {
    pub strategy_id: Uuid,
    pub language: StrategyCodeLanguage,
    pub source: String,
    pub origin: StrategyCodeOrigin,
    /// AI 生成时使用的自然语言描述。
    #[serde(default)]
    pub prompt: Option<String>,
    /// AI 或编译器的补充说明。
    #[serde(default)]
    pub notes: Option<String>,
    /// 最近一次编译成功生成的沙箱模块。
    #[serde(default)]
    pub artifact_id: Option<Uuid>,
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    /// 最近一次部署时间；为空表示尚未部署。
    #[serde(default)]
    pub deployed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl StrategyCode {
    pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

    pub fn new(strategy_id: Uuid, source: impl Into<String>, origin: StrategyCodeOrigin) -> Self {
        Self {
            strategy_id,
            language: StrategyCodeLanguage::Wat,
            source: source.into(),
            origin,
            prompt: None,
            notes: None,
            artifact_id: None,
            artifact_sha256: None,
            deployed_at: None,
            updated_at: Utc::now(),
        }
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let error = |path: &str, message: &str| ValidationIssue {
            path: path.to_string(),
            severity: ValidationSeverity::Error,
            message: message.to_string(),
        };

        if self.source.trim().is_empty() {
            issues.push(error("source", "策略代码不能为空"));
        }
        if self.source.len() > Self::MAX_SOURCE_BYTES {
            issues.push(error(
                "source",
                &format!(
                    "策略代码 {} 字节，超过上限 {} 字节",
                    self.source.len(),
                    Self::MAX_SOURCE_BYTES
                ),
            ));
        }
        if !self.source.contains("(module") {
            issues.push(error("source", "代码必须是 WebAssembly Text 模块"));
        }
        issues
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use rust_decimal_macros::dec;

    fn definition() -> StrategyDefinition {
        StrategyDefinition {
            name: "funding_spread".into(),
            kind: StrategyKind::FundingRate,
            universe: StrategyUniverse {
                venues: vec!["binance-spot".into(), "binance".into()],
                symbols: vec!["BTCUSDT".into()],
                instruments: vec![Instrument::Spot, Instrument::Perp],
            },
            entry: EntryRules {
                funding_spread_min: dec!(0.0003),
                net_edge_min: dec!(0.0001),
            },
            exit: ExitRules {
                spread_below: dec!(0.0002),
                max_holding_hours: dec!(24),
            },
            risk: StrategyRisk {
                max_notional_usd: dec!(1000),
                max_unhedged_seconds: 3,
            },
            execution: ExecutionRules {
                leverage: dec!(1),
                max_slippage_bps: dec!(10),
                cooldown_secs: 300,
            },
            data: DataRules::default(),
            costs: CostAssumptions::default(),
            notes: Some("测试策略".into()),
        }
    }

    fn sample(at: DateTime<Utc>, rate: Decimal) -> FundingSample {
        FundingSample {
            symbol: "BTCUSDT".into(),
            at,
            rate,
        }
    }

    #[test]
    fn validation_accepts_default_definition() {
        let validation = definition().validate(&RiskLimits::default());
        assert!(!validation.has_errors(), "{:?}", validation.issues);
    }

    #[test]
    fn pending_orders_dedupe_and_clear() {
        let mut runtime = StrategyRuntime::default();
        let level = |price: Decimal| StrategyPendingOrder {
            symbol: "BTCUSDT".into(),
            venue: Some("binance".into()),
            side: PositionSide::Long,
            price,
            notional_usd: dec!(100),
            created_at: Utc::now(),
        };
        assert!(runtime.upsert_pending(level(dec!(9500))));
        // 同一档位（含微小价差）只更新名义价值，不重复登记。
        assert!(!runtime.upsert_pending(level(dec!(9500.01))));
        assert_eq!(runtime.pending.len(), 1);
        assert!(runtime.upsert_pending(level(dec!(9400))));
        assert_eq!(runtime.pending.len(), 2);
        assert_eq!(runtime.clear_pending("BTCUSDT"), 2);
        assert!(runtime.pending.is_empty());
    }

    #[test]
    fn custom_strategy_allows_single_venue_and_instrument() {
        let mut definition = definition();
        definition.kind = StrategyKind::Custom;
        definition.universe = StrategyUniverse {
            venues: vec!["binance".into()],
            symbols: vec!["BTCUSDT".into()],
            instruments: vec![Instrument::Perp],
        };
        let validation = definition.validate(&RiskLimits::default());
        assert!(!validation.has_errors(), "{:?}", validation.issues);
    }

    #[test]
    fn funding_strategy_rejects_single_venue() {
        let mut definition = definition();
        definition.universe.venues = vec!["binance".into()];
        let validation = definition.validate(&RiskLimits::default());
        assert!(validation.has_errors());
    }

    #[test]
    fn validation_rejects_inverted_thresholds() {
        let mut definition = definition();
        definition.exit.spread_below = dec!(0.001);
        let validation = definition.validate(&RiskLimits::default());
        assert!(validation.has_errors());
        assert!(validation
            .issues
            .iter()
            .any(|issue| issue.path == "exit.spread_below"));
    }

    #[test]
    fn validation_rejects_notional_above_executor_limit() {
        let mut definition = definition();
        definition.risk.max_notional_usd = dec!(100000);
        let validation = definition.validate(&RiskLimits::default());
        assert!(validation.has_errors());
    }

    #[test]
    fn backtest_collects_funding_and_deducts_costs() {
        let start = Utc::now() - Duration::hours(24);
        let samples = vec![
            sample(start, dec!(0.001)),
            sample(start + Duration::hours(8), dec!(0.001)),
            sample(start + Duration::hours(16), dec!(0.0001)),
        ];
        let report = run_backtest(&definition(), &samples, 1);
        assert_eq!(report.trades, 1);
        assert_eq!(report.gross_funding_usd, dec!(2.1));
        // 四条腿手续费 1000 * 4bps * 4 = 1.6，滑点 1000 * 1bp * 2 = 0.2。
        assert_eq!(report.fees_usd, dec!(1.6));
        assert_eq!(report.slippage_usd, dec!(0.2));
        assert_eq!(report.net_pnl_usd, dec!(0.3));
        assert_eq!(report.wins, 1);
        assert_eq!(report.return_pct, dec!(0.03));
    }

    #[test]
    fn backtest_exits_on_max_holding_and_reenters() {
        let mut definition = definition();
        definition.exit.max_holding_hours = dec!(8);
        let start = Utc::now() - Duration::hours(48);
        let samples = vec![
            sample(start, dec!(0.001)),
            sample(start + Duration::hours(8), dec!(0.001)),
            sample(start + Duration::hours(16), dec!(0.001)),
        ];
        let report = run_backtest(&definition, &samples, 1);
        assert_eq!(report.trades, 2);
        assert_eq!(report.settlements, 3);
    }

    #[test]
    fn backtest_without_samples_returns_empty_report() {
        let report = run_backtest(&definition(), &[], 1);
        assert_eq!(report.trades, 0);
        assert_eq!(report.net_pnl_usd, Decimal::ZERO);
        assert!(report.sample_start.is_none());
    }

    #[test]
    fn strategy_code_accepts_wat_module() {
        let code = StrategyCode::new(
            Uuid::new_v4(),
            "(module (memory (export \"memory\") 1))",
            StrategyCodeOrigin::Ai,
        );
        assert!(!code.has_errors(), "{:?}", code.validate());
    }

    #[test]
    fn strategy_code_rejects_empty_and_non_module_source() {
        let empty = StrategyCode::new(Uuid::new_v4(), "   ", StrategyCodeOrigin::User);
        assert!(empty.has_errors());

        let text = StrategyCode::new(Uuid::new_v4(), "let x = 1;", StrategyCodeOrigin::User);
        assert!(text.has_errors());
    }

    #[test]
    fn strategy_code_rejects_oversized_source() {
        let source = format!("(module {})", "a".repeat(StrategyCode::MAX_SOURCE_BYTES));
        let code = StrategyCode::new(Uuid::new_v4(), source, StrategyCodeOrigin::User);
        assert!(code.has_errors());
    }
}
