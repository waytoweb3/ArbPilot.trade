//! 第三维：策略计算校验。
//!
//! 这里用一份**独立实现**重算指标、信号、网格档位、ATR 止损、盈亏与仓位，
//! 再与运行期实现逐项比对。独立实现只求口径正确、便于手算，不与运行期共享代码，
//! 否则实现有 bug 时两边会一起错、校验也就失去意义。

use arb_core::{indicators, DataRules, SandboxCandle, Side};
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use super::{Dimension, Issue};
use crate::skill::{self, SkillDirection, SkillRule};

/// 指标比对允许的误差：均线与 ATR 都涉及除法，保留 8 位小数后比对。
fn tolerance() -> Decimal {
    Decimal::new(1, 8)
}

fn close_enough(left: Decimal, right: Decimal) -> bool {
    (left - right).abs() <= tolerance()
}

pub fn check(rule: &SkillRule) -> Vec<Issue> {
    let mut issues = Vec::new();
    check_indicators(&mut issues);
    check_signals(&mut issues);
    check_grid(&mut issues);
    check_stop(&mut issues);
    check_pnl(&mut issues);
    check_notional(rule, &mut issues);
    issues
}

fn report(
    issues: &mut Vec<Issue>,
    code: &'static str,
    name: &str,
    runtime: impl std::fmt::Display,
    expected: impl std::fmt::Display,
) {
    issues.push(Issue::error(
        Dimension::Math,
        code,
        format!("{name}计算与独立实现不一致：运行期 {runtime}，独立实现 {expected}"),
    ));
}

/// 指标独立实现比对。
fn check_indicators(issues: &mut Vec<Issue>) {
    let candles = fixture();
    let rules = DataRules {
        timeframe: "1h".to_string(),
        lookback: 100,
        fast: 3,
        slow: 5,
        atr: 3,
        channel: 4,
        ..DataRules::default()
    };
    let runtime = indicators::compute(&candles, &rules);
    let closes: Vec<Decimal> = candles.iter().map(|candle| candle.close).collect();

    for (name, code, runtime_value, expected) in [
        (
            "快均线",
            "math.sma_fast_mismatch",
            runtime.sma_fast,
            sma(&closes, 3),
        ),
        (
            "慢均线",
            "math.sma_slow_mismatch",
            runtime.sma_slow,
            sma(&closes, 5),
        ),
        (
            "快 EMA",
            "math.ema_fast_mismatch",
            runtime.ema_fast,
            ema(&closes, 3),
        ),
        (
            "慢 EMA",
            "math.ema_slow_mismatch",
            runtime.ema_slow,
            ema(&closes, 5),
        ),
        ("ATR", "math.atr_mismatch", runtime.atr, atr(&candles, 3)),
    ] {
        match (runtime_value, expected) {
            (Some(actual), Some(expected)) if !close_enough(actual, expected) => {
                report(issues, code, name, actual, expected);
            }
            (Some(_), None) | (None, Some(_)) => {
                report(
                    issues,
                    code,
                    name,
                    runtime_value.map(|value| value.to_string()).unwrap_or_default(),
                    expected.map(|value| value.to_string()).unwrap_or_default(),
                );
            }
            _ => {}
        }
    }

    let channel = donchian(&candles, 4);
    match (runtime.channel_high, runtime.channel_low, channel) {
        (Some(high), Some(low), Some((expected_high, expected_low))) => {
            if !close_enough(high, expected_high) {
                report(issues, "math.channel_high_mismatch", "通道上轨", high, expected_high);
            }
            if !close_enough(low, expected_low) {
                report(issues, "math.channel_low_mismatch", "通道下轨", low, expected_low);
            }
        }
        (None, None, Some(_)) => issues.push(Issue::error(
            Dimension::Math,
            "math.channel_mismatch",
            "独立实现能算出通道，运行期却返回空值（K 线根数可能不足）".to_string(),
        )),
        (Some(high), Some(low), None) => issues.push(Issue::error(
            Dimension::Math,
            "math.channel_mismatch",
            format!("运行期返回通道 {high}/{low}，独立实现无法计算（周期小于 2）"),
        )),
        _ => {}
    }
}

