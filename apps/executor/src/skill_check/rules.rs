//! 第二维：业务规则完整性与互相矛盾检查。
//!
//! 「完整」指入场、出场、止损、冷却、持仓上限都有明确取值；「矛盾」指参数组合
//! 会让运行期行为与规则描述不一致，例如均线快慢周期相等、网格间距大于 100%、
//! 杠杆超过执行器上限。这里只判断，不改写规则。

use arb_core::risk::venue_min_notional_usd;
use arb_core::{RiskLimits, StrategyExecutionMode};
use rust_decimal::Decimal;

use super::{Dimension, Issue};
use crate::market::KLINE_INTERVALS;
use crate::skill::{
    SkillAction, SkillDirection, SkillKind, SkillRule, MAX_EVENT_WINDOW_SECS, MAX_GRID_LEVELS,
    MIN_EVENT_WINDOW_SECS,
};

const MIN_LOOKBACK: u32 = 20;
const MAX_LOOKBACK: u32 = 1_000;
const MIN_PERIOD: u32 = 2;
const MAX_PERIOD: u32 = 200;

/// 校验一条规则的业务参数。
pub fn check(rule: &SkillRule, limits: &RiskLimits) -> Vec<Issue> {
    let mut issues = Vec::new();
    check_identity(rule, &mut issues);
    check_trigger(rule, &mut issues);
    check_steps(rule, &mut issues);
    check_business(rule, limits, &mut issues);
    check_cautions(rule, &mut issues);
    issues
}

fn check_identity(rule: &SkillRule, issues: &mut Vec<Issue>) {
    if rule.name.trim().is_empty() {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.empty_name",
            "规则名称为空，运行记录与告警无法区分是哪条规则",
        ));
    }
    if rule.purpose.trim().is_empty() {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.empty_purpose",
            "技能用途为空：告警文案与运行快照都会缺少说明",
        ));
    }
    if !rule.enabled {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.disabled",
            "规则已停用，不参与运行；本次仅做静态检查",
        ));
    }
}

fn check_trigger(rule: &SkillRule, issues: &mut Vec<Issue>) {
    let trigger = &rule.trigger;

    if !KLINE_INTERVALS.contains(&trigger.timeframe.as_str()) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.unsupported_timeframe",
            format!(
                "K 线周期 `{}` 不在支持范围：{}",
                trigger.timeframe,
                KLINE_INTERVALS.join(", ")
            ),
        ));
    }
    if !(MIN_LOOKBACK..=MAX_LOOKBACK).contains(&trigger.lookback) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.lookback_range",
            format!(
                "lookback {} 超出 {MIN_LOOKBACK}..={MAX_LOOKBACK}；过小会算不出指标，过大浪费行情请求",
                trigger.lookback
            ),
        ));
    }
    for (name, period) in [
        ("fast", trigger.fast),
        ("slow", trigger.slow),
        ("atr", trigger.atr),
        ("channel", trigger.channel),
    ] {
        if !(MIN_PERIOD..=MAX_PERIOD).contains(&period) {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.indicator_period_range",
                format!("{name} 周期 {period} 超出 {MIN_PERIOD}..={MAX_PERIOD}"),
            ));
        }
    }
    let longest = trigger
        .slow
        .max(trigger.channel)
        .max(trigger.atr.saturating_add(1))
        .max(trigger.fast);
    if trigger.lookback < longest {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.indicator_window_not_covered",
            format!(
                "lookback {} 小于最长指标窗口 {longest}，指标会长期为空、策略永不触发",
                trigger.lookback
            ),
        ));
    }
    if rule.kind == SkillKind::MaTrend && trigger.fast >= trigger.slow {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.fast_not_below_slow",
            format!(
                "均线趋势要求快线周期小于慢线，当前 fast={} slow={}，信号会退化为恒定方向",
                trigger.fast, trigger.slow
            ),
        ));
    }
    if trigger.atr_multiple < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.negative_atr_multiple",
            "ATR 倍数不能为负数",
        ));
    }

    if rule.kind == SkillKind::Event {
        if !(MIN_EVENT_WINDOW_SECS..=MAX_EVENT_WINDOW_SECS).contains(&trigger.event_window_secs) {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.event_window_range",
                format!(
                    "事件窗口 {} 秒超出 {MIN_EVENT_WINDOW_SECS}..={MAX_EVENT_WINDOW_SECS}",
                    trigger.event_window_secs
                ),
            ));
        }
        if rule.steps.direction == SkillDirection::Short {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.event_direction_long_only",
                "公告类技能固定买入开仓，direction=short 不会生效；请改为 both 或 long",
            ));
        }
        if rule.steps.direction == SkillDirection::Both {
            issues.push(Issue::info(
                Dimension::Rules,
                "rules.event_direction_ignored",
                "公告类技能只会做多，direction=both 与 long 等价",
            ));
        }
    } else {
        if trigger.event_window_secs != crate::skill::SkillTrigger::default().event_window_secs {
            issues.push(Issue::info(
                Dimension::Rules,
                "rules.unused_event_window",
                "非公告类技能不会读取 event_window_secs",
            ));
        }
        if !trigger.event_sources.is_empty() || !trigger.event_keywords.is_empty() {
            issues.push(Issue::info(
                Dimension::Rules,
                "rules.unused_event_fields",
                "非公告类技能不会读取 event_sources / event_keywords",
            ));
        }
    }
}

