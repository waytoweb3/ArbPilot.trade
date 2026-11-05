//! 自定义（沙箱）策略回测：按历史 K 线逐根重放 wasm 模块。
//!
//! 与实盘保持同一份沙箱输入结构，模块在回测里只能看到窗口内的行情，
//! 无法区分自己是在回放还是实盘。差异全部写进 `BacktestAssumption`：
//! - 每根 K 线只喂 `data.lookback` 根窗口，模块看不到未来数据；
//! - 开平仓按 K 线收盘价成交，不模拟盘中价格路径；
//! - 影子挂单在 K 线高低点触及时按挂单价成交；
//! - 配对开仓按零基差处理，只计成本与手续费。

use arb_core::{
    BacktestAssumption, BacktestReport, EquityPoint, Instrument, PositionSide, SandboxArtifact,
    SandboxCandle, SandboxDecision, SandboxInput, SandboxLimits, SandboxMarketSnapshot,
    SandboxOrderLimits, SandboxPositionSnapshot, SandboxRunStatus, StrategyDefinition,
    StrategyPendingOrder, StrategyRecord, SANDBOX_ABI_VERSION,
};
use chrono::{DateTime, TimeDelta, Utc};
use rust_decimal::Decimal;

use crate::state::AppState;

/// 交易所单次返回的 K 线上限。
const MAX_FETCH_BARS: u32 = 1_000;
/// 单次回测最多重放的 K 线根数，避免接口长时间占用。
const MAX_REPLAY_BARS: usize = 720;
/// 回测中保留的影子挂单上限，与运行期一致。
const MAX_PENDING: usize = 32;

/// 自定义策略回测入口：拉取历史 K 线后逐根重放已绑定的沙箱模块。
///
/// `days` 为回放窗口（天），按 K 线周期换算成根数；受 [`MAX_REPLAY_BARS`] 限制。
pub async fn run(
    state: &AppState,
    record: &StrategyRecord,
    days: Option<u32>,
) -> Result<BacktestReport, String> {
    let Some(binding) = record.sandbox.clone() else {
        return Err("请先编译并部署策略代码再回测".to_string());
    };
    let artifact = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.id == binding.artifact_id)
        .ok_or_else(|| format!("沙箱模块 {} 不存在", binding.artifact_id))?;
    if artifact.sha256 != binding.artifact_hash {
        return Err("沙箱模块哈希与绑定时不一致，已拒绝回测".to_string());
    }
    let wasm = state
        .store
        .sandbox_module(binding.artifact_id)
        .await
        .ok_or_else(|| format!("沙箱模块 {} 内容缺失", binding.artifact_id))?;

    let definition = &record.definition;
    let window = definition.data.lookback.clamp(20, MAX_FETCH_BARS) as usize;
    let days = days.unwrap_or(30).clamp(1, 365) as u64;
    let wanted = ((days * 86_400) / timeframe_secs(&definition.data.timeframe).max(1))
        .clamp((window + 1) as u64, MAX_REPLAY_BARS as u64) as usize;
    let venue = definition
        .trade_venue()
        .map(str::to_string)
        .or_else(|| definition.universe.venues.first().cloned())
        .unwrap_or_else(|| "binance".to_string());

    let mut series = Vec::new();
    for symbol in &definition.universe.symbols {
        let candles = state
            .market
            .candles(&venue, symbol, &definition.data.timeframe, MAX_FETCH_BARS)
            .await
            .map_err(|error| format!("拉取 {symbol} 历史 K 线失败: {error}"))?;
        if candles.len() < window + 1 {
            return Err(format!(
                "{symbol} 历史 K 线不足：至少需要 {} 根，实际 {} 根",
                window + 1,
                candles.len()
            ));
        }
        series.push((
            symbol.clone(),
            {
                let mut bars: Vec<SandboxCandle> = candles
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
                // 各交易所返回顺序不同，回放前统一按时间正序。
                bars.sort_by(|left, right| left.open_time.cmp(&right.open_time));
                // 只保留回放窗口；窗口本身还要用到前面的 context 根 K 线。
                let keep = wanted + window - 1;
                if bars.len() > keep {
                    bars.drain(..bars.len() - keep);
                }
                bars
            },
        ));
    }
    if series.is_empty() {
        return Err("策略没有声明交易对，无法回测".to_string());
    }

    replay(
        record,
        &artifact,
        &wasm,
        binding.limits,
        &venue,
        &series,
    )
    .await
}