/// 信号独立校验：用固定输入核对方向限制与边界（等于轨道不算突破）。
fn check_signals(issues: &mut Vec<Issue>) {
    let cases: [(Decimal, Decimal, Decimal, SkillDirection, Option<Side>, &str); 6] = [
        (Decimal::from(105), Decimal::from(104), Decimal::from(96), SkillDirection::Both, Some(Side::Buy), "突破上轨应做多"),
        (Decimal::from(95), Decimal::from(104), Decimal::from(96), SkillDirection::Both, Some(Side::Sell), "跌破下轨应做空"),
        (Decimal::from(104), Decimal::from(104), Decimal::from(96), SkillDirection::Both, None, "等于上轨不应触发"),
        (Decimal::from(96), Decimal::from(104), Decimal::from(96), SkillDirection::Both, None, "等于下轨不应触发"),
        (Decimal::from(95), Decimal::from(104), Decimal::from(96), SkillDirection::Long, None, "只做多时不应做空"),
        (Decimal::from(105), Decimal::from(104), Decimal::from(96), SkillDirection::Short, None, "只做空时不应做多"),
    ];
    for (last, high, low, direction, expected, name) in cases {
        let actual = skill::breakout_signal(last, high, low, direction);
        if actual != expected {
            issues.push(Issue::error(
                Dimension::Math,
                "math.breakout_signal_mismatch",
                format!("{name}：运行期返回 {actual:?}，独立实现 {expected:?}"),
            ));
        }
    }

    let trend_cases: [(Decimal, Decimal, Decimal, SkillDirection, Option<Side>, &str); 5] = [
        (Decimal::from(103), Decimal::from(102), Decimal::from(100), SkillDirection::Both, Some(Side::Buy), "快线在上且价格站上快线应做多"),
        (Decimal::from(101), Decimal::from(102), Decimal::from(100), SkillDirection::Both, None, "价格未站上快线不应做多"),
        (Decimal::from(97), Decimal::from(98), Decimal::from(100), SkillDirection::Both, Some(Side::Sell), "快线在下且价格跌破快线应做空"),
        (Decimal::from(97), Decimal::from(98), Decimal::from(100), SkillDirection::Long, None, "只做多时不应做空"),
        (Decimal::from(103), Decimal::ZERO, Decimal::from(100), SkillDirection::Both, None, "快线为 0 时应拒绝信号"),
    ];
    for (last, fast, slow, direction, expected, name) in trend_cases {
        let actual = skill::ma_trend_signal(last, fast, slow, direction);
        if actual != expected {
            issues.push(Issue::error(
                Dimension::Math,
                "math.ma_trend_signal_mismatch",
                format!("{name}：运行期返回 {actual:?}，独立实现 {expected:?}"),
            ));
        }
    }
}

/// 网格档位比对：锚定 100、3 档、0.5% 间距应为 99.5/99/98.5 与 100.5/101/101.5。
fn check_grid(issues: &mut Vec<Issue>) {
    let (buys, sells) = skill::grid_levels(Decimal::from(100), 3, Decimal::new(5, 1));
    let expected_buys = [Decimal::from(995) / Decimal::from(10), Decimal::from(99), Decimal::new(985, 1)];
    let expected_sells = [
        Decimal::from(1005) / Decimal::from(10),
        Decimal::from(101),
        Decimal::new(1015, 1),
    ];
    if buys != expected_buys || sells != expected_sells {
        issues.push(Issue::error(
            Dimension::Math,
            "math.grid_levels_mismatch",
            format!(
                "网格档位与独立实现不一致：运行期买 {buys:?} / 卖 {sells:?}，独立实现买 {expected_buys:?} / 卖 {expected_sells:?}"
            ),
        ));
    }

    let (zero_buys, zero_sells) = skill::grid_levels(Decimal::from(100), 3, Decimal::ZERO);
    if !zero_buys.is_empty() || !zero_sells.is_empty() {
        issues.push(Issue::error(
            Dimension::Math,
            "math.grid_zero_spacing",
            "间距为 0 时不应产生任何档位".to_string(),
        ));
    }
}