fn check_steps(rule: &SkillRule, issues: &mut Vec<Issue>) {
    let mut seen = std::collections::HashSet::new();
    for venue in &rule.steps.execution_venues {
        if !seen.insert(venue.to_ascii_lowercase()) {
            issues.push(Issue::warning(
                Dimension::Rules,
                "rules.duplicate_venue",
                format!("执行交易所 {venue} 重复出现，优先级顺序不明确"),
            ));
        }
    }
    if rule.steps.execution_mode == StrategyExecutionMode::Real {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.real_execution",
            "执行步骤为真实下单：所有订单会经统一风控网关提交到交易所",
        ));
    } else {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.paper_execution",
            "执行步骤为模拟下单：只记录模拟成交，不提交交易所",
        ));
    }
}

fn check_business(rule: &SkillRule, limits: &RiskLimits, issues: &mut Vec<Issue>) {
    let business = &rule.business;
    if business.notional_usd <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.notional_non_positive",
            "下单金额必须大于 0",
        ));
    }
    if business.leverage <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.leverage_non_positive",
            "杠杆必须大于 0",
        ));
    } else if business.leverage > limits.max_leverage {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.leverage_above_limit",
            format!(
                "规则杠杆 {} 超过执行器上限 {}，运行期会被风控拒绝",
                business.leverage, limits.max_leverage
            ),
        ));
    }
    if business.take_profit_pct < Decimal::ZERO || business.stop_loss_pct < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.negative_percent",
            "止盈 / 止损百分比不能为负数",
        ));
    }

    let effective = effective_notional(rule);
    if effective > limits.max_order_notional_usd {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.notional_above_limit",
            format!(
                "有效单笔金额 {effective} 超过执行器单笔上限 {}，运行期会被风控拒绝",
                limits.max_order_notional_usd
            ),
        ));
    } else if business.notional_usd > limits.max_order_notional_usd {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.notional_capped_by_caution",
            format!(
                "业务金额 {} 超过单笔上限，但注意事项上限使其实际只用 {effective}",
                business.notional_usd
            ),
        ));
    }

    if rule.kind == SkillKind::Grid {
        check_grid(rule, effective, issues);
    } else {
        if business.grid_levels != 3
            || business.grid_spacing_pct != Decimal::new(5, 1)
            || business.grid_share_pct != Decimal::from(25)
        {
            issues.push(Issue::info(
                Dimension::Rules,
                "rules.unused_grid_fields",
                "非网格技能不会读取 grid_levels / grid_spacing_pct / grid_share_pct",
            ));
        }
    }

    if rule.kind == SkillKind::Event
        && business.take_profit_pct <= Decimal::ZERO
        && business.stop_loss_pct <= Decimal::ZERO
    {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.no_exit",
            "公告类技能没有止盈、止损，也没有信号离场，开仓后将无法自动退出",
        ));
    }

    if rule.cautions.max_daily_opens == 0 {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.no_cooldown_or_cap",
            "没有每日开仓上限，同一信号会反复开仓",
        ));
    }
}

