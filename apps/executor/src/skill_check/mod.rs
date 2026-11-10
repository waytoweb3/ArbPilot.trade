//! SKILL.md 六维校验器。
//!
//! 交易技能的 SKILL.md 在执行器里是六章参数结构（见 [`crate::skill::SkillRule`]）。
//! 本模块按用户要求拆成六个可独立运行的校验维度：
//!
//! 1. [`structure`]：YAML 语法、必填字段、数据类型、字段命名、枚举值、默认值、版本兼容；
//! 2. [`rules`]：业务规则完整性，以及入场/出场/加仓/止损/冷却/持仓之间的互相矛盾；
//! 3. [`math`]：用独立实现复核指标、信号、网格档位、ATR 止损、盈亏与仓位计算；
//! 4. [`backtest`]：用历史 K 线回放，产出收益、最大回撤、胜率、交易频率、手续费与滑点；
//! 5. [`agent`]：用事件与 K 线模拟 Agent 的触发识别、技能选择与执行步骤；
//! 6. [`safety`]：异常输入（重复信号、超限、断网、资金不足、部分成交）下的交易安全。
//!
//! 校验器只做判断，不下单；它复刻运行期规则是为了发现「实现与规则不一致」，
//! 因此不能反过来被运行期实现复用。
#![allow(dead_code)]

mod agent;
mod backtest;
mod math;
mod rules;
mod safety;
pub mod structure;

use arb_core::{RiskLimits, SandboxCandle};
use serde::Serialize;

use crate::skill::SkillRule;

pub use backtest::Costs;

/// 校验维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Structure,
    Rules,
    Math,
    Backtest,
    Agent,
    Safety,
}

impl Dimension {
    pub fn label(self) -> &'static str {
        match self {
            Self::Structure => "格式正确",
            Self::Rules => "规则完整",
            Self::Math => "策略计算正确",
            Self::Backtest => "回测结果可信",
            Self::Agent => "Agent 按预期调用",
            Self::Safety => "交易安全",
        }
    }
}

/// 问题严重程度；只有 `Error` 判定该维度不通过。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// 一条校验结论。
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub dimension: Dimension,
    /// 稳定的机器可读编码，便于 CI 断言与忽略清单。
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
}

impl Issue {
    pub fn info(dimension: Dimension, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            dimension,
            code,
            severity: Severity::Info,
            message: message.into(),
        }
    }

    pub fn warning(dimension: Dimension, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            dimension,
            code,
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    pub fn error(dimension: Dimension, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            dimension,
            code,
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

/// 六个维度的校验结果。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub issues: Vec<Issue>,
}

impl Report {
    pub fn push(&mut self, issue: Issue) {
        self.issues.push(issue);
    }

    pub fn extend(&mut self, issues: impl IntoIterator<Item = Issue>) {
        self.issues.extend(issues);
    }

    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    fn count(&self, severity: Severity) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == severity)
            .count()
    }

    pub fn has_errors(&self) -> bool {
        self.errors() > 0
    }

    pub fn issues_in(&self, dimension: Dimension) -> impl Iterator<Item = &Issue> {
        self.issues
            .iter()
            .filter(move |issue| issue.dimension == dimension)
    }

    /// 该维度是否通过：没有 `Error` 即通过，`Warning` 不阻断。
    pub fn passed(&self, dimension: Dimension) -> bool {
        !self
            .issues
            .iter()
            .any(|issue| issue.dimension == dimension && issue.severity == Severity::Error)
    }

    /// 六个维度的一句话结论；顺序固定，便于比对。
    pub fn summary(&self) -> String {
        [
            Dimension::Structure,
            Dimension::Rules,
            Dimension::Math,
            Dimension::Backtest,
            Dimension::Agent,
            Dimension::Safety,
        ]
        .into_iter()
        .map(|dimension| {
            let status = if self.passed(dimension) {
                "通过"
            } else {
                "不通过"
            };
            format!("{}：{status}", dimension.label())
        })
        .collect::<Vec<_>>()
        .join("\n")
    }
}

/// 单个维度的结论，供接口直接返回。
#[derive(Debug, Clone, Serialize)]
pub struct DimensionReport {
    pub dimension: Dimension,
    pub label: &'static str,
    pub passed: bool,
    pub errors: usize,
    pub warnings: usize,
    pub issues: Vec<Issue>,
}

/// 六维报告的对外形态：固定六个维度、顺序稳定，便于前端逐行展示与断言。
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub passed: bool,
    pub errors: usize,
    pub warnings: usize,
    /// 六行「维度：通过/不通过」结论。
    pub summary: String,
    pub dimensions: Vec<DimensionReport>,
}

