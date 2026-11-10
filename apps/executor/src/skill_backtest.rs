//! 技能策略回测：按 SKILL.md 六章参数逐根 K 线重放。
//!
//! 技能策略是 `StrategyKind::Custom`，但它没有沙箱模块（`sandbox = None`），
//! 因此不能走 `wasm_backtest`。这里用与运行期一致的规则做历史回放：
//!
//! - 突破 / 均线：每根 K 线只用「当前及之前」的窗口计算指标，收盘价成交；
//! - 网格：在价格区间内按等差或等比布置档位，K 线高低点触及档位即成交，到相邻档位止盈；
//! - 公告类：历史公告不可回放，直接跳过并在假设里标注 `NoEventReplay`。
//!
//! 费用与滑点按开平仓两次计入，口径与 `wasm_backtest` 保持一致：
//! `net = gross - fees - slippage`。

use arb_core::{
    indicators, BacktestAssumption, BacktestReport, DataRules, EquityPoint, SandboxCandle,
    SandboxIndicators, Side, StrategyRecord,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::{
    skill::{self, GridShift, SkillDirection, SkillKind, SkillRule},
    skill_turtle,
    state::AppState,
};

/// 回测成本：单边手续费与滑点（bps）。
#[derive(Debug, Clone, Copy)]
pub struct Costs {
    pub fee_bps_per_leg: Decimal,
    pub slippage_bps: Decimal,
}

impl Default for Costs {
    fn default() -> Self {
        Self {
            fee_bps_per_leg: Decimal::from(4),
            slippage_bps: Decimal::ONE,
        }
    }
}

impl Costs {
    fn fee_rate(self) -> Decimal {
        self.fee_bps_per_leg / Decimal::from(10_000)
    }

    fn slippage_rate(self) -> Decimal {
        self.slippage_bps / Decimal::from(10_000)
    }
}

/// 一笔回放交易。
#[derive(Debug, Clone)]
pub struct ReplayTrade {
    pub symbol: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub entry_price: Decimal,
    /// 未扣成本的毛盈亏。
    pub gross: Decimal,
    pub fees: Decimal,
    pub slippage: Decimal,
    /// 扣掉手续费与滑点后的净盈亏。
    pub pnl: Decimal,
}

/// 回放汇总指标。
#[derive(Debug, Clone)]
pub struct Metrics {
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub gross_usd: Decimal,
    pub fees_usd: Decimal,
    pub slippage_usd: Decimal,
    pub net_pnl_usd: Decimal,
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub win_rate_pct: Decimal,
    /// 按自然日折算的交易频率。
    pub trades_per_day: Decimal,
}

struct OpenPosition {
    symbol: String,
    side: Side,
    entry_price: Decimal,
    quantity: Decimal,
    opened_at: DateTime<Utc>,
    take_profit_pct: Decimal,
    stop_loss_pct: Decimal,
}

/// 按技能规则回放一段 K 线。
///
/// `notional` 是单笔名义金额；网格会再按 `grid_share_pct` 折算每档金额。
pub fn replay(
    rule: &SkillRule,
    symbol: &str,
    candles: &[SandboxCandle],
    notional: Decimal,
    costs: Costs,
) -> Vec<ReplayTrade> {
    if notional <= Decimal::ZERO || candles.len() < 3 {
        return Vec::new();
    }
    match rule.kind {
        SkillKind::Event => Vec::new(),
        SkillKind::Grid => replay_grid(rule, symbol, candles, notional, costs),
        SkillKind::Breakout => replay_turtle(
            rule,
            symbol,
            candles,
            notional * Decimal::from(10),
            notional,
            costs,
        ),
        SkillKind::MaTrend => replay_signal(rule, symbol, candles, notional, costs),
    }
}

/// 海龟组的单个 Unit。
struct TurtleUnit {
    side: Side,
    system: u8,
    entry_price: Decimal,
    quantity: Decimal,
    notional: Decimal,
    n: Decimal,
    opened_at: DateTime<Utc>,
}

/// 按经典海龟规则回放：双系统、Unit 风险定仓、金字塔加仓与整组止损。
fn replay_turtle(
    rule: &SkillRule,
    symbol: &str,
    candles: &[SandboxCandle],
    equity: Decimal,
    notional_cap: Decimal,
    costs: Costs,
) -> Vec<ReplayTrade> {
    let atr_multiple = rule.trigger.atr_multiple;
    let turtle = &rule.trigger.turtle;
    let warmup = warmup_bars(rule).max(
        turtle
            .entry1
            .max(turtle.entry2)
            .max(turtle.exit1)
            .max(turtle.exit2) as usize
            + 1,
    );
    if candles.len() <= warmup || equity <= Decimal::ZERO {
        return Vec::new();
    }
    let data = data_rules(rule);
    let mut trades = Vec::new();
    let mut group: Vec<TurtleUnit> = Vec::new();
    let mut last_open_at: Option<DateTime<Utc>> = None;
    let mut last_s1_win: Option<bool> = None;

    for index in warmup..candles.len() {
        let candle = &candles[index];
        let last = candle.close;
        if last <= Decimal::ZERO {
            continue;
        }
        let window = &candles[..=index];
        let indicators = indicators::compute(window, &data);
        let channels = turtle_channels(window, rule);
        let atr = indicators.atr.filter(|value| *value > Decimal::ZERO);

        if !group.is_empty() {
            let latest = group.last().expect("组内至少一个 Unit");
            let stopped =
                skill_turtle::group_stop(latest.side, latest.entry_price, latest.n, atr_multiple)
                    .map(|stop| skill_turtle::stop_hit(latest.side, last, stop))
                    .unwrap_or(false);
            if stopped || skill_turtle::exit_signal(latest.side, latest.system, last, &channels) {
                let system = latest.system;
                let mut group_pnl = Decimal::ZERO;
                for unit in group.drain(..) {
                    let trade = close_turtle_unit(unit, symbol, candle, last, costs);
                    group_pnl += trade.pnl;
                    trades.push(trade);
                }
                if system == 1 {
                    last_s1_win = Some(group_pnl > Decimal::ZERO);
                }
                continue;
            }
            let Some(atr) = atr else {
                continue;
            };
            let latest = group.last().expect("组内至少一个 Unit");
            let units = group.len() as u32;
            let adds = units.saturating_sub(1);
            if skill_turtle::should_add(
                latest.side,
                last,
                latest.entry_price,
                atr,
                units,
                adds,
                rule.business.max_units,
                rule.business.max_pyramid_adds,
                rule.business.pyramid_step_n,
            ) {
                let used: Decimal = group.iter().map(|unit| unit.notional).sum();
                let size = turtle_unit_notional(rule, equity, last, atr, notional_cap);
                let cap = equity * rule.business.max_leverage.max(Decimal::ONE);
                if size > Decimal::ZERO && used + size <= cap {
                    group.push(TurtleUnit {
                        side: latest.side,
                        system: latest.system,
                        entry_price: last,
                        quantity: size / last,
                        notional: size,
                        n: atr,
                        opened_at: candle.open_time,
                    });
                }
            }
            continue;
        }

        let Some((side, system)) =
            skill_turtle::entry_signal(last, &channels, rule.steps.direction, turtle)
        else {
            continue;
        };
        if system == 1 && turtle.profit_filter && last_s1_win == Some(true) {
            continue;
        }
        if in_cooldown(rule, last_open_at, candle.open_time) {
            continue;
        }
        let Some(atr) = atr else {
            continue;
        };
        let size = turtle_unit_notional(rule, equity, last, atr, notional_cap);
        if size <= Decimal::ZERO {
            continue;
        }
        group.push(TurtleUnit {
            side,
            system,
            entry_price: last,
            quantity: size / last,
            notional: size,
            n: atr,
            opened_at: candle.open_time,
        });
        last_open_at = Some(candle.open_time);
    }

    if !group.is_empty() {
        let candle = candles.last().expect("K 线非空");
        for unit in group.drain(..) {
            trades.push(close_turtle_unit(unit, symbol, candle, candle.close, costs));
        }
    }
    trades
}

fn close_turtle_unit(
    unit: TurtleUnit,
    symbol: &str,
    candle: &SandboxCandle,
    exit_price: Decimal,
    costs: Costs,
) -> ReplayTrade {
    close_position(
        OpenPosition {
            symbol: symbol.to_string(),
            side: unit.side,
            entry_price: unit.entry_price,
            quantity: unit.quantity,
            opened_at: unit.opened_at,
            take_profit_pct: Decimal::ZERO,
            stop_loss_pct: Decimal::ZERO,
        },
        candle,
        exit_price,
        costs,
    )
}

/// Unit 名义价值：权益 × 单 Unit 风险 ÷ (止损 N 倍 × N / 价格)。
fn turtle_unit_notional(
    rule: &SkillRule,
    equity: Decimal,
    price: Decimal,
    n: Decimal,
    cap: Decimal,
) -> Decimal {
    if price <= Decimal::ZERO || n <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let multiple = rule.trigger.atr_multiple.max(Decimal::ONE);
    let risk_pct = rule.cautions.risk_pct;
    if risk_pct <= Decimal::ZERO || equity <= Decimal::ZERO {
        return cap;
    }
    let risk_usd = equity * risk_pct / Decimal::from(100);
    let size = risk_usd * price / (n * multiple);
    if cap > Decimal::ZERO {
        size.min(cap)
    } else {
        size
    }
}

fn turtle_channels(candles: &[SandboxCandle], rule: &SkillRule) -> skill_turtle::TurtleChannels {
    let period = |value: u32| {
        if value >= 2 {
            indicators::donchian(candles, value)
        } else {
            None
        }
    };
    let turtle = &rule.trigger.turtle;
    skill_turtle::TurtleChannels {
        entry1: period(turtle.entry1),
        entry2: period(turtle.entry2),
        exit1: period(turtle.exit1),
        exit2: period(turtle.exit2),
    }
}

fn replay_signal(
    rule: &SkillRule,
    symbol: &str,
    candles: &[SandboxCandle],
    notional: Decimal,
    costs: Costs,
) -> Vec<ReplayTrade> {
    let data = data_rules(rule);
    let warmup = warmup_bars(rule);
    if candles.len() <= warmup {
        return Vec::new();
    }
    let mut trades = Vec::new();
    let mut position: Option<OpenPosition> = None;
    let mut last_open_at: Option<DateTime<Utc>> = None;

    for index in warmup..candles.len() {
        let candle = &candles[index];
        let last = candle.close;
        if last <= Decimal::ZERO {
            continue;
        }
        let window = &candles[..=index];
        let indicators = indicators::compute(window, &data);

        if let Some(open) = position.take() {
            let signal_exit = match rule.kind {
                SkillKind::MaTrend => skill::ma_exit_hit(
                    open.side,
                    last,
                    open.entry_price,
                    &indicators,
                    &rule.trigger,
                ),
                _ => exit_by_signal(rule, &open, &indicators, last),
            };
            if signal_exit || exit_by_risk(&open, last) {
                trades.push(close_position(open, candle, last, costs));
            } else {
                position = Some(open);
            }
            continue;
        }

        let signal = match rule.kind {
            SkillKind::Breakout => match (indicators.channel_high, indicators.channel_low) {
                (Some(high), Some(low)) => {
                    skill::breakout_signal(last, high, low, rule.steps.direction)
                }
                _ => None,
            },
            SkillKind::MaTrend => skill::ma_entry_signal(last, &indicators, &rule.trigger)
                .filter(|side| skill::direction_allows(*side, rule.steps.direction)),
            _ => None,
        };
        if let Some(side) = signal {
            if in_cooldown(rule, last_open_at, candle.open_time) {
                continue;
            }
            position = Some(open_position(
                rule,
                symbol,
                side,
                last,
                notional,
                candle.open_time,
                take_profit_pct(rule, last, indicators.atr),
            ));
            last_open_at = Some(candle.open_time);
        }
    }

    if let Some(open) = position {
        let candle = candles.last().expect("K 线非空");
        trades.push(close_position(open, candle, candle.close, costs));
    }
    trades
}

/// 回测里的冷却判断：与运行期一致，按同一交易对的上次开仓时间计算。
fn in_cooldown(rule: &SkillRule, last_open_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(previous) = last_open_at else {
        return false;
    };
    if rule.cautions.cooldown_hours <= Decimal::ZERO {
        return false;
    }
    let seconds = (rule.cautions.cooldown_hours * Decimal::from(3_600))
        .to_i64()
        .unwrap_or(0);
    (now - previous).num_seconds() < seconds
}

fn replay_grid(
    rule: &SkillRule,
    symbol: &str,
    candles: &[SandboxCandle],
    notional: Decimal,
    costs: Costs,
) -> Vec<ReplayTrade> {
    let fallback_spacing = rule.business.grid_spacing_pct;
    let per_level = notional * rule.business.grid_share();
    let anchor = candles
        .first()
        .map(|candle| candle.close)
        .unwrap_or_default();
    if per_level <= Decimal::ZERO || fallback_spacing <= Decimal::ZERO || anchor <= Decimal::ZERO {
        return Vec::new();
    }
    let Some((mut lower, mut upper)) = skill::grid_range(anchor, rule) else {
        return Vec::new();
    };
    let (mut levels, mut sides) = grid_shape(rule, lower, upper, anchor);
    if levels.len() < 2 {
        return Vec::new();
    }
    let mut filled = grid_filled(&sides);
    let mut armed = rule.business.grid_trigger_price <= Decimal::ZERO;
    let trigger = rule.business.grid_trigger_price;
    let mut position: Option<OpenPosition> = None;
    let mut trades = Vec::new();

    for candle in candles.iter() {
        // 触发价：到达后才开始挂档。
        if !armed {
            let reached = if trigger < anchor {
                candle.low <= trigger
            } else {
                candle.high >= trigger
            };
            if !reached {
                continue;
            }
            armed = true;
        }
        // 突破区间时按配置跟随，重建档位。
        let up = matches!(rule.business.grid_shift, GridShift::Up | GridShift::Both)
            && candle.close > upper;
        let down = matches!(rule.business.grid_shift, GridShift::Down | GridShift::Both)
            && candle.close < lower;
        if up || down {
            let delta = if up {
                candle.close - upper
            } else {
                candle.close - lower
            };
            lower += delta;
            upper += delta;
            (levels, sides) = grid_shape(rule, lower, upper, candle.close);
            filled = grid_filled(&sides);
        }
        if levels.len() < 2 {
            return trades;
        }

        if let Some(open) = position.take() {
            if grid_exit(&open, candle) || exit_by_risk(&open, candle.close) {
                let exit = grid_exit_price(&open, candle);
                trades.push(close_position(open, candle, exit, costs));
            } else {
                position = Some(open);
            }
            continue;
        }

        let mut opened = None;
        for (index, level) in levels.iter().enumerate() {
            if filled[index] {
                continue;
            }
            let Some(side) = sides[index] else {
                continue;
            };
            let hit = match side {
                Side::Buy => candle.low <= *level,
                Side::Sell => candle.high >= *level,
            };
            if !hit {
                continue;
            }
            filled[index] = true;
            let take_profit = grid_take_profit(&levels, index, side, fallback_spacing);
            opened = Some((side, *level, take_profit));
            break;
        }

        if let Some((side, price, take_profit)) = opened {
            position = Some(open_position(
                rule,
                symbol,
                side,
                price,
                per_level,
                candle.open_time,
                take_profit,
            ));
        }

        // 所有档位都成交后，按最新收盘价重建区间，模拟运行期的网格滚动。
        if filled.iter().all(|filled| *filled) {
            if let Some((next_lower, next_upper)) = skill::grid_range(candle.close, rule) {
                lower = next_lower;
                upper = next_upper;
                (levels, sides) = grid_shape(rule, lower, upper, candle.close);
                filled = grid_filled(&sides);
            }
        }
    }

    if let Some(open) = position {
        let candle = candles.last().expect("K 线非空");
        let exit = grid_exit_price(&open, candle);
        trades.push(close_position(open, candle, exit, costs));
    }
    trades
}

/// 按当前区间生成档位与方向：区间下方买入、上方卖出，方向设置会裁掉一侧。
fn grid_shape(
    rule: &SkillRule,
    lower: Decimal,
    upper: Decimal,
    anchor: Decimal,
) -> (Vec<Decimal>, Vec<Option<Side>>) {
    let levels = skill::grid_price_levels(
        lower,
        upper,
        rule.business.grid_count(),
        rule.business.grid_spacing_mode,
    );
    let sides = levels
        .iter()
        .map(|level| {
            if *level < anchor && rule.steps.direction != SkillDirection::Short {
                Some(Side::Buy)
            } else if *level > anchor && rule.steps.direction != SkillDirection::Long {
                Some(Side::Sell)
            } else {
                None
            }
        })
        .collect();
    (levels, sides)
}

/// 不可交易的档位直接标记为已处理，保证「全部成交」判断能成立。
fn grid_filled(sides: &[Option<Side>]) -> Vec<bool> {
    sides.iter().map(|side| side.is_none()).collect()
}

/// 到相邻档位的止盈百分比，与运行期的网格止盈口径一致。
fn grid_take_profit(levels: &[Decimal], index: usize, side: Side, fallback: Decimal) -> Decimal {
    let target = match side {
        Side::Buy => levels.get(index + 1),
        Side::Sell => index.checked_sub(1).and_then(|prev| levels.get(prev)),
    };
    match (target, levels.get(index)) {
        (Some(target), Some(level)) if *level > Decimal::ZERO => {
            let change = if side == Side::Buy {
                *target - *level
            } else {
                *level - *target
            };
            (change / *level * Decimal::from(100)).max(Decimal::ZERO)
        }
        _ => fallback,
    }
}

/// 开仓时的止盈百分比：配置了风险回报比就按 ATR 止损距离 × 倍数推导。
fn take_profit_pct(rule: &SkillRule, last: Decimal, atr: Option<Decimal>) -> Decimal {
    let stop_pct = atr
        .filter(|value| *value > Decimal::ZERO)
        .filter(|_| rule.trigger.atr_multiple > Decimal::ZERO)
        .filter(|_| last > Decimal::ZERO)
        .map(|atr| atr * rule.trigger.atr_multiple / last * Decimal::from(100));
    match stop_pct {
        Some(stop_pct) if rule.business.risk_reward > Decimal::ZERO => {
            stop_pct * rule.business.risk_reward
        }
        _ => rule.business.take_profit_pct,
    }
}

fn open_position(
    rule: &SkillRule,
    symbol: &str,
    side: Side,
    price: Decimal,
    notional: Decimal,
    opened_at: DateTime<Utc>,
    take_profit_pct: Decimal,
) -> OpenPosition {
    OpenPosition {
        symbol: symbol.to_string(),
        side,
        entry_price: price,
        quantity: notional / price,
        opened_at,
        take_profit_pct,
        stop_loss_pct: rule.business.stop_loss_pct,
    }
}

fn exit_by_signal(
    rule: &SkillRule,
    open: &OpenPosition,
    indicators: &SandboxIndicators,
    last: Decimal,
) -> bool {
    match rule.kind {
        SkillKind::Breakout => {
            let reverse = match open.side {
                Side::Buy => indicators.channel_low.is_some_and(|low| last < low),
                Side::Sell => indicators.channel_high.is_some_and(|high| last > high),
            };
            reverse
                || skill::atr_stop_hit(
                    open.side,
                    open.entry_price,
                    last,
                    indicators.atr,
                    rule.trigger.atr_multiple,
                )
        }
        _ => false,
    }
}

fn exit_by_risk(open: &OpenPosition, last: Decimal) -> bool {
    let change = crate::listing::pnl_pct(open.side, open.entry_price, last);
    if open.take_profit_pct > Decimal::ZERO && change >= open.take_profit_pct {
        return true;
    }
    if open.stop_loss_pct > Decimal::ZERO && change <= -open.stop_loss_pct {
        return true;
    }
    false
}

fn grid_exit(open: &OpenPosition, candle: &SandboxCandle) -> bool {
    let spacing = open.take_profit_pct / Decimal::from(100);
    if spacing <= Decimal::ZERO {
        return false;
    }
    match open.side {
        Side::Buy => candle.high >= open.entry_price * (Decimal::ONE + spacing),
        Side::Sell => candle.low <= open.entry_price * (Decimal::ONE - spacing),
    }
}

fn grid_exit_price(open: &OpenPosition, candle: &SandboxCandle) -> Decimal {
    let spacing = open.take_profit_pct / Decimal::from(100);
    let target = match open.side {
        Side::Buy => open.entry_price * (Decimal::ONE + spacing),
        Side::Sell => open.entry_price * (Decimal::ONE - spacing),
    };
    // 止盈价在 K 线范围内时按止盈价成交，否则退回收盘价。
    let touched = match open.side {
        Side::Buy => candle.high >= target,
        Side::Sell => candle.low <= target,
    };
    if spacing > Decimal::ZERO && touched {
        target
    } else {
        candle.close
    }
}

fn close_position(
    open: OpenPosition,
    candle: &SandboxCandle,
    exit_price: Decimal,
    costs: Costs,
) -> ReplayTrade {
    let entry_notional = open.quantity * open.entry_price;
    let exit_notional = open.quantity * exit_price;
    let gross = match open.side {
        Side::Buy => (exit_price - open.entry_price) * open.quantity,
        Side::Sell => (open.entry_price - exit_price) * open.quantity,
    };
    let fees = (entry_notional + exit_notional) * costs.fee_rate();
    let slippage = (entry_notional + exit_notional) * costs.slippage_rate();
    ReplayTrade {
        symbol: open.symbol,
        opened_at: open.opened_at,
        closed_at: candle.open_time,
        entry_price: open.entry_price,
        gross,
        fees,
        slippage,
        pnl: gross - fees - slippage,
    }
}

fn data_rules(rule: &SkillRule) -> DataRules {
    DataRules {
        timeframe: rule.trigger.timeframe.clone(),
        lookback: rule.trigger.lookback.clamp(20, 1_000),
        fast: rule.trigger.fast.max(2),
        slow: rule.trigger.slow.max(2),
        ma_periods: skill::resolved_ma_periods(&rule.trigger),
        atr: rule.trigger.atr.max(2),
        channel: rule.trigger.channel.max(2),
        ..DataRules::default()
    }
}

/// 指标全部可用的最少 K 线根数。
fn warmup_bars(rule: &SkillRule) -> usize {
    let trigger = &rule.trigger;
    let longest_ma = trigger
        .ma_periods
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(trigger.fast)
        .max(trigger.slow);
    longest_ma
        .max(trigger.channel.saturating_add(1))
        .max(trigger.atr.saturating_add(1))
        .max(3) as usize
}

fn interval_secs(timeframe: &str) -> i64 {
    match timeframe {
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

/// 汇总交易列表，得到收益、回撤、胜率与交易频率。
pub fn summarize(trades: &[ReplayTrade], notional: Decimal, days: i64) -> Metrics {
    let mut gross = Decimal::ZERO;
    let mut fees = Decimal::ZERO;
    let mut slippage = Decimal::ZERO;
    let mut equity = Decimal::ZERO;
    let mut peak = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    let mut wins = 0usize;

    for trade in trades {
        gross += trade.gross;
        fees += trade.fees;
        slippage += trade.slippage;
        equity += trade.pnl;
        if trade.pnl > Decimal::ZERO {
            wins += 1;
        }
        if equity > peak {
            peak = equity;
        }
        let drawdown = peak - equity;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
    }

    let net = gross - fees - slippage;
    let scale = |value: Decimal| {
        if notional > Decimal::ZERO {
            (value / notional * Decimal::from(100)).round_dp(4)
        } else {
            Decimal::ZERO
        }
    };
    let win_rate = if trades.is_empty() {
        Decimal::ZERO
    } else {
        (Decimal::from(wins) / Decimal::from(trades.len()) * Decimal::from(100)).round_dp(2)
    };
    let trades_per_day = if days > 0 {
        (Decimal::from(trades.len()) / Decimal::from(days)).round_dp(2)
    } else {
        Decimal::ZERO
    };

    Metrics {
        trades: trades.len(),
        wins,
        losses: trades.len().saturating_sub(wins),
        gross_usd: gross.round_dp(4),
        fees_usd: fees.round_dp(4),
        slippage_usd: slippage.round_dp(4),
        net_pnl_usd: net.round_dp(4),
        return_pct: scale(net),
        max_drawdown_pct: scale(max_drawdown),
        win_rate_pct: win_rate,
        trades_per_day,
    }
}

/// 回测一个技能策略实例。
pub async fn run(
    state: &AppState,
    record: &StrategyRecord,
    days: Option<u32>,
) -> Result<BacktestReport, String> {
    if !skill::manages(record) {
        return Err("该策略不是技能策略，无法按 SKILL.md 规则回测".to_string());
    }
    let settings = skill::settings_for(state, record);
    let rules: Vec<SkillRule> = settings
        .rules
        .iter()
        .filter(|rule| rule.enabled)
        .cloned()
        .collect();
    if rules.is_empty() {
        return Err("技能没有启用规则，无法回测".to_string());
    }

    let days = days.unwrap_or(30).clamp(1, 365) as i64;
    let symbols = if record.definition.universe.symbols.is_empty() {
        state.config.market_symbols.clone()
    } else {
        record.definition.universe.symbols.clone()
    };
    if symbols.is_empty() {
        return Err("策略没有配置交易对，无法回测".to_string());
    }
    let venues = if record.definition.universe.venues.is_empty() {
        vec!["binance".to_string()]
    } else {
        record.definition.universe.venues.clone()
    };
    let costs = Costs {
        fee_bps_per_leg: record.definition.costs.fee_bps_per_leg,
        slippage_bps: record.definition.costs.slippage_bps,
    };

    let mut trades = Vec::new();
    let mut bars = 0usize;
    let mut sample_start: Option<DateTime<Utc>> = None;
    let mut sample_end: Option<DateTime<Utc>> = None;
    let mut notional = Decimal::ZERO;
    // 海龟按账户权益计算 Unit，收益率分母也跟随权益，避免固定金额失真。
    let account_equity = state.accounts.total_equity_usd();
    let mut metric_base = Decimal::ZERO;
    let mut saw_event = false;
    let mut saw_grid = false;

    for rule in &rules {
        if rule.kind == SkillKind::Event {
            saw_event = true;
            continue;
        }
        let rule_notional = skill::capped_notional(rule);
        if rule_notional <= Decimal::ZERO {
            continue;
        }
        notional = notional.max(rule_notional);
        let wanted = bars_needed(days, &rule.trigger.timeframe)
            .max(i64::from(rule.trigger.lookback))
            .clamp(50, 1_000) as u32;

        for symbol in &symbols {
            let mut series = None;
            for venue in &venues {
                match state
                    .market
                    .candles(venue, symbol, &rule.trigger.timeframe, wanted)
                    .await
                {
                    Ok(candles) if candles.len() >= 3 => {
                        series = Some(candles);
                        break;
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        tracing::debug!(%venue, %symbol, %error, "技能回测取 K 线失败，尝试下一家交易所");
                    }
                }
            }
            let Some(candles) = series else {
                continue;
            };
            let snapshots: Vec<SandboxCandle> = candles
                .into_iter()
                .map(|candle| SandboxCandle {
                    open_time: candle.open_time,
                    open: candle.open,
                    high: candle.high,
                    low: candle.low,
                    close: candle.close,
                    volume: candle.volume,
                })
                .collect();
            let start = snapshots.first().map(|candle| candle.open_time);
            let end = snapshots.last().map(|candle| candle.open_time);
            sample_start = match (sample_start, start) {
                (Some(current), Some(next)) => Some(current.min(next)),
                (current, next) => current.or(next),
            };
            sample_end = match (sample_end, end) {
                (Some(current), Some(next)) => Some(current.max(next)),
                (current, next) => current.or(next),
            };
            bars += snapshots.len();
            if rule.kind == SkillKind::Grid {
                saw_grid = true;
            }
            if rule.kind == SkillKind::Breakout {
                let equity = if account_equity > Decimal::ZERO {
                    account_equity
                } else {
                    rule_notional * Decimal::from(10)
                };
                metric_base = metric_base.max(equity).max(rule_notional);
                trades.extend(replay_turtle(
                    rule,
                    symbol,
                    &snapshots,
                    equity,
                    rule_notional,
                    costs,
                ));
            } else {
                trades.extend(replay(rule, symbol, &snapshots, rule_notional, costs));
            }
        }
    }

    if notional <= Decimal::ZERO {
        return Err("公告类技能依赖实时公告，历史 K 线无法回放；请改用模拟模式实测".to_string());
    }
    if bars == 0 {
        return Err("没有取到历史 K 线，无法回测；请检查交易对与交易所配置".to_string());
    }

    trades.sort_by(|left, right| left.closed_at.cmp(&right.closed_at));
    let metrics = summarize(&trades, metric_base.max(notional), days);
    let split_at = match (sample_start, sample_end) {
        (Some(start), Some(end)) if end > start => {
            Some(start + chrono::Duration::seconds((end - start).num_seconds() * 7 / 10))
        }
        _ => None,
    };
    let mut in_sample = Decimal::ZERO;
    let mut out_of_sample = Decimal::ZERO;
    let mut curve = Vec::new();
    let mut equity = Decimal::ZERO;
    for trade in &trades {
        equity += trade.pnl;
        match split_at {
            Some(split) if trade.opened_at <= split => in_sample += trade.pnl,
            Some(_) => out_of_sample += trade.pnl,
            None => in_sample += trade.pnl,
        }
        curve.push(EquityPoint {
            at: trade.closed_at,
            cumulative_pnl_usd: equity,
            symbol: trade.symbol.clone(),
        });
    }

    let mut assumptions = vec![
        BacktestAssumption::BarCloseFills,
        BacktestAssumption::RoundTripFees,
        BacktestAssumption::RoundTripSlippage,
    ];
    if saw_grid {
        assumptions.push(BacktestAssumption::LimitFillOnTouch);
    }
    if saw_event {
        assumptions.push(BacktestAssumption::NoEventReplay);
    }

    Ok(BacktestReport {
        strategy_name: record.definition.name.clone(),
        definition_version: record.version,
        generated_at: Utc::now(),
        sample_start,
        sample_end,
        settlements: bars,
        trades: metrics.trades,
        wins: metrics.wins,
        losses: metrics.losses,
        gross_funding_usd: metrics.gross_usd,
        fees_usd: metrics.fees_usd,
        slippage_usd: metrics.slippage_usd,
        net_pnl_usd: metrics.net_pnl_usd,
        return_pct: metrics.return_pct,
        max_drawdown_pct: metrics.max_drawdown_pct,
        in_sample_net_pnl_usd: in_sample.round_dp(4),
        out_of_sample_net_pnl_usd: out_of_sample.round_dp(4),
        assumptions,
        equity_curve: downsample(curve, 240),
    })
}

fn bars_needed(days: i64, timeframe: &str) -> i64 {
    let secs = interval_secs(timeframe);
    if secs <= 0 {
        return 50;
    }
    (days * 86_400 / secs).max(50)
}

fn downsample(points: Vec<EquityPoint>, limit: usize) -> Vec<EquityPoint> {
    if points.len() <= limit {
        return points;
    }
    let step = (points.len() + limit - 1) / limit;
    points.into_iter().step_by(step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{SkillBusinessRules, SkillDirection, SkillSteps, SkillTrigger};
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn candle(
        index: i64,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
    ) -> SandboxCandle {
        SandboxCandle {
            open_time: Utc
                .timestamp_opt(index * 3_600, 0)
                .single()
                .expect("合法时间"),
            open,
            high,
            low,
            close,
            volume: Decimal::ONE,
        }
    }

    fn breakout_rule() -> SkillRule {
        SkillRule {
            name: "海龟突破".to_string(),
            purpose: "突破入场".to_string(),
            kind: SkillKind::Breakout,
            trigger: SkillTrigger {
                timeframe: "1h".to_string(),
                lookback: 100,
                fast: 2,
                slow: 3,
                atr: 2,
                channel: 3,
                atr_multiple: dec!(2),
                turtle: crate::skill::TurtleSystems {
                    system2_enabled: false,
                    entry1: 3,
                    exit1: 2,
                    entry2: 0,
                    exit2: 0,
                    ..crate::skill::TurtleSystems::default()
                },
                ..SkillTrigger::default()
            },
            steps: SkillSteps {
                execution_mode: arb_core::StrategyExecutionMode::Paper,
                execution_venues: vec!["binance".to_string()],
                direction: SkillDirection::Both,
                ..SkillSteps::default()
            },
            business: SkillBusinessRules {
                notional_usd: dec!(1000),
                leverage: Decimal::ONE,
                take_profit_pct: Decimal::ZERO,
                stop_loss_pct: Decimal::ZERO,
                ..SkillBusinessRules::default()
            },
            ..SkillRule::default()
        }
    }

    /// 合成序列只用收盘价，高低点与收盘价相同，便于手算通道与网格档位。
    fn series(prices: &[i64]) -> Vec<SandboxCandle> {
        prices
            .iter()
            .enumerate()
            .map(|(index, price)| {
                let close = Decimal::from(*price);
                candle(index as i64, close, close, close, close)
            })
            .collect()
    }

    #[test]
    fn breakout_opens_on_new_high_and_reports_costs() {
        let prices = [10, 10, 10, 10, 11, 12, 13, 14, 13, 11, 9, 9];
        let trades = replay(
            &breakout_rule(),
            "BTCUSDT",
            &series(&prices),
            dec!(1000),
            Costs::default(),
        );
        assert!(!trades.is_empty(), "趋势行情应至少产生一笔交易");
        let metrics = summarize(&trades, dec!(1000), 30);
        assert_eq!(metrics.trades, trades.len());
        assert_eq!(metrics.wins + metrics.losses, metrics.trades);
        assert!(metrics.fees_usd > Decimal::ZERO);
        assert!(metrics.slippage_usd > Decimal::ZERO);
        assert_eq!(
            metrics.net_pnl_usd,
            (metrics.gross_usd - metrics.fees_usd - metrics.slippage_usd).round_dp(4)
        );
    }

    #[test]
    fn turtle_pyramids_up_to_max_units() {
        let prices = [10, 10, 10, 10, 11, 12, 13, 14, 18];
        let trades = replay_turtle(
            &breakout_rule(),
            "BTCUSDT",
            &series(&prices),
            dec!(10000),
            dec!(1000),
            Costs::default(),
        );
        assert_eq!(trades.len(), 4, "顺势应加仓到 4 个 Units：{trades:?}");
    }

    #[test]
    fn flat_market_never_triggers() {
        let prices = [10i64; 40];
        let trades = replay(
            &breakout_rule(),
            "BTCUSDT",
            &series(&prices),
            dec!(1000),
            Costs::default(),
        );
        assert!(trades.is_empty(), "横盘不应触发突破：{trades:?}");
    }

    #[test]
    fn metrics_are_zero_without_trades() {
        let metrics = summarize(&[], dec!(1000), 30);
        assert_eq!(metrics.trades, 0);
        assert_eq!(metrics.win_rate_pct, Decimal::ZERO);
        assert_eq!(metrics.return_pct, Decimal::ZERO);
        assert_eq!(metrics.max_drawdown_pct, Decimal::ZERO);
    }

    #[test]
    fn grid_replays_level_touches() {
        let mut rule = breakout_rule();
        rule.kind = SkillKind::Grid;
        rule.trigger.timeframe = "1h".to_string();
        rule.business.grid_count = 2;
        rule.business.grid_spacing_pct = dec!(1);
        rule.business.grid_lower_price = dec!(99);
        rule.business.grid_upper_price = dec!(101);
        let prices = [100, 100, 99, 99, 100, 100, 101, 101, 100];
        let trades = replay(
            &rule,
            "BTCUSDT",
            &series(&prices),
            dec!(1000),
            Costs::default(),
        );
        assert!(!trades.is_empty(), "触及网格档位应成交：{trades:?}");
        // 区间 99-101、两格：档位价只能是 99（买）或 101（卖）。
        assert!(trades.iter().all(|trade| {
            matches!(trade.entry_price, price if price == dec!(99) || price == dec!(101))
        }));
    }

    #[test]
    fn event_rule_cannot_be_replayed() {
        let mut rule = breakout_rule();
        rule.kind = SkillKind::Event;
        let trades = replay(
            &rule,
            "BTCUSDT",
            &series(&[10, 11, 12]),
            dec!(1000),
            Costs::default(),
        );
        assert!(trades.is_empty());
    }
}
