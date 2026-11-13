//! 第六维：交易安全。
//!
//! 两段式校验：
//! 1. **规则侧**：真实下单却没有离场条件、金额低于交易所下限；
//! 2. **网关侧**：把规则的下单参数拼成订单意图，交给真实的风控网关
//!    [`arb_core::RiskEngine::preflight`] 跑一遍，确认超限、重复腿、行情过期、
//!    暂停、敞口不足这些异常场景仍然会被拒绝，且平仓不会被限额挡住。
//!
//! 交易所侧的部分成交、资金不足拒单、断网后的补单无法在本地复现，
//! 这里明确标注为需要实盘监控，而不是假装已经覆盖。

use arb_core::{
    risk::venue_min_notional_usd, OrderIntent, OrderLegIntent, OrderOrigin, RiskContext,
    RiskDecision, RiskEngine, RiskLimits, Side, StrategyExecutionMode,
};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;

use super::{Dimension, Issue};
use crate::skill::{self, SkillDirection, SkillKind, SkillRule};

/// 校验用的合成价格：只为把金额换成数量，与真实行情无关。
const SAMPLE_PRICE: i64 = 60_000;

pub fn check(rule: &SkillRule, limits: &RiskLimits) -> Vec<Issue> {
    let mut issues = Vec::new();
    check_rule(rule, limits, &mut issues);
    check_engine(limits, &mut issues);
    issues
}

/// 规则本身的交易安全。
fn check_rule(rule: &SkillRule, limits: &RiskLimits, issues: &mut Vec<Issue>) {
    let has_exit = rule.business.take_profit_pct > Decimal::ZERO
        || rule.business.stop_loss_pct > Decimal::ZERO
        || rule.kind == SkillKind::Grid;
    if rule.steps.execution_mode == StrategyExecutionMode::Real && !has_exit {
        issues.push(Issue::warning(
            Dimension::Safety,
            "safety.real_without_exit",
            "真实下单但没有任何止盈、止损条件，浮亏可能无限扩大".to_string(),
        ));
    }

    let notional = skill::capped_notional(rule);
    for venue in &rule.steps.execution_venues {
        let Some(minimum) = venue_min_notional_usd(venue) else {
            continue;
        };
        // 网格把下单金额按档位数均分，按每档金额判断更有意义。
        let per_order = if rule.kind == SkillKind::Grid {
            notional * rule.business.grid_share()
        } else {
            notional
        };
        if per_order > Decimal::ZERO && per_order < minimum {
            issues.push(Issue::error(
                Dimension::Safety,
                "safety.below_venue_minimum",
                format!("{venue} 单笔金额 {per_order} USD 低于交易所最小下单额 {minimum} USD"),
            ));
        }
    }

    // 现货腿只能买入（做多），带做空方向时现货交易所上的信号会被跳过。
    if rule.steps.direction != SkillDirection::Long
        && rule
            .steps
            .execution_venues
            .iter()
            .any(|venue| crate::market::is_spot_venue(venue))
    {
        issues.push(Issue::warning(
            Dimension::Safety,
            "safety.spot_long_only",
            "现货交易所只能做多，做空信号会在现货腿上被跳过".to_string(),
        ));
    }

    // 规则金额与杠杆必须能通过网关；否则策略「看起来在跑，实际一单都下不出去」。
    let now = Utc::now();
    let intent = intent(rule, limits, notional, now);
    let context = context(limits, now, Decimal::ZERO, false);
    if let RiskDecision::Rejected { violations } = RiskEngine::preflight(&intent, &context) {
        issues.push(Issue::error(
            Dimension::Safety,
            "safety.order_would_be_rejected",
            format!(
                "按规则参数下单会被风控拒绝：{}",
                violations
                    .iter()
                    .map(|violation| format!("{}（{:?}）", violation.message, violation.code))
                    .collect::<Vec<_>>()
                    .join("；")
            ),
        ));
    }
}