impl ValidationReport {
    /// 阻断原因：逐条列出错误，供接口 message 直接展示。
    pub fn blockers(&self) -> impl Iterator<Item = &Issue> {
        self.dimensions
            .iter()
            .flat_map(|dimension| dimension.issues.iter())
            .filter(|issue| issue.severity == Severity::Error)
    }
}

impl Report {
    /// 转成固定六维的对外报告；没有 `Error` 即整体通过。
    pub fn view(&self) -> ValidationReport {
        let dimensions = [
            Dimension::Structure,
            Dimension::Rules,
            Dimension::Math,
            Dimension::Backtest,
            Dimension::Agent,
            Dimension::Safety,
        ]
        .into_iter()
        .map(|dimension| {
            let issues: Vec<Issue> = self.issues_in(dimension).cloned().collect();
            DimensionReport {
                dimension,
                label: dimension.label(),
                passed: self.passed(dimension),
                errors: issues
                    .iter()
                    .filter(|issue| issue.severity == Severity::Error)
                    .count(),
                warnings: issues
                    .iter()
                    .filter(|issue| issue.severity == Severity::Warning)
                    .count(),
                issues,
            }
        })
        .collect();
        ValidationReport {
            passed: !self.has_errors(),
            errors: self.errors(),
            warnings: self.warnings(),
            summary: self.summary(),
            dimensions,
        }
    }
}

/// 一次完整校验：结构校验失败时不再执行其余维度。
pub struct Evaluation {
    /// 结构合法时返回解析后的规则，供后续人工复核或落库。
    pub rule: Option<SkillRule>,
    pub report: Report,
}

/// 校验一份 SKILL.md 文本。
///
/// `candles` 是该规则周期的历史 K 线；为空时回测维度给出提示而不判失败。
pub fn evaluate(
    text: &str,
    limits: &RiskLimits,
    candles: &[SandboxCandle],
    costs: Costs,
) -> Evaluation {
    let parsed = structure::parse(text);
    let mut report = Report::default();
    report.extend(parsed.issues);
    let Some(rule) = parsed.rule else {
        return Evaluation { rule: None, report };
    };
    report.extend(evaluate_rule(&rule, limits, candles, costs).issues);
    Evaluation {
        rule: Some(rule),
        report,
    }
}

/// 直接校验一条已结构化的技能规则（跳过 YAML 解析）。
pub fn evaluate_rule(
    rule: &SkillRule,
    limits: &RiskLimits,
    candles: &[SandboxCandle],
    costs: Costs,
) -> Report {
    let mut report = Report::default();
    report.extend(rules::check(rule, limits));
    report.extend(math::check(rule));
    report.extend(agent::check(rule));
    report.extend(safety::check(rule, limits));
    report.extend(backtest::check(rule, candles, costs));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> RiskLimits {
        RiskLimits::default()
    }

    fn valid_yaml() -> &'static str {
        r#"
version: 1
name: 海龟突破
purpose: 价格突破唐奇安通道入场，反向突破或 2 倍 ATR 止损离场。
trigger:
  kind: breakout
  timeframe: 1h
  lookback: 300
  fast: 20
  slow: 55
  atr: 20
  channel: 20
  atr_multiple: 2
steps:
  execution_mode: paper
  execution_venues: [binance]
  direction: both
business:
  notional_usd: 100
  leverage: 2
  take_profit_pct: 0
  stop_loss_pct: 0
  grid_levels: 3
  grid_spacing_pct: 0.5
  grid_share_pct: 25
output:
  action: open_long
  notify: true
cautions:
  max_notional_usd: 0
  max_daily_opens: 0
"#
    }

    #[test]
    fn valid_document_passes_all_six_dimensions() {
        let evaluation = evaluate(valid_yaml(), &limits(), &[], Costs::default());
        let rule = evaluation.rule.expect("结构合法应返回规则");
        assert_eq!(rule.name, "海龟突破");
        assert_eq!(rule.trigger.lookback, 300);
        for dimension in [
            Dimension::Structure,
            Dimension::Rules,
            Dimension::Math,
            Dimension::Agent,
            Dimension::Safety,
        ] {
            assert!(
                evaluation.report.passed(dimension),
                "{} 未通过：{:?}",
                dimension.label(),
                evaluation
                    .report
                    .issues_in(dimension)
                    .collect::<Vec<_>>()
            );
        }
        let view = evaluation.report.view();
        assert!(view.passed);
        assert_eq!(view.dimensions.len(), 6);
        assert!(view.blockers().next().is_none());
    }

    #[test]
    fn summary_lists_every_dimension() {
        let evaluation = evaluate(valid_yaml(), &limits(), &[], Costs::default());
        let summary = evaluation.report.summary();
        for dimension in [
            Dimension::Structure,
            Dimension::Rules,
            Dimension::Math,
            Dimension::Backtest,
            Dimension::Agent,
            Dimension::Safety,
        ] {
            assert!(summary.contains(dimension.label()));
        }
    }
}
