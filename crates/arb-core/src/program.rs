//! 通用策略程序：策略由「数据计划 + 表达式 + 仓位/风控」组成，而不是固定枚举。
//!
//! 表达式只做确定性的指标与比较运算，不接触网络、文件或交易所密钥；
//! 同一条程序可以在回测、平台重放与实盘执行器里得到一致结果。

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};

use crate::{sandbox::SandboxCandle, types::Side};

/// 当前程序格式版本；结构变化时递增，旧程序由迁移层处理。
pub const PROGRAM_SCHEMA_VERSION: u32 = 1;
/// 单条表达式允许的最大节点数。
pub const MAX_EXPR_NODES: usize = 512;
/// 单条表达式的最大嵌套深度。
pub const MAX_EXPR_DEPTH: usize = 32;
/// 参数数量上限。
pub const MAX_PROGRAM_PARAMS: usize = 32;
/// 指标周期的合法范围。
pub const MIN_PERIOD: u32 = 2;
pub const MAX_PERIOD: u32 = 1_000;

/// 策略可读取的行情序列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Series {
    Open,
    High,
    Low,
    Close,
    Volume,
    /// 当前资金费率；未提供时按 0 处理。
    FundingRate,
    /// 未平仓量；未提供时按 0 处理。
    OpenInterest,
}

impl Default for Series {
    fn default() -> Self {
        Self::Close
    }
}

/// 可组合的指标原语；新增指标只需要扩展这里和求值器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    /// `sma(value, period)`
    Sma,
    /// `ema(value, period)`
    Ema,
    /// `rsi(value, period)`
    Rsi,
    /// `atr(period)`
    Atr,
    /// 唐奇安上轨（不含当前 K 线）：`donchian_high(period)`
    DonchianHigh,
    /// 唐奇安下轨（不含当前 K 线）：`donchian_low(period)`
    DonchianLow,
    /// 区间最高值（含当前 K 线）：`highest(value, period)`
    Highest,
    /// 区间最低值（含当前 K 线）：`lowest(value, period)`
    Lowest,
    /// 区间标准差：`stddev(value, period)`
    StdDev,
    /// 绝对值：`abs(value)`
    Abs,
    /// 较大值：`max(a, b)`
    Max,
    /// 较小值：`min(a, b)`
    Min,
    /// 向前平移：`shift(value, bars)`
    Shift,
}

impl Primitive {
    fn arity(self) -> (usize, usize) {
        match self {
            Self::Sma | Self::Ema | Self::Rsi | Self::Highest | Self::Lowest | Self::StdDev => {
                (2, 2)
            }
            Self::Atr | Self::DonchianHigh | Self::DonchianLow | Self::Abs => (1, 1),
            Self::Shift | Self::Max | Self::Min => (2, 2),
        }
    }

    /// 周期参数在参数列表里的下标；`Shift` 的第二项是平移根数。
    fn period_index(self) -> Option<usize> {
        match self {
            Self::Sma | Self::Ema | Self::Rsi | Self::Highest | Self::Lowest | Self::StdDev => {
                Some(1)
            }
            Self::Atr | Self::DonchianHigh | Self::DonchianLow | Self::Shift => Some(0),
            Self::Abs | Self::Max | Self::Min => None,
        }
    }
}

/// 二元运算；比较类返回 1/0，逻辑类按非零为真。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
    Neq,
    CrossAbove,
    CrossBelow,
    And,
    Or,
}

/// 一元运算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnOp {
    Neg,
    Not,
}

/// 策略表达式。策略是数据，新增策略不需要新增 `SkillKind`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Expr {
    Const {
        value: Decimal,
    },
    Param {
        key: String,
    },
    Series {
        series: Series,
    },
    Call {
        primitive: Primitive,
        #[serde(default)]
        args: Vec<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnOp,
        value: Box<Expr>,
    },
}

impl Expr {
    pub fn node_count(&self) -> usize {
        1 + match self {
            Self::Const { .. } | Self::Param { .. } | Self::Series { .. } => 0,
            Self::Call { args, .. } => args.iter().map(Self::node_count).sum(),
            Self::Binary { left, right, .. } => left.node_count() + right.node_count(),
            Self::Unary { value, .. } => value.node_count(),
        }
    }

    fn depth(&self) -> usize {
        1 + match self {
            Self::Const { .. } | Self::Param { .. } | Self::Series { .. } => 0,
            Self::Call { args, .. } => args.iter().map(Self::depth).max().unwrap_or(0),
            Self::Binary { left, right, .. } => left.depth().max(right.depth()),
            Self::Unary { value, .. } => value.depth(),
        }
    }

