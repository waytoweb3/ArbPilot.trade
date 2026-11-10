//! 第四维：回测结果可信度。
//!
//! 用真实历史 K 线跑一遍 [`crate::skill_backtest`]，检查成本口径、样本量与成交数量，
//! 并把收益、最大回撤、胜率、交易频率、手续费与滑点作为结论输出。
//!
//! 这里不重复实现回放逻辑：回测可信度的前提是「回测与实盘共用同一份规则」，
//! 因此校验器只核对口径与统计，不另写一套回放。

use arb_core::SandboxCandle;
use rust_decimal::Decimal;

use super::{Dimension, Issue};
use crate::skill::{self, SkillKind, SkillRule};
use crate::skill_backtest;

pub use crate::skill_backtest::Costs;

/// 少于该笔数时统计量不具代表性。
const MIN_TRADES: usize = 10;
/// 少于该根数时样本过短，回测结论仅供参考。
const MIN_CANDLES: usize = 100;

pub fn check(rule: &SkillRule, candles: &[SandboxCandle], costs: Costs) -> Vec<Issue> {
    let mut issues = Vec::new();

    if costs.fee_bps_per_leg <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Backtest,
            "backtest.zero_fees",
            "手续费假设为 0，回测收益会被系统性高估".to_string(),
        ));
    }
    if costs.slippage_bps <= Decimal::ZERO {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.zero_slippage",
            "滑点假设为 0，成交价过于乐观；建议至少保留 1 bps".to_string(),
        ));
    }

    if rule.kind == SkillKind::Event {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.event_not_replayable",
            "公告类技能依赖实时公告，历史 K 线无法回放；请在模拟模式下实测".to_string(),
        ));
        return issues;
    }

    if candles.is_empty() {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.no_sample",
            "未提供历史 K 线，回测维度未执行".to_string(),
        ));
        return issues;
    }

    let warmup = warmup_bars(rule);
    if candles.len() <= warmup {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.short_sample",
            format!(
                "K 线只有 {} 根，不足以完成 {warmup} 根预热，无法产生信号",
                candles.len()
            ),
        ));
        return issues;
    }
    if candles.len() < MIN_CANDLES {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.short_sample",
            format!(
                "样本只有 {} 根 K 线（建议 ≥ {MIN_CANDLES} 根），结论代表性有限",
                candles.len()
            ),
        ));
    }

    let notional = skill::capped_notional(rule);
    if notional <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Backtest,
            "backtest.no_notional",
            "有效下单金额为 0，回测无法产生成交".to_string(),
        ));
        return issues;
    }

    let trades = skill_backtest::replay(rule, "BACKTEST", candles, notional, costs);
    let days = span_days(candles);
    let metrics = skill_backtest::summarize(&trades, notional, days);
    if metrics.trades == 0 {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.no_trades",
            "样本内没有触发任何成交，无法评估策略收益".to_string(),
        ));
        return issues;
    }
    if metrics.trades < MIN_TRADES {
        issues.push(Issue::warning(
            Dimension::Backtest,
            "backtest.few_trades",
            format!(
                "样本内只有 {} 笔成交（建议 ≥ {MIN_TRADES} 笔），胜率与回撤波动较大",
                metrics.trades
            ),
        ));
    }

    issues.push(Issue::info(
        Dimension::Backtest,
        "backtest.metrics",
        format!(
            "回测 {} 笔：胜率 {}%，净收益 {} USD，收益率 {}%，最大回撤 {}%，频率 {} 笔/天",
            metrics.trades,
            metrics.win_rate_pct,
            metrics.net_pnl_usd,
            metrics.return_pct,
            metrics.max_drawdown_pct,
            metrics.trades_per_day
        ),
    ));
    issues.push(Issue::info(
        Dimension::Backtest,
        "backtest.assumptions",
        format!(
            "成本口径：单边手续费 {} bps、滑点 {} bps，按开平仓两次计入；毛收益 {} USD，手续费 {} USD，滑点 {} USD",
            costs.fee_bps_per_leg, costs.slippage_bps, metrics.gross_usd, metrics.fees_usd, metrics.slippage_usd
        ),
    ));
    issues
}

/// 指标全部可用的最少 K 线根数，与回测预热口径保持一致。
fn warmup_bars(rule: &SkillRule) -> usize {
    let trigger = &rule.trigger;
    trigger
        .slow
        .max(trigger.channel.saturating_add(1))
        .max(trigger.atr.saturating_add(1))
        .max(trigger.fast)
        .max(3) as usize
}

fn span_days(candles: &[SandboxCandle]) -> i64 {
    match (candles.first(), candles.last()) {
        (Some(first), Some(last)) => {
            ((last.open_time - first.open_time).num_seconds() / 86_400).max(1)
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    fn candle(index: i64, close: Decimal) -> SandboxCandle {
        SandboxCandle {
            open_time: Utc
                .timestamp_opt(index * 3_600, 0)
                .single()
                .expect("合法时间"),
            open: close,
            high: close,
            low: close,
            close,
            volume: Decimal::ONE,
        }
    }

    fn rule(kind: SkillKind) -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            kind,
            trigger: crate::skill::SkillTrigger {
                timeframe: "1h".to_string(),
                fast: 2,
                slow: 3,
                atr: 2,
                channel: 3,
                atr_multiple: dec!(2),
                ..crate::skill::SkillTrigger::default()
            },
            business: crate::skill::SkillBusinessRules {
                notional_usd: dec!(100),
                ..crate::skill::SkillBusinessRules::default()
            },
            ..SkillRule::default()
        }
    }

    fn trending() -> Vec<SandboxCandle> {
        (0..160)
            .map(|index| {
                // 每 20 根抬升 10，制造可重复的突破与回落。
                let base = 100 + (index / 20) * 10 + index % 3;
                candle(index, Decimal::from(base))
            })
            .collect()
    }

    #[test]
    fn trending_sample_produces_metrics_without_errors() {
        let issues = check(&rule(SkillKind::Breakout), &trending(), Costs::default());
        assert!(
            !issues
                .iter()
                .any(|issue| issue.severity == super::super::Severity::Error),
            "默认成本与趋势样本不应报错：{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.code == "backtest.metrics"),
            "应输出收益/回撤/胜率统计"
        );
    }

    #[test]
    fn zero_fee_is_an_error() {
        let costs = Costs {
            fee_bps_per_leg: Decimal::ZERO,
            slippage_bps: Decimal::ONE,
        };
        let issues = check(&rule(SkillKind::Breakout), &trending(), costs);
        assert!(issues.iter().any(|issue| issue.code == "backtest.zero_fees"
            && issue.severity == super::super::Severity::Error));
    }

    #[test]
    fn event_rule_reports_no_replay() {
        let issues = check(&rule(SkillKind::Event), &trending(), Costs::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "backtest.event_not_replayable"));
    }

    #[test]
    fn empty_sample_is_reported() {
        let issues = check(&rule(SkillKind::Breakout), &[], Costs::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "backtest.no_sample"));
    }

    #[test]
    fn flat_market_reports_no_trades() {
        let flat: Vec<SandboxCandle> = (0..160).map(|index| candle(index, dec!(100))).collect();
        let issues = check(&rule(SkillKind::Breakout), &flat, Costs::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "backtest.no_trades"));
    }
}
