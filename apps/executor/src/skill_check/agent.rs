//! 第五维：Agent 行为校验。
//!
//! 用合成行情与固定参数模拟 Agent 的三步动作：
//! 1. 识别触发条件：会不会该出手时不出手、横盘时乱出手；
//! 2. 选择技能：`NotifyOnly`、`Paper`、空执行交易所等配置是否被正确理解；
//! 3. 执行步骤：是否会把「只提醒」当成真实下单，或把模拟单发到交易所。

use arb_core::{indicators, DataRules, SandboxCandle, StrategyExecutionMode};
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use super::{Dimension, Issue};
use crate::skill::{self, SkillAction, SkillDirection, SkillKind, SkillRule};

pub fn check(rule: &SkillRule) -> Vec<Issue> {
    let mut issues = Vec::new();
    check_selection(rule, &mut issues);
    check_trigger(rule, &mut issues);
    check_execution(rule, &mut issues);
    issues
}

/// 技能选择：输出动作与执行交易所是否会被运行期正确解释。
fn check_selection(rule: &SkillRule, issues: &mut Vec<Issue>) {
    if rule.output.action == SkillAction::NotifyOnly {
        if rule.kind == SkillKind::Event {
            issues.push(Issue::info(
                Dimension::Agent,
                "agent.event_notify_only",
                "公告类技能只提醒：会占用冷却窗口，但不占用每日开仓额度".to_string(),
            ));
        } else {
            issues.push(Issue::error(
                Dimension::Agent,
                "agent.notify_only_ignored",
                "运行期只对公告类技能识别「只提醒」；指标类技能仍会按执行模式开仓。\
                 若只想观察信号，请把执行模式改成模拟"
                    .to_string(),
            ));
        }
    }

    if rule.steps.execution_venues.is_empty() && rule.kind != SkillKind::Event {
        issues.push(Issue::warning(
            Dimension::Agent,
            "agent.venues_fallback",
            "未指定执行交易所：运行期会回落到全部可交易交易所（模拟模式还会回落到策略交易范围），\
             同一信号可能落到意料之外的交易所"
                .to_string(),
        ));
    }
}

/// 触发识别：在合成行情上必须能识别出至少一次触发。
fn check_trigger(rule: &SkillRule, issues: &mut Vec<Issue>) {
    if rule.kind == SkillKind::Event {
        issues.push(Issue::info(
            Dimension::Agent,
            "agent.event_requires_news",
            "公告类技能的触发依赖实时公告，无法用 K 线模拟；请在模拟模式下用真实公告验证"
                .to_string(),
        ));
        return;
    }

    let candles = match rule.kind {
        SkillKind::Grid => grid_series(),
        _ => signal_series(rule.steps.direction),
    };
    let hits = match rule.kind {
        SkillKind::Grid => grid_hits(rule, &candles),
        _ => signal_hits(rule, &candles),
    };
    if hits == 0 {
        issues.push(Issue::warning(
            Dimension::Agent,
            "agent.never_triggers",
            format!(
                "在 {} 根模拟 K 线上一次都没有识别到触发条件，请检查方向、周期与档位设置",
                candles.len()
            ),
        ));
    }
}

/// 执行步骤：模拟/真实模式是否符合预期。
fn check_execution(rule: &SkillRule, issues: &mut Vec<Issue>) {
    match rule.steps.execution_mode {
        StrategyExecutionMode::Paper => issues.push(Issue::info(
            Dimension::Agent,
            "agent.paper_mode",
            "模拟模式：只记录成交与盈亏，不会向交易所提交任何委托".to_string(),
        )),
        StrategyExecutionMode::Real => {}
    }
}

/// 指标类技能的模拟行情：先横盘制造通道，再单边推动触发信号。
fn signal_series(direction: SkillDirection) -> Vec<SandboxCandle> {
    let falling = direction == SkillDirection::Short;
    let mut price = Decimal::from(100);
    let mut candles = Vec::new();
    for index in 0..80i64 {
        if index >= 20 {
            price += if falling { -Decimal::ONE } else { Decimal::ONE };
        }
        let close = price.max(Decimal::ONE);
        candles.push(SandboxCandle {
            open_time: Utc
                .timestamp_opt(index * 3_600, 0)
                .single()
                .expect("合法时间"),
            open: close,
            high: close,
            low: close,
            close,
            volume: Decimal::ONE,
        });
    }
    candles
}

/// 网格的模拟行情：围绕锚定价上下摆动，确保买卖档位都会被触及。
fn grid_series() -> Vec<SandboxCandle> {
    (0..60i64)
        .map(|index| {
            let close = Decimal::from(100 + (index % 5) * 2 - 4);
            SandboxCandle {
                open_time: Utc
                    .timestamp_opt(index * 3_600, 0)
                    .single()
                    .expect("合法时间"),
                open: close,
                high: close + Decimal::ONE,
                low: close - Decimal::ONE,
                close,
                volume: Decimal::ONE,
            }
        })
        .collect()
}

