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
    MaEntry, MaExit, SkillAction, SkillDirection, SkillKind, SkillRule, MAX_EVENT_WINDOW_SECS,
    MAX_GRID_LEVELS, MIN_EVENT_WINDOW_SECS,
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
    for period in &trigger.ma_periods {
        if !(MIN_PERIOD..=MAX_PERIOD).contains(period) {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.ma_period_range",
                format!("均线周期 {period} 超出 {MIN_PERIOD}..={MAX_PERIOD}"),
            ));
        }
    }
    if rule.kind == SkillKind::MaTrend && trigger.ma_periods.is_empty() {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.ma_periods_defaulted",
            "未配置均线周期集合，运行期会回落到 fast / slow 两条均线",
        ));
    }
    let longest = trigger
        .slow
        .max(trigger.channel)
        .max(trigger.atr.saturating_add(1))
        .max(trigger.fast)
        .max(trigger.ma_periods.iter().copied().max().unwrap_or(0));
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
    if rule.kind == SkillKind::MaTrend
        && trigger.fast >= trigger.slow
        && trigger.ma_periods.is_empty()
    {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.fast_not_below_slow",
            format!(
                "未配置均线周期集合时要求快线周期小于慢线，当前 fast={} slow={}，信号会退化为恒定方向",
                trigger.fast, trigger.slow
            ),
        ));
    }
    if rule.kind == SkillKind::MaTrend {
        check_ma_conditions(rule, issues);
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

/// 均线入场/出场条件必须引用已配置的周期，否则指标层拿不到均线，信号永远不会出现。
fn check_ma_conditions(rule: &SkillRule, issues: &mut Vec<Issue>) {
    let trigger = &rule.trigger;
    let available = crate::skill::resolved_ma_periods(trigger);
    if trigger.entries.is_empty() {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.ma_entries_defaulted",
            "未配置均线入场条件，运行期回落到快慢均线的旧逻辑",
        ));
    }
    for entry in &trigger.entries {
        match entry {
            MaEntry::Alignment { periods } => {
                let mut used: Vec<u32> = periods
                    .iter()
                    .copied()
                    .filter(|p| *p >= MIN_PERIOD)
                    .collect();
                used.sort_unstable();
                used.dedup();
                if used.len() < 2 {
                    issues.push(Issue::error(
                        Dimension::Rules,
                        "rules.ma_alignment_periods",
                        "均线排列至少需要两个不同的均线周期",
                    ));
                }
                for period in used {
                    if !available.contains(&period) {
                        issues.push(Issue::error(
                            Dimension::Rules,
                            "rules.ma_period_undefined",
                            format!("入场条件引用了未计算的 MA{period}，请把它加入均线周期集合"),
                        ));
                    }
                }
            }
            MaEntry::PriceAbove { ma } | MaEntry::Pullback { ma } => {
                if !available.contains(ma) {
                    issues.push(Issue::error(
                        Dimension::Rules,
                        "rules.ma_period_undefined",
                        format!("入场条件引用了未计算的 MA{ma}，请把它加入均线周期集合"),
                    ));
                }
            }
        }
    }
    if trigger.exits.is_empty() {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.ma_exits_defaulted",
            "未配置均线出场条件，运行期回落到均线反转逻辑",
        ));
    }
    for exit in &trigger.exits {
        match exit {
            MaExit::Cross { fast, slow } => {
                for period in [fast, slow] {
                    if !available.contains(period) {
                        issues.push(Issue::error(
                            Dimension::Rules,
                            "rules.ma_period_undefined",
                            format!("出场条件引用了未计算的 MA{period}，请把它加入均线周期集合"),
                        ));
                    }
                }
            }
            MaExit::BreakMa { ma } => {
                if !available.contains(ma) {
                    issues.push(Issue::error(
                        Dimension::Rules,
                        "rules.ma_period_undefined",
                        format!("出场条件引用了未计算的 MA{ma}，请把它加入均线周期集合"),
                    ));
                }
            }
            MaExit::AtrStop => {
                if trigger.atr_multiple <= Decimal::ZERO {
                    issues.push(Issue::warning(
                        Dimension::Rules,
                        "rules.atr_stop_disabled",
                        "出场条件包含 ATR 止损，但 ATR 倍数为 0，该条件不会生效",
                    ));
                }
            }
        }
    }
}