/// 回放一组交易对，产出与资金费率回测同构的报告。
async fn replay(
    record: &StrategyRecord,
    artifact: &SandboxArtifact,
    wasm: &[u8],
    limits: SandboxLimits,
    venue: &str,
    series: &[(String, Vec<SandboxCandle>)],
) -> Result<BacktestReport, String> {
    let definition = &record.definition;
    let window = definition.data.lookback.clamp(20, MAX_FETCH_BARS) as usize;
    let max_notional = definition.risk.max_notional_usd;

    let mut trades: Vec<ClosedTrade> = Vec::new();
    let mut bars = 0usize;
    let mut sample_start: Option<DateTime<Utc>> = None;
    let mut sample_end: Option<DateTime<Utc>> = None;
    let mut saw_pair = false;

    for (symbol, candles) in series {
        let first = window - 1;
        let last = candles.len().min(window + MAX_REPLAY_BARS) - 1;
        if first >= last {
            continue;
        }
        sample_start = Some(
            sample_start
                .map(|start| start.min(candles[first].open_time))
                .unwrap_or(candles[first].open_time),
        );
        sample_end = Some(
            sample_end
                .map(|end| end.max(bar_close(&candles[last], &definition.data.timeframe)))
                .unwrap_or_else(|| bar_close(&candles[last], &definition.data.timeframe)),
        );

        let result = replay_symbol(
            record,
            artifact,
            wasm,
            &limits,
            venue,
            symbol,
            candles,
            first,
            last,
            max_notional,
        )
        .await?;
        bars += result.bars;
        saw_pair |= result.saw_pair;
        trades.extend(result.trades);
    }

    trades.sort_by(|left, right| left.closed_at.cmp(&right.closed_at));

    let notional = max_notional;
    let mut gross = Decimal::ZERO;
    let mut fees = Decimal::ZERO;
    let mut slippage = Decimal::ZERO;
    let mut equity = Decimal::ZERO;
    let mut peak = Decimal::ZERO;
    let mut max_drawdown_usd = Decimal::ZERO;
    let mut curve = Vec::new();
    let mut wins = 0usize;

    for trade in &trades {
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
        if drawdown > max_drawdown_usd {
            max_drawdown_usd = drawdown;
        }
        curve.push(EquityPoint {
            at: trade.closed_at,
            cumulative_pnl_usd: equity,
            symbol: trade.symbol.clone(),
        });
    }

    let split_at = match (sample_start, sample_end) {
        (Some(start), Some(end)) if end > start => {
            Some(start + TimeDelta::seconds((end - start).num_seconds() * 7 / 10))
        }
        _ => None,
    };
    let mut in_sample = Decimal::ZERO;
    let mut out_of_sample = Decimal::ZERO;
    for trade in &trades {
        match split_at {
            Some(split) if trade.opened_at <= split => in_sample += trade.pnl,
            Some(_) => out_of_sample += trade.pnl,
            None => in_sample += trade.pnl,
        }
    }

    let return_pct = if notional > Decimal::ZERO {
        (equity / notional * Decimal::from(100)).round_dp(4)
    } else {
        Decimal::ZERO
    };
    let max_drawdown_pct = if notional > Decimal::ZERO {
        (max_drawdown_usd / notional * Decimal::from(100)).round_dp(4)
    } else {
        Decimal::ZERO
    };

    let mut assumptions = vec![
        BacktestAssumption::WasmBarReplay,
        BacktestAssumption::BarCloseFills,
        BacktestAssumption::LimitFillOnTouch,
        BacktestAssumption::RoundTripFees,
        BacktestAssumption::RoundTripSlippage,
        BacktestAssumption::NoEventReplay,
    ];
    if saw_pair {
        assumptions.push(BacktestAssumption::ZeroBasis);
    }

    Ok(BacktestReport {
        strategy_name: definition.name.clone(),
        definition_version: record.version,
        generated_at: Utc::now(),
        sample_start,
        sample_end,
        settlements: bars,
        trades: trades.len(),
        wins,
        losses: trades.len().saturating_sub(wins),
        gross_funding_usd: gross.round_dp(4),
        fees_usd: fees.round_dp(4),
        slippage_usd: slippage.round_dp(4),
        net_pnl_usd: equity.round_dp(4),
        return_pct,
        max_drawdown_pct,
        in_sample_net_pnl_usd: in_sample.round_dp(4),
        out_of_sample_net_pnl_usd: out_of_sample.round_dp(4),
        assumptions,
        equity_curve: downsample(curve, 240),
    })
}

