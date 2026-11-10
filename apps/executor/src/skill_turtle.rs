//! 海龟双系统的纯规则计算。
//!
//! 运行期与回测共用这里的判断：入场系统选择、退出通道、整组止损、
//! 金字塔加仓条件与系统一盈利过滤。这里只做纯计算，不读写数据库。

use arb_core::Side;
use rust_decimal::Decimal;

use crate::{listing::ListingTrade, skill::SkillDirection};

/// 四个唐奇安通道：系统一/系统二的入场与退出。
#[derive(Debug, Clone, Copy, Default)]
pub struct TurtleChannels {
    pub entry1: Option<(Decimal, Decimal)>,
    pub entry2: Option<(Decimal, Decimal)>,
    pub exit1: Option<(Decimal, Decimal)>,
    pub exit2: Option<(Decimal, Decimal)>,
}

/// 双系统入场信号：同一根 K 线同时命中时优先系统二。
pub fn entry_signal(
    last: Decimal,
    channels: &TurtleChannels,
    direction: SkillDirection,
    systems: &crate::skill::TurtleSystems,
) -> Option<(Side, u8)> {
    let allows_long = matches!(direction, SkillDirection::Both | SkillDirection::Long);
    let allows_short = matches!(direction, SkillDirection::Both | SkillDirection::Short);
    let signal = |period: Option<(Decimal, Decimal)>| match period {
        Some((high, _)) if allows_long && last > high => Some(Side::Buy),
        Some((_, low)) if allows_short && last < low => Some(Side::Sell),
        _ => None,
    };
    if systems.system2_enabled {
        if let Some(side) = signal(channels.entry2) {
            return Some((side, 2));
        }
    }
    if systems.system1_enabled {
        if let Some(side) = signal(channels.entry1) {
            return Some((side, 1));
        }
    }
    if !systems.system1_enabled && !systems.system2_enabled {
        return signal(channels.entry1).map(|side| (side, 1));
    }
    None
}

/// 反向退出通道：系统一用 10 根，系统二用 20 根。
pub fn exit_signal(side: Side, system: u8, last: Decimal, channels: &TurtleChannels) -> bool {
    let period = if system == 2 {
        channels.exit2
    } else {
        channels.exit1
    };
    match (side, period) {
        (Side::Buy, Some((_, low))) => last < low,
        (Side::Sell, Some((high, _))) => last > high,
        _ => false,
    }
}

/// 整组止损：跟随最新一个 Unit 的入场价外侧 N 倍距离。
pub fn group_stop(
    side: Side,
    latest_entry: Decimal,
    n_at_entry: Decimal,
    atr_multiple: Decimal,
) -> Option<Decimal> {
    if latest_entry <= Decimal::ZERO || n_at_entry <= Decimal::ZERO || atr_multiple <= Decimal::ZERO
    {
        return None;
    }
    let distance = n_at_entry * atr_multiple;
    Some(if side == Side::Buy {
        latest_entry - distance
    } else {
        latest_entry + distance
    })
}

pub fn stop_hit(side: Side, last: Decimal, stop: Decimal) -> bool {
    if side == Side::Buy {
        last <= stop
    } else {
        last >= stop
    }
}

/// 组内最新（unit_index 最大）的 Unit。
pub fn latest_unit(open: &[ListingTrade]) -> Option<&ListingTrade> {
    open.iter().max_by_key(|trade| trade.unit_index)
}

/// 是否满足金字塔加仓：顺势距离达到 0.5N，且 Unit 数与加仓次数未达上限。
#[allow(clippy::too_many_arguments)]
pub fn should_add(
    side: Side,
    last: Decimal,
    latest_entry: Decimal,
    n: Decimal,
    unit_count: u32,
    adds: u32,
    max_units: u32,
    max_adds: u32,
    step_n: Decimal,
) -> bool {
    if n <= Decimal::ZERO || step_n <= Decimal::ZERO {
        return false;
    }
    if unit_count >= max_units || adds >= max_adds {
        return false;
    }
    let distance = n * step_n;
    if side == Side::Buy {
        last >= latest_entry + distance
    } else {
        last <= latest_entry - distance
    }
}

/// 组内已实现盈亏；用于系统一盈利过滤。未平仓记录会被忽略。
pub fn group_pnl(trades: &[ListingTrade]) -> Decimal {
    trades
        .iter()
        .filter(|trade| trade.closed_at.is_some() && trade.quantity > Decimal::ZERO)
        .filter_map(|trade| {
            let close = trade.close_price?;
            let diff = match trade.side {
                Side::Buy => close - trade.entry_price,
                Side::Sell => trade.entry_price - close,
            };
            Some(diff * trade.quantity)
        })
        .sum()
}