/// 逐根回放，统计识别到信号的 K 线数量；只用当前及之前的窗口，避免未来函数。
fn signal_hits(rule: &SkillRule, candles: &[SandboxCandle]) -> usize {
    let data = data_rules(rule);
    let warmup = warmup_bars(rule);
    if candles.len() <= warmup {
        return 0;
    }
    let mut hits = 0;
    for index in warmup..candles.len() {
        let window = &candles[..=index];
        let last = window[window.len() - 1].close;
        let values = indicators::compute(window, &data);
        let signal = match rule.kind {
            SkillKind::Breakout => match (values.channel_high, values.channel_low) {
                (Some(high), Some(low)) => {
                    skill::breakout_signal(last, high, low, rule.steps.direction)
                }
                _ => None,
            },
            SkillKind::MaTrend => match (values.sma_fast, values.sma_slow) {
                (Some(fast), Some(slow)) => {
                    skill::ma_trend_signal(last, fast, slow, rule.steps.direction)
                }
                _ => None,
            },
            _ => None,
        };
        if signal.is_some() {
            hits += 1;
        }
    }
    hits
}

/// 网格按锚定价展开档位，统计至少触及一个允许方向档位的 K 线数量。
fn grid_hits(rule: &SkillRule, candles: &[SandboxCandle]) -> usize {
    let Some(anchor) = candles.first().map(|candle| candle.close) else {
        return 0;
    };
    let (buys, sells) = skill::grid_levels(
        anchor,
        rule.business.grid_levels,
        rule.business.grid_spacing_pct,
    );
    if buys.is_empty() && sells.is_empty() {
        return 0;
    }
    candles
        .iter()
        .filter(|candle| {
            let buy_hit = rule.steps.direction != SkillDirection::Short
                && buys.iter().any(|level| candle.low <= *level);
            let sell_hit = rule.steps.direction != SkillDirection::Long
                && sells.iter().any(|level| candle.high >= *level);
            buy_hit || sell_hit
        })
        .count()
}

fn data_rules(rule: &SkillRule) -> DataRules {
    DataRules {
        timeframe: rule.trigger.timeframe.clone(),
        lookback: rule.trigger.lookback.clamp(20, 1_000),
        fast: rule.trigger.fast.max(2),
        slow: rule.trigger.slow.max(2),
        atr: rule.trigger.atr.max(2),
        channel: rule.trigger.channel.max(2),
        ..DataRules::default()
    }
}

fn warmup_bars(rule: &SkillRule) -> usize {
    let trigger = &rule.trigger;
    trigger
        .slow
        .max(trigger.channel.saturating_add(1))
        .max(trigger.atr.saturating_add(1))
        .max(trigger.fast)
        .max(3) as usize
}

fn bar_secs(timeframe: &str) -> i64 {
    match timeframe {
        "30s" => 30,
        "1m" => 60,
        "5m" => 300,
        "10m" => 600,
        "15m" => 900,
        "30m" => 1_800,
        "1h" => 3_600,
        "4h" => 14_400,
        "1d" => 86_400,
        "1w" => 604_800,
        _ => 3_600,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{SkillBusinessRules, SkillOutput, SkillSteps, SkillTrigger};

    fn breakout_rule(direction: SkillDirection) -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            kind: SkillKind::Breakout,
            trigger: SkillTrigger {
                timeframe: "1h".to_string(),
                lookback: 100,
                fast: 3,
                slow: 5,
                atr: 3,
                channel: 5,
                atr_multiple: rust_decimal_macros::dec!(2),
                ..SkillTrigger::default()
            },
            steps: SkillSteps {
                execution_mode: StrategyExecutionMode::Paper,
                execution_venues: vec!["binance".to_string()],
                direction,
            },
            business: SkillBusinessRules {
                notional_usd: rust_decimal_macros::dec!(100),
                ..SkillBusinessRules::default()
            },
            output: SkillOutput::default(),
            ..SkillRule::default()
        }
    }

    #[test]
    fn rising_market_triggers_breakout() {
        let issues = check(&breakout_rule(SkillDirection::Both));
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "agent.never_triggers"),
            "单边上涨应识别到突破：{issues:?}"
        );
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "agent.notify_only_ignored"));
    }

    #[test]
    fn notify_only_on_indicator_is_an_error() {
        let mut rule = breakout_rule(SkillDirection::Both);
        rule.output.action = SkillAction::NotifyOnly;
        let issues = check(&rule);
        assert!(issues.iter().any(|issue| {
            issue.code == "agent.notify_only_ignored"
                && issue.severity == super::super::Severity::Error
        }));
    }

    #[test]
    fn notify_only_on_event_stays_informational() {
        let mut rule = breakout_rule(SkillDirection::Both);
        rule.kind = SkillKind::Event;
        rule.output.action = SkillAction::NotifyOnly;
        let issues = check(&rule);
        assert!(!issues
            .iter()
            .any(|issue| { issue.severity == super::super::Severity::Error }));
        assert!(issues
            .iter()
            .any(|issue| issue.code == "agent.event_notify_only"));
    }

    #[test]
    fn empty_venues_is_a_warning() {
        let mut rule = breakout_rule(SkillDirection::Both);
        rule.steps.execution_venues.clear();
        let issues = check(&rule);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "agent.venues_fallback"));
    }

    #[test]
    fn grid_rule_touches_levels() {
        let mut rule = breakout_rule(SkillDirection::Both);
        rule.kind = SkillKind::Grid;
        rule.business.grid_levels = 2;
        rule.business.grid_spacing_pct = rust_decimal_macros::dec!(1);
        let issues = check(&rule);
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "agent.never_triggers"));
    }

    #[test]
    fn paper_mode_is_reported() {
        let issues = check(&breakout_rule(SkillDirection::Both));
        assert!(issues.iter().any(|issue| issue.code == "agent.paper_mode"));
    }
}