struct SymbolRun {
    trades: Vec<ClosedTrade>,
    bars: usize,
    saw_pair: bool,
}

struct ClosedTrade {
    symbol: String,
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
    gross: Decimal,
    fees: Decimal,
    slippage: Decimal,
    pnl: Decimal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PositionKind {
    Single,
    Pair,
}

struct SimPosition {
    kind: PositionKind,
    side: PositionSide,
    notional_usd: Decimal,
    entry_price: Decimal,
    opened_at: DateTime<Utc>,
    opened_index: usize,
}

#[allow(clippy::too_many_arguments)]
async fn replay_symbol(
    record: &StrategyRecord,
    artifact: &SandboxArtifact,
    wasm: &[u8],
    limits: &SandboxLimits,
    venue: &str,
    symbol: &str,
    candles: &[SandboxCandle],
    first: usize,
    last: usize,
    max_notional: Decimal,
) -> Result<SymbolRun, String> {
    let definition = &record.definition;
    let timeframe = definition.data.timeframe.as_str();
    let window_len = definition.data.lookback.clamp(20, MAX_FETCH_BARS) as usize;
    let instrument = instrument_for(venue);

    let mut position: Option<SimPosition> = None;
    let mut pending: Vec<StrategyPendingOrder> = Vec::new();
    let mut trades = Vec::new();
    let mut bars = 0usize;
    let mut saw_pair = false;

    for index in first..=last {
        let candle = &candles[index];
        let closed_at = bar_close(candle, timeframe);

        // 影子挂单先按本根 K 线的高低点判断是否触及；触发后按挂单价成交。
        let mut triggered = Vec::new();
        pending.retain(|order| {
            let hit = match order.side {
                PositionSide::Long => candle.low <= order.price,
                PositionSide::Short => candle.high >= order.price,
            };
            if hit {
                triggered.push(order.clone());
            }
            !hit
        });
        for order in triggered {
            if position.is_some() {
                continue;
            }
            let notional = order.notional_usd.min(max_notional);
            if notional <= Decimal::ZERO {
                continue;
            }
            position = Some(SimPosition {
                kind: PositionKind::Single,
                side: order.side,
                notional_usd: notional,
                entry_price: order.price,
                opened_at: closed_at,
                opened_index: index,
            });
        }

        let window = &candles[index + 1 - window_len..=index];
        let input = SandboxInput {
            abi_version: SANDBOX_ABI_VERSION,
            strategy_id: record.id,
            strategy_version: record.version,
            now: closed_at,
            limits: SandboxOrderLimits {
                max_notional_usd: max_notional,
                max_leverage: definition.execution.leverage,
                max_slippage_bps: definition.execution.max_slippage_bps,
                max_unhedged_secs: definition.risk.max_unhedged_seconds,
            },
            markets: vec![SandboxMarketSnapshot {
                symbol: symbol.to_string(),
                venue: venue.to_string(),
                instrument,
                bid: candle.close,
                ask: candle.close,
                last: Some(candle.close),
                funding_rate: None,
                next_funding_at: None,
                max_notional_usd: max_notional,
                observed_at: closed_at,
                candles: window.to_vec(),
                indicators: arb_core::indicators::compute(window, &definition.data),
            }],
            position: position.as_ref().map(|open| SandboxPositionSnapshot {
                symbol: symbol.to_string(),
                venue: venue.to_string(),
                side: open.side,
                notional_usd: open.notional_usd,
                entry_price: open.entry_price,
                opened_at: open.opened_at,
                held_secs: (closed_at - open.opened_at).num_seconds().max(0) as u64,
                held_bars: (index - open.opened_index) as u32,
                entry_net_edge_bps: Decimal::ZERO,
            }),
            // 回测没有历史公告可重放，事件输入保持为空。
            events: Vec::new(),
        };

        let report = crate::sandbox::evaluate(
            artifact.clone(),
            wasm.to_vec(),
            input,
            limits.clone(),
        )
        .await;
        if report.status != SandboxRunStatus::Ok {
            return Err(format!(
                "{symbol} 第 {} 根 K 线执行失败（{:?}）：{}",
                index + 1,
                report.status,
                report.error.unwrap_or_else(|| "无错误信息".to_string())
            ));
        }
        bars += 1;

        // 决策按本根 K 线收盘价执行；挂单登记为影子委托，等待下一根 K 线触发。
        for decision in report.decisions {
            match decision {
                SandboxDecision::Open {
                    symbol: target,
                    notional_usd,
                    side,
                    ..
                } => {
                    if !target.eq_ignore_ascii_case(symbol) || position.is_some() {
                        continue;
                    }
                    let notional = notional_usd.min(max_notional);
                    if notional <= Decimal::ZERO {
                        continue;
                    }
                    position = Some(SimPosition {
                        kind: PositionKind::Single,
                        side,
                        notional_usd: notional,
                        entry_price: candle.close,
                        opened_at: closed_at,
                        opened_index: index,
                    });
                }
                SandboxDecision::OpenPair {
                    symbol: target,
                    notional_usd,
                    ..
                } => {
                    if !target.eq_ignore_ascii_case(symbol) || position.is_some() {
                        continue;
                    }
                    let notional = notional_usd.min(max_notional);
                    if notional <= Decimal::ZERO {
                        continue;
                    }
                    saw_pair = true;
                    position = Some(SimPosition {
                        kind: PositionKind::Pair,
                        side: PositionSide::Short,
                        notional_usd: notional,
                        entry_price: candle.close,
                        opened_at: closed_at,
                        opened_index: index,
                    });
                }
                SandboxDecision::Close { symbol: target, .. } => {
                    if !target.eq_ignore_ascii_case(symbol) {
                        continue;
                    }
                    if let Some(open) = position.take() {
                        trades.push(close_trade(
                            symbol,
                            open,
                            candle.close,
                            closed_at,
                            definition,
                        ));
                    }
                }
                SandboxDecision::PlaceLimit {
                    symbol: target,
                    side,
                    price,
                    notional_usd,
                    ..
                } => {
                    if !target.eq_ignore_ascii_case(symbol) || price <= Decimal::ZERO {
                        continue;
                    }
                    let order = StrategyPendingOrder {
                        symbol: symbol.to_string(),
                        venue: Some(venue.to_string()),
                        side,
                        price,
                        notional_usd: notional_usd.min(max_notional),
                        created_at: closed_at,
                    };
                    if pending.len() < MAX_PENDING
                        && !pending.iter().any(|existing| existing.same_level(&order))
                    {
                        pending.push(order);
                    }
                }
                SandboxDecision::Cancel { symbol: target, .. } => {
                    if !target.eq_ignore_ascii_case(symbol) {
                        continue;
                    }
                    pending.clear();
                }
                SandboxDecision::Hold { .. } => {}
            }
        }
    }

    // 样本结束时强制平仓，避免未实现收益被忽略。
    if let Some(open) = position.take() {
        let last_candle = &candles[last];
        trades.push(close_trade(
            symbol,
            open,
            last_candle.close,
            bar_close(last_candle, timeframe),
            definition,
        ));
    }

    Ok(SymbolRun {
        trades,
        bars,
        saw_pair,
    })
}

fn close_trade(
    symbol: &str,
    position: SimPosition,
    exit_price: Decimal,
    closed_at: DateTime<Utc>,
    definition: &StrategyDefinition,
) -> ClosedTrade {
    let notional = position.notional_usd;
    let gross = match (position.kind, position.side) {
        (PositionKind::Pair, _) => Decimal::ZERO,
        (_, PositionSide::Long) if position.entry_price > Decimal::ZERO => {
            (exit_price - position.entry_price) / position.entry_price * notional
        }
        (_, PositionSide::Short) if position.entry_price > Decimal::ZERO => {
            (position.entry_price - exit_price) / position.entry_price * notional
        }
        _ => Decimal::ZERO,
    };
    // 单腿策略开平各一次；配对开仓两条腿，各开平一次。
    let legs = if position.kind == PositionKind::Pair {
        Decimal::from(4)
    } else {
        Decimal::from(2)
    };
    let fees = notional * definition.costs.fee_bps_per_leg * legs / Decimal::from(10_000);
    let slippage =
        notional * definition.costs.slippage_bps * Decimal::from(2) / Decimal::from(10_000);

    ClosedTrade {
        symbol: symbol.to_string(),
        opened_at: position.opened_at,
        closed_at,
        gross: gross.round_dp(4),
        fees: fees.round_dp(4),
        slippage: slippage.round_dp(4),
        pnl: (gross - fees - slippage).round_dp(4),
    }
}

/// K 线收盘时间，用于回放中的 `now` 与持仓时长。
fn bar_close(candle: &SandboxCandle, timeframe: &str) -> DateTime<Utc> {
    candle.open_time + TimeDelta::seconds(timeframe_secs(timeframe) as i64)
}

fn timeframe_secs(timeframe: &str) -> u64 {
    match timeframe {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1_800,
        "1h" => 3_600,
        "4h" => 14_400,
        "1d" => 86_400,
        "1w" => 604_800,
        _ => 300,
    }
}

/// 以 `-spot` 结尾的交易所标识走现货，其余按永续处理。
fn instrument_for(venue: &str) -> Instrument {
    if venue.trim().to_ascii_lowercase().ends_with("-spot") {
        Instrument::Spot
    } else {
        Instrument::Perp
    }
}

/// 回测曲线最多保留 240 个点，与资金费率回测保持一致。
fn downsample(points: Vec<EquityPoint>, limit: usize) -> Vec<EquityPoint> {
    if points.len() <= limit {
        return points;
    }
    let step = (points.len() as f64 / limit as f64).ceil() as usize;
    let mut sampled: Vec<EquityPoint> = points.iter().step_by(step.max(1)).cloned().collect();
    if let Some(last) = points.last() {
        if sampled.last().map(|point| point.at) != Some(last.at) {
            sampled.push(last.clone());
        }
    }
    sampled
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::{
        CostAssumptions, DataRules, EntryRules, ExecutionRules, ExitRules, StrategyRisk,
        StrategySource, StrategyStatus, StrategyUniverse,
    };
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    /// 空仓时开多，持仓时平仓：用来验证逐根重放的开平仓与成本口径。
    const FLIP_WAT: &str = r#"
(module
  (import "arbpilot" "position_present" (func $present (result i32)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "close" (func $close (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "BTCUSDT")
  (func (export "abi_version") (result i32) i32.const 2)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (if (i32.eq (call $present) (i32.const 0))
      (then (drop (call $open (i32.const 1024) (i32.const 7) (f64.const 100) (i32.const 1))))
      (else (drop (call $close (i32.const 1024) (i32.const 7)))))
    i32.const 0))
"#;

    fn record() -> StrategyRecord {
        StrategyRecord::new(
            StrategyDefinition {
                name: "replay".to_string(),
                kind: arb_core::StrategyKind::Custom,
                universe: StrategyUniverse {
                    venues: vec!["binance".to_string()],
                    symbols: vec!["BTCUSDT".to_string()],
                    instruments: vec![Instrument::Perp],
                },
                entry: EntryRules {
                    funding_spread_min: dec!(0.0003),
                    net_edge_min: dec!(0.0001),
                },
                exit: ExitRules {
                    spread_below: dec!(0.0002),
                    max_holding_hours: dec!(24),
                },
                risk: StrategyRisk {
                    max_notional_usd: dec!(100),
                    max_unhedged_seconds: 3,
                },
                execution: ExecutionRules {
                    leverage: Decimal::ONE,
                    max_slippage_bps: dec!(10),
                    cooldown_secs: 0,
                },
                data: DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 20,
                    fast: 5,
                    slow: 10,
                    atr: 5,
                    channel: 5,
                    ..DataRules::default()
                },
                costs: CostAssumptions::default(),
                notes: None,
            },
            StrategySource::Manual,
            StrategyStatus::Draft,
        )
    }

    fn candles(closes: &[i64]) -> Vec<SandboxCandle> {
        closes
            .iter()
            .enumerate()
            .map(|(index, close)| SandboxCandle {
                open_time: Utc
                    .timestamp_opt(1_700_000_000 + index as i64 * 300, 0)
                    .single()
                    .expect("合法时间"),
                open: Decimal::from(*close),
                high: Decimal::from(*close) + Decimal::ONE,
                low: Decimal::from(*close) - Decimal::ONE,
                close: Decimal::from(*close),
                volume: Decimal::ONE,
            })
            .collect()
    }

    #[tokio::test]
    async fn replays_wasm_and_charges_round_trip_costs() {
        let bytes = wat::parse_str(FLIP_WAT).expect("wat 编译");
        let artifact = SandboxArtifact {
            id: uuid::Uuid::new_v4(),
            name: "replay".to_string(),
            sha256: crate::sandbox::hash_wasm(&bytes),
            size_bytes: bytes.len(),
            abi_version: Some(SANDBOX_ABI_VERSION),
            source: arb_core::SandboxArtifactSource::Uploaded,
            notes: None,
            created_at: Utc::now(),
        };
        let record = record();
        let closes: Vec<i64> = (0..25).map(|index| 100 + index).collect();
        let series = vec![("BTCUSDT".to_string(), candles(&closes))];

        let report = replay(
            &record,
            &artifact,
            &bytes,
            SandboxLimits::default(),
            "binance",
            &series,
        )
        .await
        .expect("回放成功");

        // 25 根 K 线，窗口 20 根，可重放 6 根：第 1 根开仓、第 2 根平仓，循环往复。
        assert_eq!(report.settlements, 6);
        assert_eq!(report.trades, 3);
        assert!(report.wins >= 2, "{:?}", report);
        assert!(report.sample_start.is_some());
        assert!(report.sample_end.is_some());
        assert_eq!(report.net_pnl_usd, report.gross_funding_usd - report.fees_usd - report.slippage_usd);
        assert!(report
            .assumptions
            .contains(&BacktestAssumption::WasmBarReplay));
        assert!(report.assumptions.contains(&BacktestAssumption::BarCloseFills));
        // 每笔交易 2 腿手续费：100 * 4bps * 2 / 10000 = 0.08
        assert_eq!(report.fees_usd, dec!(0.24));
    }
}