fn check_grid(rule: &SkillRule, effective_notional: Decimal, issues: &mut Vec<Issue>) {
    let business = &rule.business;
    if business.grid_levels < 1 || business.grid_levels > MAX_GRID_LEVELS {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_levels_range",
            format!(
                "网格档位数 {} 超出 1..={MAX_GRID_LEVELS}",
                business.grid_levels
            ),
        ));
    }
    if business.grid_spacing_pct <= Decimal::ZERO || business.grid_spacing_pct >= Decimal::from(100)
    {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_spacing_range",
            format!(
                "网格间距 {}% 必须在 (0, 100) 之间，否则档位价会为负或反向",
                business.grid_spacing_pct
            ),
        ));
    } else if business.grid_spacing_pct * Decimal::from(business.grid_levels)
        >= Decimal::from(100)
    {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_spacing_negative_price",
            format!(
                "间距 {}% × {} 档 ≥ 100%，最远买档价格会变成 0 或负数",
                business.grid_spacing_pct, business.grid_levels
            ),
        ));
    }
    if business.grid_share_pct <= Decimal::ZERO || business.grid_share_pct > Decimal::from(100) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_share_range",
            format!(
                "网格每档金额占比 {}% 超出 (0, 100]",
                business.grid_share_pct
            ),
        ));
    } else {
        let per_level = effective_notional * business.grid_share_pct / Decimal::from(100);
        for venue in &rule.steps.execution_venues {
            if let Some(minimum) = venue_min_notional_usd(venue) {
                if per_level < minimum {
                    issues.push(Issue::error(
                        Dimension::Rules,
                        "rules.grid_below_min_notional",
                        format!(
                            "{venue} 每档金额 {per_level} 低于交易所最小下单额 {minimum}"
                        ),
                    ));
                }
            }
        }
    }
    if business.take_profit_pct > Decimal::ZERO {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.grid_take_profit_ignored",
            "网格止盈使用相邻档位间距，take_profit_pct 不会生效",
        ));
    }
    if business.stop_loss_pct <= Decimal::ZERO {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.grid_no_stop",
            "网格没有止损，单边行情会持续持有浮亏档位",
        ));
    }
}

fn check_cautions(rule: &SkillRule, issues: &mut Vec<Issue>) {
    let cautions = &rule.cautions;
    if cautions.max_notional_usd < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.cautions_negative",
            "单笔金额上限不能为负数",
        ));
    } else if cautions.max_notional_usd > Decimal::ZERO
        && cautions.max_notional_usd < rule.business.notional_usd
    {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.cautions_caps_notional",
            format!(
                "注意事项把单笔金额从 {} 收紧到 {}",
                rule.business.notional_usd, cautions.max_notional_usd
            ),
        ));
    }
    if rule.output.action == SkillAction::NotifyOnly && cautions.max_daily_opens > 0 {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.notify_only_daily_cap_ignored",
            "只提醒模式不占用每日开仓次数",
        ));
    }
}