/// ATR 止损比对：做多距离为 `ATR × 倍数`，恰好触及时应离场。
fn check_stop(issues: &mut Vec<Issue>) {
    let atr = Some(Decimal::from(2));
    let cases: [(Side, Decimal, Decimal, Option<Decimal>, Decimal, bool, &str); 6] = [
        (Side::Buy, Decimal::from(100), Decimal::from(96), atr, Decimal::from(2), true, "做多触及 2×ATR 应止损"),
        (Side::Buy, Decimal::from(100), Decimal::new(961, 1), atr, Decimal::from(2), false, "做多未触及止损价不应离场"),
        (Side::Buy, Decimal::from(100), Decimal::from(90), None, Decimal::from(2), false, "ATR 缺失时不应止损"),
        (Side::Sell, Decimal::from(100), Decimal::from(104), atr, Decimal::from(2), true, "做空触及 2×ATR 应止损"),
        (Side::Sell, Decimal::from(100), Decimal::new(1039, 1), atr, Decimal::from(2), false, "做空未触及止损价不应离场"),
        (Side::Buy, Decimal::from(100), Decimal::from(90), atr, Decimal::ZERO, false, "倍数为 0 时不应止损"),
    ];
    for (side, entry, last, atr, multiple, expected, name) in cases {
        let actual = skill::atr_stop_hit(side, entry, last, atr, multiple);
        if actual != expected {
            issues.push(Issue::error(
                Dimension::Math,
                "math.atr_stop_mismatch",
                format!("{name}：运行期返回 {actual}，独立实现 {expected}"),
            ));
        }
    }
}

/// 盈亏比对：做多价格上涨为正、做空价格上涨为负，百分数保留两位。
fn check_pnl(issues: &mut Vec<Issue>) {
    let cases = [
        (Side::Buy, Decimal::from(100), Decimal::from(110), Decimal::from(10)),
        (Side::Buy, Decimal::from(100), Decimal::from(90), Decimal::from(-10)),
        (Side::Sell, Decimal::from(100), Decimal::from(110), Decimal::from(-10)),
        (Side::Sell, Decimal::from(100), Decimal::from(90), Decimal::from(10)),
        (Side::Buy, Decimal::ZERO, Decimal::from(110), Decimal::ZERO),
    ];
    for (side, entry, price, expected) in cases {
        let actual = crate::listing::pnl_pct(side, entry, price);
        if actual != expected {
            issues.push(Issue::error(
                Dimension::Math,
                "math.pnl_mismatch",
                format!("{side:?} 入场 {entry} 出场 {price}：运行期 {actual}%，独立实现 {expected}%"),
            ));
        }
    }
}

/// 有效下单金额比对：业务金额与注意事项上限取小。
fn check_notional(rule: &SkillRule, issues: &mut Vec<Issue>) {
    let expected = if rule.cautions.max_notional_usd > Decimal::ZERO {
        rule.business.notional_usd.min(rule.cautions.max_notional_usd)
    } else {
        rule.business.notional_usd
    };
    let actual = skill::capped_notional(rule);
    if actual != expected {
        issues.push(Issue::error(
            Dimension::Math,
            "math.notional_cap_mismatch",
            format!("有效下单金额：运行期 {actual}，独立实现 {expected}"),
        ));
    }
}

fn fixture() -> Vec<SandboxCandle> {
    (0..40i64)
        .map(|index| {
            let base = Decimal::from(100 + (index % 7) * 3 + index);
            SandboxCandle {
                open_time: Utc.timestamp_opt(index * 3_600, 0).single().expect("合法时间"),
                open: base,
                high: base + Decimal::from(2),
                low: base - Decimal::from(2),
                close: base,
                volume: Decimal::ONE,
            }
        })
        .collect()
}

fn sma(closes: &[Decimal], period: u32) -> Option<Decimal> {
    if period < 2 {
        return None;
    }
    let period = period as usize;
    let start = closes.len().checked_sub(period)?;
    Some(closes[start..].iter().sum::<Decimal>() / Decimal::from(period as u32))
}