/// 系统一上一笔已平仓交易是否盈利；只统计指定规则与交易对。
pub fn last_closed_system1_win(
    trades: &[ListingTrade],
    rule_id: uuid::Uuid,
    symbol: &str,
) -> Option<bool> {
    let last = trades
        .iter()
        .filter(|trade| {
            trade.system == Some(1)
                && trade.rule_id == Some(rule_id)
                && trade.symbol.eq_ignore_ascii_case(symbol)
                && trade.closed_at.is_some()
        })
        .max_by_key(|trade| trade.closed_at)?;
    let group: Vec<ListingTrade> = match last.group_id {
        Some(group_id) => trades
            .iter()
            .filter(|trade| trade.group_id == Some(group_id))
            .cloned()
            .collect(),
        None => vec![last.clone()],
    };
    Some(group_pnl(&group) > Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::skill::TurtleSystems;

    fn channels() -> TurtleChannels {
        TurtleChannels {
            entry1: Some((Decimal::from(100), Decimal::from(90))),
            entry2: Some((Decimal::from(110), Decimal::from(80))),
            exit1: Some((Decimal::from(105), Decimal::from(95))),
            exit2: Some((Decimal::from(115), Decimal::from(85))),
        }
    }

    #[test]
    fn system_two_takes_priority() {
        let systems = TurtleSystems::default();
        let signal = entry_signal(
            Decimal::from(115),
            &channels(),
            SkillDirection::Both,
            &systems,
        );
        assert_eq!(signal, Some((Side::Buy, 2)));
    }

    #[test]
    fn system_one_signal_when_system_two_misses() {
        let systems = TurtleSystems::default();
        let signal = entry_signal(
            Decimal::from(101),
            &channels(),
            SkillDirection::Both,
            &systems,
        );
        assert_eq!(signal, Some((Side::Buy, 1)));
    }

    #[test]
    fn exit_uses_system_specific_channel() {
        let channels = channels();
        assert!(exit_signal(Side::Buy, 1, Decimal::from(94), &channels));
        assert!(!exit_signal(Side::Buy, 2, Decimal::from(94), &channels));
    }

    #[test]
    fn stop_follows_latest_entry() {
        let stop = group_stop(
            Side::Buy,
            Decimal::new(1025, 1),
            Decimal::from(5),
            Decimal::from(2),
        );
        assert_eq!(stop, Some(Decimal::new(925, 1)));
    }

    #[test]
    fn add_requires_half_n_move_and_room() {
        assert!(should_add(
            Side::Buy,
            Decimal::from(103),
            Decimal::from(100),
            Decimal::from(5),
            1,
            0,
            4,
            3,
            Decimal::new(5, 1),
        ));
        assert!(!should_add(
            Side::Buy,
            Decimal::from(102),
            Decimal::from(100),
            Decimal::from(5),
            4,
            3,
            4,
            3,
            Decimal::new(5, 1),
        ));
    }

    #[test]
    fn closed_group_pnl_counts_all_units() {
        let now = Utc::now();
        let group_id = Uuid::new_v4();
        let rule_id = Uuid::new_v4();
        let trade = |entry: i64, close: i64| ListingTrade {
            id: Uuid::new_v4(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            token: "BTC".to_string(),
            headline: String::new(),
            side: Side::Buy,
            status: crate::listing::ListingStatus::Closed,
            notional_usd: Decimal::from(100),
            leverage: Decimal::from(3),
            quantity: Decimal::from(1),
            entry_price: Decimal::from(entry),
            close_price: Some(Decimal::from(close)),
            entry_order_id: None,
            close_order_id: None,
            paper: true,
            skill: true,
            max_hold_secs: 0,
            notify: false,
            take_profit_pct: Decimal::ZERO,
            stop_loss_pct: Decimal::ZERO,
            strategy_id: Some(Uuid::new_v4()),
            rule_id: Some(rule_id),
            source: None,
            system: Some(1),
            group_id: Some(group_id),
            unit_index: 0,
            n_at_entry: Some(Decimal::from(5)),
            note: None,
            opened_at: now,
            closed_at: Some(now),
            updated_at: now,
        };
        let trades = vec![trade(100, 110), trade(105, 110)];
        assert_eq!(group_pnl(&trades), Decimal::from(15));
        assert_eq!(
            last_closed_system1_win(&trades, rule_id, "BTCUSDT"),
            Some(true)
        );
    }
}