    fn collect_params(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Param { key } => {
                out.insert(key.clone());
            }
            Self::Call { args, .. } => {
                for arg in args {
                    arg.collect_params(out);
                }
            }
            Self::Binary { left, right, .. } => {
                left.collect_params(out);
                right.collect_params(out);
            }
            Self::Unary { value, .. } => value.collect_params(out),
            Self::Const { .. } | Self::Series { .. } => {}
        }
    }
}

/// 一个可配置参数；`value` 是作者默认值，买家在参数空间内覆盖。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramParam {
    pub label: String,
    /// Decimal 的 serde 形态是字符串，保证哈希稳定。
    pub value: Decimal,
    pub min: Option<Decimal>,
    pub max: Option<Decimal>,
    pub step: Option<Decimal>,
    #[serde(default = "default_true")]
    pub editable: bool,
}

impl Default for ProgramParam {
    fn default() -> Self {
        Self {
            label: String::new(),
            value: Decimal::ZERO,
            min: None,
            max: None,
            step: None,
            editable: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// 数据计划：程序只声明需要哪段行情，执行器负责拉取。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramData {
    pub timeframe: String,
    pub lookback: u32,
}

impl Default for ProgramData {
    fn default() -> Self {
        Self {
            timeframe: "1h".to_string(),
            lookback: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramDirection {
    #[default]
    Both,
    Long,
    Short,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramSizing {
    pub notional_usd: Decimal,
    /// 单笔风险占权益百分比；大于 0 时优先按风险定仓。
    pub risk_pct: Decimal,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramRisk {
    pub stop_loss_pct: Decimal,
    pub take_profit_pct: Decimal,
    pub trailing_stop_pct: Decimal,
    pub max_hold_hours: Decimal,
    pub cooldown_hours: Decimal,
}

/// 一条完整策略程序。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StrategyProgram {
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    pub data: ProgramData,
    pub params: BTreeMap<String, ProgramParam>,
    pub entry_long: Option<Expr>,
    pub entry_short: Option<Expr>,
    pub exit: Option<Expr>,
    pub filters: Vec<Expr>,
    pub direction: ProgramDirection,
    pub sizing: ProgramSizing,
    pub risk: ProgramRisk,
}

impl Default for StrategyProgram {
    fn default() -> Self {
        Self {
            schema_version: PROGRAM_SCHEMA_VERSION,
            name: String::new(),
            description: String::new(),
            data: ProgramData::default(),
            params: BTreeMap::new(),
            entry_long: None,
            entry_short: None,
            exit: None,
            filters: Vec::new(),
            direction: ProgramDirection::Both,
            sizing: ProgramSizing::default(),
            risk: ProgramRisk::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProgramIssue {
    pub path: String,
    pub message: String,
}

impl ProgramIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl StrategyProgram {
    /// 结构校验：不依赖行情，作者保存/发布时就能发现问题。
    pub fn validate(&self) -> Vec<ProgramIssue> {
        let mut issues = Vec::new();
        if self.schema_version != PROGRAM_SCHEMA_VERSION {
            issues.push(ProgramIssue::new(
                "schema_version",
                format!(
                    "程序版本 {} 不受支持，当前只接受 {}",
                    self.schema_version, PROGRAM_SCHEMA_VERSION
                ),
            ));
        }
        if self.data.timeframe.trim().is_empty() {
            issues.push(ProgramIssue::new("data.timeframe", "K 线周期不能为空"));
        }
        if !(3..=MAX_PERIOD).contains(&self.data.lookback) {
            issues.push(ProgramIssue::new(
                "data.lookback",
                format!("回看根数必须在 3..={MAX_PERIOD} 之间"),
            ));
        }
        match self.direction {
            ProgramDirection::Long if self.entry_long.is_none() => issues.push(ProgramIssue::new(
                "entry_long",
                "方向为做多时必须提供做多入场表达式",
            )),
            ProgramDirection::Short if self.entry_short.is_none() => issues.push(
                ProgramIssue::new("entry_short", "方向为做空时必须提供做空入场表达式"),
            ),
            ProgramDirection::Both if self.entry_long.is_none() && self.entry_short.is_none() => {
                issues.push(ProgramIssue::new("entry", "至少提供一条入场表达式"))
            }
            _ => {}
        }

        if self.params.len() > MAX_PROGRAM_PARAMS {
            issues.push(ProgramIssue::new(
                "params",
                format!("参数数量不能超过 {MAX_PROGRAM_PARAMS} 个"),
            ));
        }
        for (key, param) in &self.params {
            if key.trim().is_empty() {
                issues.push(ProgramIssue::new("params", "参数名不能为空"));
            }
            if !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                issues.push(ProgramIssue::new(
                    format!("params.{key}"),
                    "参数名只能包含字母、数字和下划线",
                ));
            }
            if let (Some(min), Some(max)) = (param.min, param.max) {
                if min > max {
                    issues.push(ProgramIssue::new(
                        format!("params.{key}"),
                        "参数下界不能大于上界",
                    ));
                }
            }
            if let Some(min) = param.min {
                if param.value < min {
                    issues.push(ProgramIssue::new(
                        format!("params.{key}.value"),
                        "默认值小于下界",
                    ));
                }
            }
            if let Some(max) = param.max {
                if param.value > max {
                    issues.push(ProgramIssue::new(
                        format!("params.{key}.value"),
                        "默认值大于上界",
                    ));
                }
            }
            if let Some(step) = param.step {
                if step <= Decimal::ZERO {
                    issues.push(ProgramIssue::new(
                        format!("params.{key}.step"),
                        "步长必须大于 0",
                    ));
                }
            }
        }

        if let Some(entry) = &self.entry_long {
            self.validate_expr("entry_long", entry, &mut issues);
        }
        if let Some(entry) = &self.entry_short {
            self.validate_expr("entry_short", entry, &mut issues);
        }
        if let Some(exit) = &self.exit {
            self.validate_expr("exit", exit, &mut issues);
        }
        for (index, filter) in self.filters.iter().enumerate() {
            self.validate_expr(&format!("filters.{index}"), filter, &mut issues);
        }

        issues
    }

    fn validate_expr(&self, path: &str, expr: &Expr, issues: &mut Vec<ProgramIssue>) {
        if expr.node_count() > MAX_EXPR_NODES {
            issues.push(ProgramIssue::new(
                path,
                format!("表达式节点数超过上限 {MAX_EXPR_NODES}"),
            ));
        }
        if expr.depth() > MAX_EXPR_DEPTH {
            issues.push(ProgramIssue::new(
                path,
                format!("表达式嵌套深度超过上限 {MAX_EXPR_DEPTH}"),
            ));
        }
        let mut params = BTreeSet::new();
        expr.collect_params(&mut params);
        for key in params {
            if !self.params.contains_key(&key) {
                issues.push(ProgramIssue::new(
                    path,
                    format!("引用了未定义的参数 `{key}`"),
                ));
            }
        }
        self.validate_primitive(path, expr, issues);
    }

    fn validate_primitive(&self, path: &str, expr: &Expr, issues: &mut Vec<ProgramIssue>) {
        match expr {
            Expr::Call { primitive, args } => {
                let (min, max) = primitive.arity();
                if args.len() < min || args.len() > max {
                    issues.push(ProgramIssue::new(
                        path,
                        format!(
                            "{:?} 需要 {}..={} 个参数，实际 {} 个",
                            primitive,
                            min,
                            max,
                            args.len()
                        ),
                    ));
                }
                if let Some(index) = primitive.period_index() {
                    if let Some(value) = args.get(index).and_then(|arg| self.static_value(arg)) {
                        let valid = if matches!(primitive, Primitive::Shift) {
                            value.to_u32().is_some_and(|bars| bars <= MAX_PERIOD)
                        } else {
                            value
                                .to_u32()
                                .is_some_and(|period| (MIN_PERIOD..=MAX_PERIOD).contains(&period))
                        };
                        if !valid {
                            issues.push(ProgramIssue::new(
                                path,
                                if matches!(primitive, Primitive::Shift) {
                                    format!("平移根数必须在 0..={MAX_PERIOD} 之间")
                                } else {
                                    format!("周期必须在 {MIN_PERIOD}..={MAX_PERIOD} 之间")
                                },
                            ));
                        }
                    }
                }
                for arg in args {
                    self.validate_primitive(path, arg, issues);
                }
            }
            Expr::Binary { left, right, .. } => {
                self.validate_primitive(path, left, issues);
                self.validate_primitive(path, right, issues);
            }
            Expr::Unary { value, .. } => self.validate_primitive(path, value, issues),
            Expr::Const { .. } | Expr::Param { .. } | Expr::Series { .. } => {}
        }
    }

    fn static_value(&self, expr: &Expr) -> Option<Decimal> {
        match expr {
            Expr::Const { value } => Some(*value),
            Expr::Param { key } => self.params.get(key).map(|param| param.value),
            _ => None,
        }
    }

    /// 补齐参数默认值并夹回上下界；加载旧程序时调用。
    pub fn normalize(&mut self) {
        self.schema_version = PROGRAM_SCHEMA_VERSION;
        self.data.lookback = self.data.lookback.clamp(3, MAX_PERIOD);
        for param in self.params.values_mut() {
            if let Some(min) = param.min {
                if param.value < min {
                    param.value = min;
                }
            }
            if let Some(max) = param.max {
                if param.value > max {
                    param.value = max;
                }
            }
        }
        self.risk.stop_loss_pct = self.risk.stop_loss_pct.max(Decimal::ZERO);
        self.risk.take_profit_pct = self.risk.take_profit_pct.max(Decimal::ZERO);
        self.risk.trailing_stop_pct = self.risk.trailing_stop_pct.max(Decimal::ZERO);
        self.risk.max_hold_hours = self.risk.max_hold_hours.max(Decimal::ZERO);
        self.risk.cooldown_hours = self.risk.cooldown_hours.max(Decimal::ZERO);
        self.sizing.notional_usd = self.sizing.notional_usd.max(Decimal::ZERO);
        self.sizing.risk_pct = self
            .sizing
            .risk_pct
            .clamp(Decimal::ZERO, Decimal::from(100u32));
    }

    /// 生成一个可直接运行的均线交叉示例，作为「通用表达式」模板的默认值。
    pub fn example_ma_cross() -> Self {
        let mut params = BTreeMap::new();
        params.insert(
            "fast".to_string(),
            ProgramParam {
                label: "快均线".to_string(),
                value: Decimal::from(20u32),
                min: Some(Decimal::from(2u32)),
                max: Some(Decimal::from(200u32)),
                step: Some(Decimal::ONE),
                editable: true,
            },
        );
        params.insert(
            "slow".to_string(),
            ProgramParam {
                label: "慢均线".to_string(),
                value: Decimal::from(60u32),
                min: Some(Decimal::from(3u32)),
                max: Some(Decimal::from(500u32)),
                step: Some(Decimal::ONE),
                editable: true,
            },
        );
        params.insert(
            "notional_usd".to_string(),
            ProgramParam {
                label: "单笔金额（USD）".to_string(),
                value: Decimal::from(100u32),
                min: Some(Decimal::from(10u32)),
                max: Some(Decimal::from(100_000u32)),
                step: Some(Decimal::from(10u32)),
                editable: true,
            },
        );
        let fast = || Expr::Call {
            primitive: Primitive::Sma,
            args: vec![
                Expr::Series {
                    series: Series::Close,
                },
                Expr::Param {
                    key: "fast".to_string(),
                },
            ],
        };
        let slow = || Expr::Call {
            primitive: Primitive::Sma,
            args: vec![
                Expr::Series {
                    series: Series::Close,
                },
                Expr::Param {
                    key: "slow".to_string(),
                },
            ],
        };
        Self {
            name: "通用均线交叉".to_string(),
            description: "快线上穿慢线做多、下穿做空；止损与最长持有由程序风控控制。".to_string(),
            params,
            entry_long: Some(Expr::Binary {
                op: BinOp::CrossAbove,
                left: Box::new(fast()),
                right: Box::new(slow()),
            }),
            entry_short: Some(Expr::Binary {
                op: BinOp::CrossBelow,
                left: Box::new(fast()),
                right: Box::new(slow()),
            }),
            exit: Some(Expr::Binary {
                op: BinOp::Or,
                left: Box::new(Expr::Binary {
                    op: BinOp::CrossBelow,
                    left: Box::new(fast()),
                    right: Box::new(slow()),
                }),
                right: Box::new(Expr::Binary {
                    op: BinOp::CrossAbove,
                    left: Box::new(fast()),
                    right: Box::new(slow()),
                }),
            }),
            direction: ProgramDirection::Both,
            sizing: ProgramSizing {
                notional_usd: Decimal::from(100u32),
                risk_pct: Decimal::ZERO,
            },
            risk: ProgramRisk {
                stop_loss_pct: Decimal::from(2u32),
                take_profit_pct: Decimal::from(6u32),
                trailing_stop_pct: Decimal::ZERO,
                max_hold_hours: Decimal::from(72u32),
                cooldown_hours: Decimal::from(1u32),
            },
            ..Self::default()
        }
    }
}

/// 程序看到的一笔持仓；由执行器根据运行记录构造。
#[derive(Debug, Clone)]
pub struct ProgramPosition {
    pub side: Side,
    pub entry_price: Decimal,
    pub opened_at: DateTime<Utc>,
    pub highest_since_entry: Decimal,
    pub lowest_since_entry: Decimal,
}

/// 一次求值的上下文。
pub struct ProgramContext<'a> {
    pub candles: &'a [SandboxCandle],
    pub funding_rate: Option<Decimal>,
    pub position: Option<ProgramPosition>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramAction {
    Hold,
    Open(Side),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramDecision {
    pub action: ProgramAction,
    pub reason: String,
}

impl ProgramDecision {
    fn hold(reason: impl Into<String>) -> Self {
        Self {
            action: ProgramAction::Hold,
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Copy)]
struct EvalCtx<'a> {
    candles: &'a [SandboxCandle],
    params: &'a BTreeMap<String, ProgramParam>,
    funding_rate: Option<Decimal>,
    index: usize,
}

/// 对最新一根 K 线求值。
pub fn evaluate(program: &StrategyProgram, ctx: &ProgramContext) -> ProgramDecision {
    let Some(last_index) = ctx.candles.len().checked_sub(1) else {
        return ProgramDecision::hold("没有行情数据");
    };
    if ctx.candles.len() < 3 {
        return ProgramDecision::hold("行情数据不足");
    }
    let eval_ctx = EvalCtx {
        candles: ctx.candles,
        params: &program.params,
        funding_rate: ctx.funding_rate,
        index: last_index,
    };
    let last = ctx.candles[last_index].close;
    if last <= Decimal::ZERO {
        return ProgramDecision::hold("最新价无效");
    }

    if let Some(position) = &ctx.position {
        if let Some(reason) = risk_exit(program, position, last, ctx.now) {
            return ProgramDecision {
                action: ProgramAction::Close,
                reason,
            };
        }
        if let Some(exit) = &program.exit {
            if truthy(eval(exit, &eval_ctx)) {
                return ProgramDecision {
                    action: ProgramAction::Close,
                    reason: "离场表达式成立".to_string(),
                };
            }
        }
        return ProgramDecision::hold("持仓中，未触发离场");
    }

    for (index, filter) in program.filters.iter().enumerate() {
        if !truthy(eval(filter, &eval_ctx)) {
            return ProgramDecision::hold(format!("过滤条件 {} 未通过", index + 1));
        }
    }

    let long = program
        .entry_long
        .as_ref()
        .filter(|_| program.direction != ProgramDirection::Short)
        .is_some_and(|entry| truthy(eval(entry, &eval_ctx)));
    let short = program
        .entry_short
        .as_ref()
        .filter(|_| program.direction != ProgramDirection::Long)
        .is_some_and(|entry| truthy(eval(entry, &eval_ctx)));

    match (long, short) {
        (true, false) => ProgramDecision {
            action: ProgramAction::Open(Side::Buy),
            reason: "做多入场表达式成立".to_string(),
        },
        (false, true) => ProgramDecision {
            action: ProgramAction::Open(Side::Sell),
            reason: "做空入场表达式成立".to_string(),
        },
        (true, true) => ProgramDecision::hold("多空入场同时成立，按冲突处理"),
        (false, false) => ProgramDecision::hold("入场条件未满足"),
    }
}

fn truthy(value: Option<Decimal>) -> bool {
    value.is_some_and(|value| value != Decimal::ZERO)
}

fn risk_exit(
    program: &StrategyProgram,
    position: &ProgramPosition,
    last: Decimal,
    now: DateTime<Utc>,
) -> Option<String> {
    if position.entry_price <= Decimal::ZERO {
        return None;
    }
    let pnl_pct = match position.side {
        Side::Buy => (last - position.entry_price) / position.entry_price * Decimal::from(100u32),
        Side::Sell => (position.entry_price - last) / position.entry_price * Decimal::from(100u32),
    };
    if program.risk.stop_loss_pct > Decimal::ZERO && pnl_pct <= -program.risk.stop_loss_pct {
        return Some(format!("触发止损（{pnl_pct:.2}%）"));
    }
    if program.risk.take_profit_pct > Decimal::ZERO && pnl_pct >= program.risk.take_profit_pct {
        return Some(format!("触发止盈（{pnl_pct:.2}%）"));
    }
    if program.risk.trailing_stop_pct > Decimal::ZERO {
        match position.side {
            Side::Buy if position.highest_since_entry > Decimal::ZERO => {
                let drawdown = (position.highest_since_entry - last) / position.highest_since_entry
                    * Decimal::from(100u32);
                if drawdown >= program.risk.trailing_stop_pct {
                    return Some(format!("触发移动止损（回撤 {drawdown:.2}%）"));
                }
            }
            Side::Sell if position.lowest_since_entry > Decimal::ZERO => {
                let rebound = (last - position.lowest_since_entry) / position.lowest_since_entry
                    * Decimal::from(100u32);
                if rebound >= program.risk.trailing_stop_pct {
                    return Some(format!("触发移动止损（反弹 {rebound:.2}%）"));
                }
            }
            _ => {}
        }
    }
    if program.risk.max_hold_hours > Decimal::ZERO {
        let held = now.signed_duration_since(position.opened_at);
        let minutes = (program.risk.max_hold_hours * Decimal::from(60u32))
            .to_i64()
            .unwrap_or(i64::MAX);
        if held.num_minutes() >= minutes {
            return Some("超过最长持有时间".to_string());
        }
    }
    None
}

/// 在指定下标求值；数据不足、除零或周期非法时返回 `None`。
fn eval(expr: &Expr, ctx: &EvalCtx) -> Option<Decimal> {
    match expr {
        Expr::Const { value } => Some(*value),
        Expr::Param { key } => ctx.params.get(key).map(|param| param.value),
        Expr::Series { series } => series_value(*series, ctx),
        Expr::Unary { op, value } => {
            let value = eval(value, ctx)?;
            Some(match op {
                UnOp::Neg => -value,
                UnOp::Not => bool_value(!truthy(Some(value))),
            })
        }
        Expr::Binary { op, left, right } => eval_binary(*op, left, right, ctx),
        Expr::Call { primitive, args } => eval_call(*primitive, args, ctx),
    }
}

fn eval_binary(op: BinOp, left: &Expr, right: &Expr, ctx: &EvalCtx) -> Option<Decimal> {
    if matches!(op, BinOp::And | BinOp::Or) {
        let left_value = truthy(eval(left, ctx));
        return match op {
            BinOp::And => {
                if !left_value {
                    Some(Decimal::ZERO)
                } else {
                    Some(bool_value(truthy(eval(right, ctx))))
                }
            }
            BinOp::Or => {
                if left_value {
                    Some(Decimal::ONE)
                } else {
                    Some(bool_value(truthy(eval(right, ctx))))
                }
            }
            _ => unreachable!(),
        };
    }
    if matches!(op, BinOp::CrossAbove | BinOp::CrossBelow) {
        let previous = ctx.index.checked_sub(1)?;
        let left_now = eval(left, ctx)?;
        let right_now = eval(right, ctx)?;
        let previous_ctx = EvalCtx {
            index: previous,
            ..*ctx
        };
        let left_prev = eval(left, &previous_ctx)?;
        let right_prev = eval(right, &previous_ctx)?;
        let crossed = match op {
            BinOp::CrossAbove => left_prev <= right_prev && left_now > right_now,
            BinOp::CrossBelow => left_prev >= right_prev && left_now < right_now,
            _ => false,
        };
        return Some(bool_value(crossed));
    }
    let left = eval(left, ctx)?;
    let right = eval(right, ctx)?;
    Some(match op {
        BinOp::Add => left + right,
        BinOp::Sub => left - right,
        BinOp::Mul => left * right,
        BinOp::Div => {
            if right == Decimal::ZERO {
                return None;
            }
            left / right
        }
        BinOp::Gt => bool_value(left > right),
        BinOp::Gte => bool_value(left >= right),
        BinOp::Lt => bool_value(left < right),
        BinOp::Lte => bool_value(left <= right),
        BinOp::Eq => bool_value(left == right),
        BinOp::Neq => bool_value(left != right),
        BinOp::CrossAbove | BinOp::CrossBelow | BinOp::And | BinOp::Or => unreachable!(),
    })
}

fn eval_call(primitive: Primitive, args: &[Expr], ctx: &EvalCtx) -> Option<Decimal> {
    match primitive {
        Primitive::Sma => {
            let period = period_arg(args.get(1), ctx)?;
            rolling_values(args.first()?, ctx, period)
                .map(|values| values.iter().sum::<Decimal>() / Decimal::from(period))
        }
        Primitive::Ema => {
            let period = period_arg(args.get(1), ctx)?;
            let values = rolling_values(args.first()?, ctx, period)?;
            Some(ema(&values, period))
        }
        Primitive::Rsi => {
            let period = period_arg(args.get(1), ctx)?;
            let values = rolling_values(args.first()?, ctx, period + 1)?;
            rsi(&values, period)
        }
        Primitive::Atr => {
            let period = period_arg(args.first(), ctx)?;
            atr(ctx.candles, ctx.index, period)
        }
        Primitive::DonchianHigh => {
            let period = period_arg(args.first(), ctx)?;
            donchian(ctx.candles, ctx.index, period).map(|(high, _)| high)
        }
        Primitive::DonchianLow => {
            let period = period_arg(args.first(), ctx)?;
            donchian(ctx.candles, ctx.index, period).map(|(_, low)| low)
        }
        Primitive::Highest => {
            let period = period_arg(args.get(1), ctx)?;
            rolling_values(args.first()?, ctx, period)?
                .into_iter()
                .max()
        }
        Primitive::Lowest => {
            let period = period_arg(args.get(1), ctx)?;
            rolling_values(args.first()?, ctx, period)?
                .into_iter()
                .min()
        }
        Primitive::StdDev => {
            let period = period_arg(args.get(1), ctx)?;
            stddev(&rolling_values(args.first()?, ctx, period)?)
        }
        Primitive::Abs => Some(eval(args.first()?, ctx)?.abs()),
        Primitive::Max => Some(eval(args.first()?, ctx)?.max(eval(args.get(1)?, ctx)?)),
        Primitive::Min => Some(eval(args.first()?, ctx)?.min(eval(args.get(1)?, ctx)?)),
        Primitive::Shift => {
            let bars = bars_arg(args.get(1), ctx)? as usize;
            let shifted = EvalCtx {
                index: ctx.index.checked_sub(bars)?,
                ..*ctx
            };
            eval(args.first()?, &shifted)
        }
    }
}

fn period_arg(expr: Option<&Expr>, ctx: &EvalCtx) -> Option<u32> {
    let period = eval(expr?, ctx)?.to_u32()?;
    (MIN_PERIOD..=MAX_PERIOD)
        .contains(&period)
        .then_some(period)
}

fn bars_arg(expr: Option<&Expr>, ctx: &EvalCtx) -> Option<u32> {
    let bars = eval(expr?, ctx)?.to_u32()?;
    (bars <= MAX_PERIOD).then_some(bars)
}

fn rolling_values(expr: &Expr, ctx: &EvalCtx, period: u32) -> Option<Vec<Decimal>> {
    let period = period as usize;
    let start = ctx.index.checked_add(1)?.checked_sub(period)?;
    let mut values = Vec::with_capacity(period);
    for index in start..=ctx.index {
        let window_ctx = EvalCtx { index, ..*ctx };
        values.push(eval(expr, &window_ctx)?);
    }
    Some(values)
}

fn series_value(series: Series, ctx: &EvalCtx) -> Option<Decimal> {
    let candle = ctx.candles.get(ctx.index)?;
    Some(match series {
        Series::Open => candle.open,
        Series::High => candle.high,
        Series::Low => candle.low,
        Series::Close => candle.close,
        Series::Volume => candle.volume,
        Series::FundingRate => ctx.funding_rate.unwrap_or(Decimal::ZERO),
        Series::OpenInterest => Decimal::ZERO,
    })
}

fn ema(values: &[Decimal], period: u32) -> Decimal {
    let period = period as usize;
    let divisor = Decimal::from(period);
    let mut value = values[..period].iter().sum::<Decimal>() / divisor;
    let weight = Decimal::from(2u32) / (divisor + Decimal::ONE);
    for close in &values[period..] {
        value = (*close - value) * weight + value;
    }
    value
}

fn rsi(values: &[Decimal], period: u32) -> Option<Decimal> {
    let period = period as usize;
    if values.len() <= period {
        return None;
    }
    let mut gains = Decimal::ZERO;
    let mut losses = Decimal::ZERO;
    for pair in values[values.len() - period - 1..].windows(2) {
        let change = pair[1] - pair[0];
        if change >= Decimal::ZERO {
            gains += change;
        } else {
            losses -= change;
        }
    }
    if losses == Decimal::ZERO {
        return Some(Decimal::from(100u32));
    }
    let rs = gains / losses;
    Some(Decimal::from(100u32) - Decimal::from(100u32) / (Decimal::ONE + rs))
}

fn atr(candles: &[SandboxCandle], index: usize, period: u32) -> Option<Decimal> {
    let period = period as usize;
    if index < period {
        return None;
    }
    let start = index + 1 - period;
    let mut sum = Decimal::ZERO;
    for cursor in start..=index {
        let previous_close = if cursor == 0 {
            candles[0].close
        } else {
            candles[cursor - 1].close
        };
        let candle = &candles[cursor];
        let high_low = candle.high - candle.low;
        let high_close = (candle.high - previous_close).abs();
        let low_close = (candle.low - previous_close).abs();
        sum += high_low.max(high_close).max(low_close);
    }
    Some(sum / Decimal::from(period))
}

fn donchian(candles: &[SandboxCandle], index: usize, period: u32) -> Option<(Decimal, Decimal)> {
    let period = period as usize;
    let end = index.checked_sub(1)?;
    let start = end.checked_add(1)?.checked_sub(period)?;
    let window = candles.get(start..=end)?;
    let high = window.iter().map(|candle| candle.high).max()?;
    let low = window.iter().map(|candle| candle.low).min()?;
    Some((high, low))
}

fn stddev(values: &[Decimal]) -> Option<Decimal> {
    if values.is_empty() {
        return None;
    }
    let count = Decimal::from(values.len() as u64);
    let mean = values.iter().sum::<Decimal>() / count;
    let variance = values
        .iter()
        .map(|value| {
            let diff = *value - mean;
            diff * diff
        })
        .sum::<Decimal>()
        / count;
    Decimal::from_f64_retain(variance.to_f64()?.sqrt())
}

fn bool_value(value: bool) -> Decimal {
    if value {
        Decimal::ONE
    } else {
        Decimal::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    fn candle(index: i64, close: Decimal) -> SandboxCandle {
        SandboxCandle {
            open_time: Utc.timestamp_opt(index * 60, 0).single().expect("合法时间"),
            open: close,
            high: close + Decimal::ONE,
            low: close - Decimal::ONE,
            close,
            volume: dec!(1),
        }
    }

    fn candles(closes: &[Decimal]) -> Vec<SandboxCandle> {
        closes
            .iter()
            .enumerate()
            .map(|(index, close)| candle(index as i64, *close))
            .collect()
    }

    fn decision(program: &StrategyProgram, series: &[SandboxCandle]) -> ProgramDecision {
        evaluate(
            program,
            &ProgramContext {
                candles: series,
                funding_rate: None,
                position: None,
                now: Utc::now(),
            },
        )
    }

    #[test]
    fn example_program_is_valid() {
        let program = StrategyProgram::example_ma_cross();
        assert!(program.validate().is_empty(), "{:?}", program.validate());
    }

    #[test]
    fn missing_param_is_reported() {
        let mut program = StrategyProgram::example_ma_cross();
        program.params.remove("fast");
        let issues = program.validate();
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("未定义的参数 `fast`")),
            "{issues:?}"
        );
    }

    #[test]
    fn invalid_period_is_reported() {
        let mut program = StrategyProgram::example_ma_cross();
        program.params.get_mut("fast").expect("参数存在").value = Decimal::ONE;
        let issues = program.validate();
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("周期必须在")),
            "{issues:?}"
        );
    }

    #[test]
    fn sma_cross_opens_long() {
        let closes = [
            dec!(10),
            dec!(10),
            dec!(10),
            dec!(9),
            dec!(9),
            dec!(9),
            dec!(11),
        ];
        let mut program = StrategyProgram::default();
        program.params.insert(
            "fast".to_string(),
            ProgramParam {
                value: dec!(2),
                ..ProgramParam::default()
            },
        );
        program.params.insert(
            "slow".to_string(),
            ProgramParam {
                value: dec!(4),
                ..ProgramParam::default()
            },
        );
        let fast = || Expr::Call {
            primitive: Primitive::Sma,
            args: vec![
                Expr::Series {
                    series: Series::Close,
                },
                Expr::Param {
                    key: "fast".to_string(),
                },
            ],
        };
        let slow = || Expr::Call {
            primitive: Primitive::Sma,
            args: vec![
                Expr::Series {
                    series: Series::Close,
                },
                Expr::Param {
                    key: "slow".to_string(),
                },
            ],
        };
        program.entry_long = Some(Expr::Binary {
            op: BinOp::CrossAbove,
            left: Box::new(fast()),
            right: Box::new(slow()),
        });
        let result = decision(&program, &candles(&closes));
        assert_eq!(result.action, ProgramAction::Open(Side::Buy), "{result:?}");
    }

    #[test]
    fn stop_loss_closes_position() {
        let program = StrategyProgram {
            risk: ProgramRisk {
                stop_loss_pct: dec!(3),
                ..ProgramRisk::default()
            },
            ..StrategyProgram::default()
        };
        let position = ProgramPosition {
            side: Side::Buy,
            entry_price: dec!(100),
            opened_at: Utc::now(),
            highest_since_entry: dec!(101),
            lowest_since_entry: dec!(95),
        };
        let result = evaluate(
            &program,
            &ProgramContext {
                candles: &candles(&[dec!(100), dec!(100), dec!(95)]),
                funding_rate: None,
                position: Some(position),
                now: Utc::now(),
            },
        );
        assert_eq!(result.action, ProgramAction::Close, "{result:?}");
    }

    #[test]
    fn max_hold_closes_position() {
        let program = StrategyProgram {
            risk: ProgramRisk {
                max_hold_hours: dec!(1),
                ..ProgramRisk::default()
            },
            ..StrategyProgram::default()
        };
        let position = ProgramPosition {
            side: Side::Buy,
            entry_price: dec!(100),
            opened_at: Utc::now() - chrono::Duration::hours(2),
            highest_since_entry: dec!(101),
            lowest_since_entry: dec!(99),
        };
        let result = evaluate(
            &program,
            &ProgramContext {
                candles: &candles(&[dec!(100), dec!(100), dec!(100)]),
                funding_rate: None,
                position: Some(position),
                now: Utc::now(),
            },
        );
        assert_eq!(result.action, ProgramAction::Close, "{result:?}");
    }

    #[test]
    fn params_are_clamped() {
        let mut program = StrategyProgram::example_ma_cross();
        let param = program.params.get_mut("fast").expect("参数存在");
        param.value = dec!(9999);
        param.max = Some(dec!(200));
        program.normalize();
        assert_eq!(program.params["fast"].value, dec!(200));
    }
}