/// 网关异常场景：每一个都必须被拒绝，平仓必须始终放行。
fn check_engine(limits: &RiskLimits, issues: &mut Vec<Issue>) {
    let now = Utc::now();
    let price = Decimal::from(SAMPLE_PRICE);
    let within_cap = limits.max_order_notional_usd.min(Decimal::from(1_000));
    let quantity = if price > Decimal::ZERO {
        within_cap / price
    } else {
        Decimal::ZERO
    };

    let base = |observed_at: DateTime<Utc>| OrderIntent {
        idempotency_key: "skill-check".to_string(),
        origin: OrderOrigin::Strategy,
        strategy_id: None,
        legs: vec![OrderLegIntent {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            quantity,
            limit_price: price,
            reduce_only: false,
        }],
        leverage: Decimal::ONE,
        max_slippage_bps: Decimal::ZERO,
        quote_observed_at: observed_at,
        created_at: observed_at,
    };

    let cases: [(&str, &str, OrderIntent, RiskContext, bool); 5] = [
        (
            "safety.paused_not_enforced",
            "执行器暂停时的新开仓必须被拒绝",
            base(now),
            context(limits, now, Decimal::ZERO, true),
            false,
        ),
        (
            "safety.stale_quote_not_enforced",
            "行情过期时的新开仓必须被拒绝",
            base(now - Duration::seconds(limits.max_quote_age_secs + 30)),
            context(limits, now, Decimal::ZERO, false),
            false,
        ),
        (
            "safety.total_exposure_not_enforced",
            "已有敞口占满总额度时的新开仓必须被拒绝（资金不足场景）",
            base(now),
            context(limits, now, limits.max_total_notional_usd, false),
            false,
        ),
        (
            "safety.duplicate_leg_not_enforced",
            "同一交易所同一方向的重复腿必须被拒绝",
            {
                let mut intent = base(now);
                intent.legs.push(intent.legs[0].clone());
                intent
            },
            context(limits, now, Decimal::ZERO, false),
            false,
        ),
        (
            "safety.close_blocked",
            "只减仓的平仓单必须始终放行（即使金额超过开仓限额）",
            {
                let mut intent = base(now);
                intent.legs[0].reduce_only = true;
                intent.legs[0].quantity =
                    (limits.max_order_notional_usd * Decimal::from(10)) / price;
                intent
            },
            context(limits, now, limits.max_total_notional_usd, false),
            true,
        ),
    ];

    for (code, message, intent, context, expect_approved) in cases {
        let decision = RiskEngine::preflight(&intent, &context);
        if decision.is_approved() != expect_approved {
            issues.push(Issue::error(
                Dimension::Safety,
                code,
                format!(
                    "{message}：网关实际给出 {:?}",
                    match decision {
                        RiskDecision::Approved { .. } => "批准".to_string(),
                        RiskDecision::Rejected { violations } => format!(
                            "拒绝（{}）",
                            violations
                                .iter()
                                .map(|violation| format!("{:?}", violation.code))
                                .collect::<Vec<_>>()
                                .join("/")
                        ),
                    }
                ),
            ));
        }
    }

    // 明确场景覆盖边界，避免把「未覆盖」误当成「已通过」。
    issues.push(Issue::info(
        Dimension::Safety,
        "safety.manual_review",
        "交易所侧部分成交、余额不足拒单、断网后的补单需要在实盘监控里核对，本地校验不覆盖"
            .to_string(),
    ));
}

fn intent(
    rule: &SkillRule,
    limits: &RiskLimits,
    notional: Decimal,
    now: DateTime<Utc>,
) -> OrderIntent {
    let price = Decimal::from(SAMPLE_PRICE);
    let venue = rule
        .steps
        .execution_venues
        .first()
        .cloned()
        .unwrap_or_else(|| "binance".to_string());
    // 现货腿没有杠杆，按 1 倍校验，避免误报「杠杆超限」。
    let leverage = if crate::market::is_spot_venue(&venue) {
        Decimal::ONE
    } else {
        rule.business.leverage
    };
    OrderIntent {
        idempotency_key: "skill-check".to_string(),
        origin: OrderOrigin::Strategy,
        strategy_id: None,
        legs: vec![OrderLegIntent {
            venue,
            symbol: "BTCUSDT".to_string(),
            side: Side::Buy,
            quantity: if price > Decimal::ZERO {
                notional / price
            } else {
                Decimal::ZERO
            },
            limit_price: price,
            reduce_only: false,
        }],
        leverage,
        max_slippage_bps: limits.max_slippage_bps,
        quote_observed_at: now,
        created_at: now,
    }
}