/// 有效单笔金额：业务金额与注意事项上限取小，与运行期 `capped_notional` 一致。
pub fn effective_notional(rule: &SkillRule) -> Decimal {
    if rule.cautions.max_notional_usd > Decimal::ZERO {
        rule.business
            .notional_usd
            .min(rule.cautions.max_notional_usd)
    } else {
        rule.business.notional_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn limits() -> RiskLimits {
        RiskLimits::default()
    }

    fn breakout() -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            purpose: "突破入场".to_string(),
            kind: SkillKind::Breakout,
            trigger: crate::skill::SkillTrigger {
                timeframe: "1h".to_string(),
                lookback: 300,
                fast: 20,
                slow: 55,
                atr: 20,
                channel: 20,
                ..crate::skill::SkillTrigger::default()
            },
            steps: crate::skill::SkillSteps {
                execution_mode: StrategyExecutionMode::Paper,
                execution_venues: vec!["binance".to_string()],
                direction: SkillDirection::Both,
            },
            business: crate::skill::SkillBusinessRules {
                notional_usd: dec!(100),
                leverage: dec!(2),
                take_profit_pct: Decimal::ZERO,
                stop_loss_pct: Decimal::ZERO,
                ..crate::skill::SkillBusinessRules::default()
            },
            ..SkillRule::default()
        }
    }

    fn error_codes(issues: &[Issue]) -> Vec<&'static str> {
        issues
            .iter()
            .filter(|issue| issue.severity == super::super::Severity::Error)
            .map(|issue| issue.code)
            .collect()
    }

    #[test]
    fn valid_breakout_rule_has_no_errors() {
        let issues = check(&breakout(), &limits());
        assert!(error_codes(&issues).is_empty(), "{issues:?}");
    }

    #[test]
    fn leverage_above_executor_limit_is_rejected() {
        let mut rule = breakout();
        rule.business.leverage = dec!(50);
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.leverage_above_limit"));
    }

    #[test]
    fn lookback_must_cover_longest_window() {
        let mut rule = breakout();
        rule.trigger.lookback = 30;
        assert!(
            error_codes(&check(&rule, &limits()))
                .contains(&"rules.indicator_window_not_covered")
        );
    }

    #[test]
    fn lookback_out_of_range_is_rejected() {
        let mut rule = breakout();
        rule.trigger.lookback = 10;
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.lookback_range"));
    }

    #[test]
    fn grid_spacing_over_hundred_percent_is_rejected() {
        let mut rule = breakout();
        rule.kind = SkillKind::Grid;
        rule.business.grid_spacing_pct = dec!(120);
        rule.business.grid_levels = 3;
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.grid_spacing_range"));
    }

    #[test]
    fn grid_levels_can_push_price_below_zero() {
        let mut rule = breakout();
        rule.kind = SkillKind::Grid;
        rule.business.grid_levels = 20;
        rule.business.grid_spacing_pct = dec!(6);
        assert!(
            error_codes(&check(&rule, &limits())).contains(&"rules.grid_spacing_negative_price")
        );
    }

    #[test]
    fn grid_per_level_below_venue_minimum_is_rejected() {
        let mut rule = breakout();
        rule.kind = SkillKind::Grid;
        rule.steps.execution_venues = vec!["lighter".to_string()];
        rule.business.notional_usd = dec!(100);
        rule.business.grid_share_pct = dec!(5);
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.grid_below_min_notional"));
    }

    #[test]
    fn event_short_direction_is_rejected() {
        let mut rule = breakout();
        rule.kind = SkillKind::Event;
        rule.steps.direction = SkillDirection::Short;
        assert!(
            error_codes(&check(&rule, &limits())).contains(&"rules.event_direction_long_only")
        );
    }

    #[test]
    fn event_without_exit_is_a_warning() {
        let mut rule = breakout();
        rule.kind = SkillKind::Event;
        rule.business.take_profit_pct = Decimal::ZERO;
        rule.business.stop_loss_pct = Decimal::ZERO;
        let issues = check(&rule, &limits());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "rules.no_exit"
                && issue.severity == super::super::Severity::Warning));
    }

    #[test]
    fn notional_above_executor_limit_is_rejected() {
        let mut rule = breakout();
        rule.business.notional_usd = dec!(9000);
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.notional_above_limit"));
    }

    #[test]
    fn cautions_can_bring_notional_back_under_limit() {
        let mut rule = breakout();
        rule.business.notional_usd = dec!(9000);
        rule.cautions.max_notional_usd = dec!(300);
        let issues = check(&rule, &limits());
        assert!(error_codes(&issues).is_empty(), "{issues:?}");
        assert!(issues
            .iter()
            .any(|issue| issue.code == "rules.cautions_caps_notional"));
    }

    #[test]
    fn ma_trend_with_equal_periods_is_a_warning() {
        let mut rule = breakout();
        rule.kind = SkillKind::MaTrend;
        rule.trigger.fast = 20;
        rule.trigger.slow = 20;
        assert!(check(&rule, &limits())
            .iter()
            .any(|issue| issue.code == "rules.fast_not_below_slow"));
    }
}
