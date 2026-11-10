//! 主机侧指标计算：沙箱模块只读取结果，不自己维护滚动窗口。

use rust_decimal::Decimal;

use crate::{
    sandbox::{SandboxCandle, SandboxIndicators},
    strategy::DataRules,
};

/// 按策略的数据窗口计算指标；数据不足的指标保持为空（模块读取时得到 0）。
pub fn compute(candles: &[SandboxCandle], rules: &DataRules) -> SandboxIndicators {
    let closes: Vec<Decimal> = candles.iter().map(|candle| candle.close).collect();
    let channel = donchian(candles, rules.channel);
    // 指标层只算配置里出现的均线周期；规则层再按周期引用它们。
    let mut sma_by_period = std::collections::BTreeMap::new();
    for period in &rules.ma_periods {
        if let Some(value) = sma(&closes, *period) {
            sma_by_period.insert(*period, value);
        }
    }
    SandboxIndicators {
        sma_fast: sma(&closes, rules.fast),
        sma_slow: sma(&closes, rules.slow),
        ema_fast: ema(&closes, rules.fast),
        ema_slow: ema(&closes, rules.slow),
        sma_by_period,
        atr: atr(candles, rules.atr),
        channel_high: channel.map(|(high, _)| high),
        channel_low: channel.map(|(_, low)| low),
    }
}

fn divisor(period: u32) -> Option<Decimal> {
    if period < 2 {
        return None;
    }
    Some(Decimal::from(period))
}

fn sma(closes: &[Decimal], period: u32) -> Option<Decimal> {
    let divisor = divisor(period)?;
    let start = closes.len().checked_sub(period as usize)?;
    Some(closes[start..].iter().sum::<Decimal>() / divisor)
}

fn ema(closes: &[Decimal], period: u32) -> Option<Decimal> {
    let divisor = divisor(period)?;
    let period = period as usize;
    if closes.len() < period {
        return None;
    }
    let weight = Decimal::from(2) / (divisor + Decimal::ONE);
    let mut value = closes[..period].iter().sum::<Decimal>() / divisor;
    for close in &closes[period..] {
        value = (*close - value) * weight + value;
    }
    Some(value)
}

fn atr(candles: &[SandboxCandle], period: u32) -> Option<Decimal> {
    let divisor = divisor(period)?;
    let period = period as usize;
    if candles.len() <= period {
        return None;
    }
    let ranges: Vec<Decimal> = candles
        .windows(2)
        .map(|pair| true_range(&pair[1], pair[0].close))
        .collect();
    let start = ranges.len().checked_sub(period)?;
    Some(ranges[start..].iter().sum::<Decimal>() / divisor)
}

fn true_range(candle: &SandboxCandle, previous_close: Decimal) -> Decimal {
    let high_low = candle.high - candle.low;
    let high_close = (candle.high - previous_close).abs();
    let low_close = (candle.low - previous_close).abs();
    high_low.max(high_close).max(low_close)
}

/// 唐奇安通道：取最新一根之前 `period` 根的最高价与最低价。
///
/// 排除最新一根，避免当前 K 线用自身的高点触发突破信号。
pub fn donchian(candles: &[SandboxCandle], period: u32) -> Option<(Decimal, Decimal)> {
    if period < 2 {
        return None;
    }
    let period = period as usize;
    let historical = candles.get(..candles.len().checked_sub(1)?)?;
    let start = historical.len().checked_sub(period)?;
    let window = &historical[start..];
    let high = window.iter().map(|candle| candle.high).max()?;
    let low = window.iter().map(|candle| candle.low).min()?;
    Some((high, low))
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

    fn rules(fast: u32, slow: u32, atr: u32, channel: u32) -> DataRules {
        DataRules {
            timeframe: "1m".to_string(),
            lookback: 100,
            fast,
            slow,
            atr,
            channel,
            ..DataRules::default()
        }
    }

    #[test]
    fn computes_moving_averages_and_channel() {
        let candles: Vec<SandboxCandle> = (0..5)
            .map(|index| candle(index, dec!(10) + Decimal::from(index)))
            .collect();
        let indicators = compute(&candles, &rules(2, 3, 2, 2));

        assert_eq!(indicators.sma_fast, Some(dec!(13.5)));
        assert_eq!(indicators.sma_slow, Some(dec!(13)));
        assert_eq!(indicators.ema_fast, Some(dec!(13.5)));
        assert_eq!(indicators.atr, Some(dec!(2)));
        // 最新一根被排除，通道只覆盖收盘 12 / 13 两根。
        assert_eq!(indicators.channel_high, Some(dec!(14)));
        assert_eq!(indicators.channel_low, Some(dec!(11)));
    }

    #[test]
    fn insufficient_data_leaves_indicators_empty() {
        let candles = vec![candle(0, dec!(10)), candle(1, dec!(11))];
        let indicators = compute(&candles, &rules(2, 3, 5, 10));

        assert_eq!(indicators.sma_fast, Some(dec!(10.5)));
        assert_eq!(indicators.sma_slow, None);
        assert_eq!(indicators.ema_slow, None);
        assert_eq!(indicators.atr, None);
        assert_eq!(indicators.channel_high, None);
        assert_eq!(
            indicators.by_id(crate::sandbox::SANDBOX_INDICATOR_SMA_SLOW),
            0.0
        );
    }
}