fn check_turtle(rule: &SkillRule, limits: &RiskLimits, issues: &mut Vec<Issue>) {
    let turtle = &rule.trigger.turtle;
    let business = &rule.business;
    let systems = [
        (1, turtle.system1_enabled, turtle.entry1, turtle.exit1),
        (2, turtle.system2_enabled, turtle.entry2, turtle.exit2),
    ];
    let mut enabled = 0;
    for (index, on, entry, exit) in systems {
        if !on {
            continue;
        }
        enabled += 1;
        if entry < 2 || entry > 1000 {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.turtle_entry_range",
                format!("系统{index} 入场周期 {entry} 超出 2..=1000"),
            ));
        }
        if exit < 2 || exit > 1000 {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.turtle_exit_range",
                format!("系统{index} 退出周期 {exit} 超出 2..=1000"),
            ));
        }
        if entry <= exit {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.turtle_entry_exit_order",
                format!("系统{index} 入场周期 {entry} 必须大于退出周期 {exit}"),
            ));
        }
    }
    if enabled == 0 {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.turtle_no_system",
            "两个海龟系统都未启用，回测与运行期不会产生新的突破信号",
        ));
    }
    if !(1..=20).contains(&business.max_units) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_max_units_range",
            format!("最大 Unit 数 {} 超出 1..=20", business.max_units),
        ));
    }
    if business.max_pyramid_adds >= business.max_units {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_adds_above_units",
            format!(
                "最大加仓次数 {} 必须小于最大 Unit 数 {}",
                business.max_pyramid_adds, business.max_units
            ),
        ));
    }
    if business.pyramid_step_n <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_step_non_positive",
            "加仓间距必须大于 0",
        ));
    }
    if business.max_leverage <= Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_leverage_non_positive",
            "海龟最大杠杆必须大于 0",
        ));
    } else if business.max_leverage > limits.max_leverage {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_leverage_above_limit",
            format!(
                "海龟最大杠杆 {} 超过执行器上限 {}，运行期会被风控拒绝",
                business.max_leverage, limits.max_leverage
            ),
        ));
    }
    if business.max_symbols == 0 {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.turtle_max_symbols_unlimited",
            "最大持仓币种数为 0，海龟规则会同时持有任意数量的币种",
        ));
    }
    if business.max_hold_hours < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.turtle_hold_negative",
            "最长持有时间不能为负数",
        ));
    }
    if rule.steps.order_type == crate::skill::SkillOrderType::Limit {
        issues.push(Issue::info(
            Dimension::Rules,
            "rules.limit_order_market_execution",
            "订单类型选择了限价，但当前执行链路仍按市价成交",
        ));
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
    if business.risk_reward < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.risk_reward_negative",
            "风险回报比不能为负数",
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
            || business.grid_count != 0
            || business.grid_lower_price != Decimal::ZERO
            || business.grid_upper_price != Decimal::ZERO
            || business.grid_trigger_price != Decimal::ZERO
            || business.grid_spacing_mode != crate::skill::GridSpacingMode::Arithmetic
            || business.grid_shift != crate::skill::GridShift::None
            || business.grid_close_on_fail
        {
            issues.push(Issue::info(
                Dimension::Rules,
                "rules.unused_grid_fields",
                "非网格技能不会读取网格区间、数量与跟随等参数",
            ));
        }
    }

    if rule.kind == SkillKind::Breakout {
        check_turtle(rule, limits, issues);
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

    if rule.cautions.max_daily_opens == 0
        && rule.cautions.max_positions == 0
        && rule.cautions.cooldown_hours <= Decimal::ZERO
    {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.no_cooldown_or_cap",
            "没有每日开仓上限、持仓上限或冷却时间，同一信号会反复开仓",
        ));
    }
}