fn ema(closes: &[Decimal], period: u32) -> Option<Decimal> {
    if period < 2 {
        return None;
    }
    let period = period as usize;
    if closes.len() < period {
        return None;
    }
    let divisor = Decimal::from(period as u32);
    let weight = Decimal::from(2) / (divisor + Decimal::ONE);
    let mut value = closes[..period].iter().sum::<Decimal>() / divisor;
    for close in &closes[period..] {
        value = (*close - value) * weight + value;
    }
    Some(value)
}

fn true_range(candle: &SandboxCandle, previous_close: Decimal) -> Decimal {
    (candle.high - candle.low)
        .max((candle.high - previous_close).abs())
        .max((candle.low - previous_close).abs())
}

fn atr(candles: &[SandboxCandle], period: u32) -> Option<Decimal> {
    if period < 2 {
        return None;
    }
    let period_usize = period as usize;
    if candles.len() <= period_usize {
        return None;
    }
    let ranges: Vec<Decimal> = candles
        .windows(2)
        .map(|pair| true_range(&pair[1], pair[0].close))
        .collect();
    let start = ranges.len().checked_sub(period_usize)?;
    Some(ranges[start..].iter().sum::<Decimal>() / Decimal::from(period))
}

fn donchian(candles: &[SandboxCandle], period: u32) -> Option<(Decimal, Decimal)> {
    if period < 2 {
        return None;
    }
    let historical = candles.get(..candles.len().checked_sub(1)?)?;
    let period = period as usize;
    let start = historical.len().checked_sub(period)?;
    let window = &historical[start..];
    let high = window.iter().map(|candle| candle.high).max()?;
    let low = window.iter().map(|candle| candle.low).min()?;
    Some((high, low))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn rule() -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            kind: crate::skill::SkillKind::Breakout,
            business: crate::skill::SkillBusinessRules {
                notional_usd: dec!(100),
                ..crate::skill::SkillBusinessRules::default()
            },
            ..SkillRule::default()
        }
    }

    #[test]
    fn default_rule_math_is_consistent() {
        let issues = check(&rule());
        assert!(issues.is_empty(), "默认突破规则计算应一致：{issues:?}");
    }

    #[test]
    fn independent_indicators_match_runtime() {
        let candles = fixture();
        let rules = DataRules {
            fast: 3,
            slow: 5,
            atr: 3,
            channel: 4,
            ..DataRules::default()
        };
        let runtime = indicators::compute(&candles, &rules);
        let closes: Vec<Decimal> = candles.iter().map(|candle| candle.close).collect();
        assert_eq!(runtime.sma_fast, sma(&closes, 3));
        assert_eq!(runtime.sma_slow, sma(&closes, 5));
        assert_eq!(runtime.ema_fast, ema(&closes, 3));
        assert_eq!(runtime.atr, atr(&candles, 3));
        assert_eq!(
            (runtime.channel_high, runtime.channel_low),
            donchian(&candles, 4).map(|(high, low)| (Some(high), Some(low))).unwrap_or((None, None))
        );
    }

    #[test]
    fn grid_levels_match_manual_calculation() {
        let (buys, sells) = skill::grid_levels(dec!(100), 3, dec!(0.5));
        assert_eq!(buys, vec![dec!(99.5), dec!(99), dec!(98.5)]);
        assert_eq!(sells, vec![dec!(100.5), dec!(101), dec!(101.5)]);
        let (buys, sells) = skill::grid_levels(dec!(100), 3, Decimal::ZERO);
        assert!(buys.is_empty() && sells.is_empty());
    }

    #[test]
    fn atr_stop_triggers_exactly_at_distance() {
        assert!(skill::atr_stop_hit(
            Side::Buy,
            dec!(100),
            dec!(96),
            Some(dec!(2)),
            dec!(2)
        ));
        assert!(!skill::atr_stop_hit(
            Side::Buy,
            dec!(100),
            dec!(96.1),
            Some(dec!(2)),
            dec!(2)
        ));
    }

    #[test]
    fn pnl_sign_follows_side() {
        assert_eq!(crate::listing::pnl_pct(Side::Buy, dec!(100), dec!(110)), dec!(10));
        assert_eq!(
            crate::listing::pnl_pct(Side::Sell, dec!(100), dec!(110)),
            dec!(-10)
        );
    }
}