fn context(limits: &RiskLimits, now: DateTime<Utc>, open: Decimal, paused: bool) -> RiskContext {
    RiskContext {
        now,
        limits: limits.clone(),
        open_notional_usd: open,
        paused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{SkillBusinessRules, SkillSteps, SkillTrigger};
    use rust_decimal_macros::dec;

    fn rule() -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            kind: SkillKind::Breakout,
            trigger: SkillTrigger {
                timeframe: "1h".to_string(),
                lookback: 200,
                ..SkillTrigger::default()
            },
            steps: SkillSteps {
                execution_mode: StrategyExecutionMode::Paper,
                execution_venues: vec!["binance".to_string()],
                ..SkillSteps::default()
            },
            business: SkillBusinessRules {
                notional_usd: dec!(100),
                leverage: dec!(2),
                take_profit_pct: dec!(5),
                stop_loss_pct: dec!(3),
                ..SkillBusinessRules::default()
            },
            ..SkillRule::default()
        }
    }

    #[test]
    fn default_rule_has_no_safety_errors() {
        let issues = check(&rule(), &RiskLimits::default());
        assert!(
            !issues
                .iter()
                .any(|issue| issue.severity == super::super::Severity::Error),
            "默认规则不应触发安全问题：{issues:?}"
        );
    }

    #[test]
    fn over_limit_notional_is_rejected_by_gateway() {
        let mut rule = rule();
        rule.steps.execution_mode = StrategyExecutionMode::Real;
        rule.business.notional_usd = dec!(99_999);
        rule.cautions.max_notional_usd = Decimal::ZERO;
        let issues = check(&rule, &RiskLimits::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "safety.order_would_be_rejected"));
    }

    #[test]
    fn real_without_exit_is_a_warning() {
        let mut rule = rule();
        rule.steps.execution_mode = StrategyExecutionMode::Real;
        rule.business.take_profit_pct = Decimal::ZERO;
        rule.business.stop_loss_pct = Decimal::ZERO;
        let issues = check(&rule, &RiskLimits::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "safety.real_without_exit"));
    }

    #[test]
    fn venue_minimum_is_checked_per_order() {
        let mut rule = rule();
        rule.kind = SkillKind::Grid;
        rule.steps.execution_venues = vec!["lighter".to_string()];
        rule.business.notional_usd = dec!(20);
        rule.business.grid_count = 10;
        rule.cautions.max_notional_usd = Decimal::ZERO;
        let issues = check(&rule, &RiskLimits::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "safety.below_venue_minimum"));
    }

    #[test]
    fn spot_venue_with_short_signals_is_a_warning() {
        let mut rule = rule();
        rule.steps.execution_venues = vec!["binance-spot".to_string()];
        rule.steps.direction = SkillDirection::Both;
        let issues = check(&rule, &RiskLimits::default());
        assert!(issues
            .iter()
            .any(|issue| issue.code == "safety.spot_long_only"));
    }

    #[test]
    fn spot_venue_is_checked_at_one_x_leverage() {
        let mut rule = rule();
        rule.steps.execution_venues = vec!["binance-spot".to_string()];
        rule.steps.direction = SkillDirection::Long;
        rule.business.leverage = dec!(5);
        let issues = check(&rule, &RiskLimits::default());
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "safety.order_would_be_rejected"),
            "现货腿不应因杠杆被拒：{issues:?}"
        );
    }
}