fn check_grid(rule: &SkillRule, effective_notional: Decimal, issues: &mut Vec<Issue>) {
    let business = &rule.business;
    let count = business.grid_count();
    if business.grid_count > 0 && !(2..=MAX_GRID_LEVELS).contains(&business.grid_count) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_count_range",
            format!(
                "网格数量 {} 超出 2..={MAX_GRID_LEVELS}",
                business.grid_count
            ),
        ));
    } else if count < 2 {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_count_range",
            "网格数量至少为 2，否则无法形成买卖档位",
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
    } else if business.grid_lower_price <= Decimal::ZERO
        && business.grid_upper_price <= Decimal::ZERO
        && business.grid_spacing_pct * Decimal::from(count) / Decimal::from(2) >= Decimal::from(100)
    {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.grid_spacing_negative_price",
            format!(
                "间距 {}% × {} 档的一半 ≥ 100%，自动推导的最远买档价格会变成 0 或负数",
                business.grid_spacing_pct, count
            ),
        ));
    }
    let range_set =
        business.grid_lower_price > Decimal::ZERO || business.grid_upper_price > Decimal::ZERO;
    if range_set {
        if business.grid_lower_price <= Decimal::ZERO || business.grid_upper_price <= Decimal::ZERO
        {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.grid_range_incomplete",
                "价格区间需要同时填写下限与上限",
            ));
        } else if business.grid_upper_price <= business.grid_lower_price {
            issues.push(Issue::error(
                Dimension::Rules,
                "rules.grid_range_order",
                format!(
                    "价格区间上限 {} 必须大于下限 {}",
                    business.grid_upper_price, business.grid_lower_price
                ),
            ));
        }
    }
    if business.grid_trigger_price > Decimal::ZERO
        && range_set
        && (business.grid_trigger_price < business.grid_lower_price
            || business.grid_trigger_price > business.grid_upper_price)
    {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.grid_trigger_outside_range",
            format!(
                "触发价 {} 不在价格区间 {} - {} 内，网格可能永远不会启动",
                business.grid_trigger_price, business.grid_lower_price, business.grid_upper_price
            ),
        ));
    }
    let per_level = effective_notional * business.grid_share();
    for venue in &rule.steps.execution_venues {
        if let Some(minimum) = venue_min_notional_usd(venue) {
            if per_level < minimum {
                issues.push(Issue::error(
                    Dimension::Rules,
                    "rules.grid_below_min_notional",
                    format!("{venue} 每档金额 {per_level} 低于交易所最小下单额 {minimum}"),
                ));
            }
        }
    }
    if business.take_profit_pct <= Decimal::ZERO && business.stop_loss_pct <= Decimal::ZERO {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.grid_no_exit",
            "整组止盈与止损都未设置，单边行情会持续持有浮亏档位",
        ));
    } else if business.stop_loss_pct <= Decimal::ZERO {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.grid_no_stop",
            "网格没有整组止损，单边行情会持续持有浮亏档位",
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
    if !(Decimal::ZERO..=Decimal::from(100)).contains(&cautions.risk_pct) {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.risk_pct_range",
            "单笔风险比例必须在 0..=100 之间（1 表示账户权益的 1%）",
        ));
    } else if cautions.risk_pct > Decimal::ZERO && rule.trigger.atr_multiple <= Decimal::ZERO {
        issues.push(Issue::warning(
            Dimension::Rules,
            "rules.risk_pct_without_stop",
            "配置了单笔风险比例，但没有 ATR 止损距离（atr_multiple=0），运行期会回落到固定金额",
        ));
    }
    if cautions.cooldown_hours < Decimal::ZERO {
        issues.push(Issue::error(
            Dimension::Rules,
            "rules.cooldown_negative",
            "冷却时间不能为负数",
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
                ..crate::skill::SkillSteps::default()
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
            error_codes(&check(&rule, &limits())).contains(&"rules.indicator_window_not_covered")
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
        rule.business.grid_count = 20;
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.grid_below_min_notional"));
    }

    #[test]
    fn event_short_direction_is_rejected() {
        let mut rule = breakout();
        rule.kind = SkillKind::Event;
        rule.steps.direction = SkillDirection::Short;
        assert!(error_codes(&check(&rule, &limits())).contains(&"rules.event_direction_long_only"));
    }

    #[test]
    fn event_without_exit_is_a_warning() {
        let mut rule = breakout();
        rule.kind = SkillKind::Event;
        rule.business.take_profit_pct = Decimal::ZERO;
        rule.business.stop_loss_pct = Decimal::ZERO;
        let issues = check(&rule, &limits());
        assert!(issues.iter().any(|issue| issue.code == "rules.no_exit"
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
