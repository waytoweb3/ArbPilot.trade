use std::time::Duration;

use arb_core::{
    OrderOrigin, OrderStatus, PositionSide, SandboxDecision, SandboxInput, SandboxMarketSnapshot,
    SandboxRunStatus, StrategyEventLevel, StrategyKind, StrategyOpenPosition, StrategyPendingOrder,
    StrategyRecord, StrategyRunScope, StrategyStatus,
};
use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;

use crate::{
    gateway::{self, LegOrderRequest, OrderBatchRequest, PairOrderRequest},
    state::AppState,
    store::{StrategyRun, StrategyRunEvent},
};

/// 策略运行时：只把「运行中」的策略翻译成订单意图，再交给统一风控网关。
///
/// 策略本身不接触交易所、密钥或下单接口；执行器整体暂停时也不会开新仓。
pub fn spawn(state: AppState) {
    let poll = Duration::from_secs(state.config.strategy_poll_secs.max(5));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll);
        loop {
            ticker.tick().await;
            if let Err(error) = evaluate(&state).await {
                tracing::warn!(%error, "策略运行评估失败");
            }
        }
    });
}

async fn evaluate(state: &AppState) -> anyhow::Result<()> {
    // 开启授权校验时，没有 strategy 套餐就不产生新的订单意图（默认关闭）。
    if !state.license.allows("strategy") {
        tracing::debug!("授权不足，跳过策略运行评估");
        return Ok(());
    }

    let records: Vec<StrategyRecord> = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .filter(|record| record.status == StrategyStatus::Running && runs_here(record))
        .cloned()
        .collect();

    for record in records {
        evaluate_one(state, record).await;
    }
    Ok(())
}

/// 策略始终在用户自己的执行器上运行；历史遗留的平台托管记录不在本地执行。
fn runs_here(record: &StrategyRecord) -> bool {
    record.run_scope == StrategyRunScope::SelfHosted
}

async fn evaluate_one(state: &AppState, mut record: StrategyRecord) {
    let now = Utc::now();
    record.last_heartbeat_at = Some(now);
    record.runtime.last_evaluated_at = Some(now);
    let events_before = record.runtime.events.len();

    // 校验未通过的策略不允许运行：直接暂停，避免带着错误定义下单。
    if !record.can_run() {
        record.status = StrategyStatus::Paused;
        record
            .runtime
            .record(StrategyEventLevel::Error, "校验未通过，策略已暂停", None);
        sync_run(state, &mut record, events_before).await;
        commit(state, record, true).await;
        return;
    }

    // 内置「现货-永续」资金费率策略已下线，历史记录直接暂停。
    if record.definition.kind == StrategyKind::FundingRate {
        record.status = StrategyStatus::Paused;
        record.runtime.record(
            StrategyEventLevel::Error,
            "内置资金费率套利（现货-永续）已下线，请改用永续-永续资金费率策略",
            None,
        );
        sync_run(state, &mut record, events_before).await;
        commit(state, record, true).await;
        return;
    }

    // 自定义策略没有内置规则，缺少沙箱绑定时必须停下。
    if record.sandbox.is_none() {
        record.status = StrategyStatus::Paused;
        record.runtime.record(
            StrategyEventLevel::Error,
            "自定义策略尚未绑定沙箱代码，策略已暂停",
            None,
        );
        sync_run(state, &mut record, events_before).await;
        commit(state, record, true).await;
        return;
    }

    if state.is_paused() {
        sync_run(state, &mut record, events_before).await;
        commit(state, record, false).await;
        return;
    }

    let evaluated = evaluate_sandbox(state, &mut record).await;

    let persist = match evaluated {
        Ok(changed) => changed,
        Err(message) => {
            record
                .runtime
                .record(StrategyEventLevel::Warning, message, None);
            true
        }
    };
    sync_run(state, &mut record, events_before).await;
    commit(state, record, persist).await;
}

/// 维护运行实例：运行中的策略补齐实例与心跳，本轮新增事件写入运行事件表。
async fn sync_run(state: &AppState, record: &mut StrategyRecord, events_before: usize) {
    if record.status != StrategyStatus::Running {
        if let Some(run_id) = record.runtime.run_id.take() {
            if let Some(mut run) = state.store.strategy_run(run_id).await {
                run.status = record.status;
                run.stopped_at = Some(Utc::now());
                run.last_heartbeat_at = Some(Utc::now());
                state.store.save_strategy_run(&run).await;
            }
        }
        return;
    }

    let run_id = match record.runtime.run_id {
        Some(id) => id,
        None => {
            let run = StrategyRun::new(record);
            let id = run.id;
            state.store.save_strategy_run(&run).await;
            record.runtime.run_id = Some(id);
            id
        }
    };

    if let Some(mut run) = state.store.strategy_run(run_id).await {
        run.strategy_version = record.version;
        run.status = StrategyStatus::Running;
        run.last_heartbeat_at = Some(Utc::now());
        state.store.save_strategy_run(&run).await;
    }

    // 上限裁剪会丢掉最早的事件，取不到原下标时按最新事件全量补写。
    let start = events_before.min(record.runtime.events.len());
    for event in record.runtime.events[start..].iter() {
        state
            .store
            .append_strategy_run_event(&StrategyRunEvent {
                run_id,
                strategy_id: record.id,
                level: event.level,
                message: event.message.clone(),
                order_id: event.order_id,
                created_at: event.at,
            })
            .await;
    }
}

/// 提交开仓意图。所有策略入口都经过统一风控网关。
#[allow(clippy::too_many_arguments)]
async fn submit_open(
    state: &AppState,
    record: &mut StrategyRecord,
    symbol: &str,
    long_venue: &str,
    short_venue: &str,
    notional: Decimal,
    entry_net_edge_bps: Decimal,
    message: String,
    idempotency_key: String,
) -> Result<bool, String> {
    let request = PairOrderRequest {
        symbol: symbol.to_string(),
        long_venue: long_venue.to_string(),
        short_venue: short_venue.to_string(),
        notional_usd: notional,
        leverage: record.definition.execution.leverage,
        max_slippage_bps: Some(record.definition.execution.max_slippage_bps),
        strategy_id: Some(record.id),
        origin: OrderOrigin::Strategy,
        idempotency_key,
        closing: false,
        max_unhedged_secs: Some(record.definition.risk.max_unhedged_seconds),
    };

    match gateway::submit_pair(state, request).await {
        Ok(outcome) => {
            record.runtime.signals += 1;
            record.runtime.last_order_id = Some(outcome.order.id);
            if outcome.order.status == OrderStatus::Filled {
                record.runtime.orders += 1;
                record.runtime.last_signal_at = Some(Utc::now());
                record.runtime.open = Some(StrategyOpenPosition {
                    symbol: symbol.to_string(),
                    long_venue: long_venue.to_string(),
                    short_venue: short_venue.to_string(),
                    venue: None,
                    side: None,
                    entry_price: None,
                    notional_usd: notional,
                    opened_at: Utc::now(),
                    entry_net_edge_bps,
                });
                record
                    .runtime
                    .record(StrategyEventLevel::Info, message, Some(outcome.order.id));
            } else {
                let note = outcome
                    .order
                    .note
                    .clone()
                    .unwrap_or_else(|| "订单未成交".to_string());
                record.runtime.record(
                    StrategyEventLevel::Warning,
                    format!("开仓未成交: {note}"),
                    Some(outcome.order.id),
                );
            }
            Ok(true)
        }
        Err(error) => Err(format!("提交开仓失败: {error:?}")),
    }
}

/// 提交平仓意图。撤回发布、暂停策略都不会跳过这里的风控复核。
async fn submit_close(
    state: &AppState,
    record: &mut StrategyRecord,
    open: &StrategyOpenPosition,
    message: String,
    idempotency_key: String,
) -> Result<bool, String> {
    let request = PairOrderRequest {
        symbol: open.symbol.clone(),
        long_venue: open.long_venue.clone(),
        short_venue: open.short_venue.clone(),
        notional_usd: open.notional_usd,
        leverage: record.definition.execution.leverage,
        max_slippage_bps: Some(record.definition.execution.max_slippage_bps),
        strategy_id: Some(record.id),
        origin: OrderOrigin::Strategy,
        idempotency_key,
        closing: true,
        max_unhedged_secs: Some(record.definition.risk.max_unhedged_seconds),
    };

    match gateway::submit_pair(state, request).await {
        Ok(outcome) => {
            record.runtime.last_order_id = Some(outcome.order.id);
            if outcome.order.status == OrderStatus::Filled {
                record.runtime.orders += 1;
                record.runtime.last_signal_at = Some(Utc::now());
                record.runtime.open = None;
                record
                    .runtime
                    .record(StrategyEventLevel::Info, message, Some(outcome.order.id));
            } else {
                let note = outcome
                    .order
                    .note
                    .clone()
                    .unwrap_or_else(|| "订单未成交".to_string());
                record.runtime.record(
                    StrategyEventLevel::Warning,
                    format!("平仓未成交: {note}"),
                    Some(outcome.order.id),
                );
            }
            Ok(true)
        }
        Err(error) => Err(format!("提交平仓失败: {error:?}")),
    }
}

/// 提交单腿方向性开仓。方向性策略（趋势、网格）只交易一条腿。
async fn submit_single_open(
    state: &AppState,
    record: &mut StrategyRecord,
    venue: &str,
    symbol: &str,
    side: PositionSide,
    notional: Decimal,
    message: String,
    idempotency_key: String,
) -> Result<bool, String> {
    let request = OrderBatchRequest {
        legs: vec![LegOrderRequest {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side: side.open_side(),
            notional_usd: notional,
            limit_price: None,
            quantity: None,
        }],
        leverage: record.definition.execution.leverage,
        max_slippage_bps: Some(record.definition.execution.max_slippage_bps),
        strategy_id: Some(record.id),
        origin: OrderOrigin::Strategy,
        idempotency_key,
        closing: false,
        max_unhedged_secs: Some(record.definition.risk.max_unhedged_seconds),
    };

    match gateway::submit_legs(state, request).await {
        Ok(outcome) => {
            record.runtime.signals += 1;
            record.runtime.last_order_id = Some(outcome.order.id);
            if outcome.order.status == OrderStatus::Filled {
                record.runtime.orders += 1;
                record.runtime.last_signal_at = Some(Utc::now());
                let entry_price = outcome.order.legs.first().and_then(|leg| leg.average_price);
                record.runtime.open = Some(StrategyOpenPosition {
                    symbol: symbol.to_string(),
                    long_venue: String::new(),
                    short_venue: String::new(),
                    venue: Some(venue.to_string()),
                    side: Some(side),
                    entry_price,
                    notional_usd: notional,
                    opened_at: Utc::now(),
                    entry_net_edge_bps: Decimal::ZERO,
                });
                record
                    .runtime
                    .record(StrategyEventLevel::Info, message, Some(outcome.order.id));
            } else {
                let note = outcome
                    .order
                    .note
                    .clone()
                    .unwrap_or_else(|| "订单未成交".to_string());
                record.runtime.record(
                    StrategyEventLevel::Warning,
                    format!("开仓未成交: {note}"),
                    Some(outcome.order.id),
                );
            }
            Ok(true)
        }
        Err(error) => Err(format!("提交开仓失败: {error:?}")),
    }
}

/// 平掉单腿持仓：方向与开仓相反，仍经过统一风控网关。
async fn submit_single_close(
    state: &AppState,
    record: &mut StrategyRecord,
    open: &StrategyOpenPosition,
    message: String,
    idempotency_key: String,
) -> Result<bool, String> {
    let Some(venue) = open.venue.clone() else {
        return Err("单腿持仓缺少交易所，无法平仓".to_string());
    };
    let side = open.side.unwrap_or(PositionSide::Long);
    let request = OrderBatchRequest {
        legs: vec![LegOrderRequest {
            venue,
            symbol: open.symbol.clone(),
            side: side.close_side(),
            notional_usd: open.notional_usd,
            limit_price: None,
            quantity: None,
        }],
        leverage: record.definition.execution.leverage,
        max_slippage_bps: Some(record.definition.execution.max_slippage_bps),
        strategy_id: Some(record.id),
        origin: OrderOrigin::Strategy,
        idempotency_key,
        closing: true,
        max_unhedged_secs: Some(record.definition.risk.max_unhedged_seconds),
    };

    match gateway::submit_legs(state, request).await {
        Ok(outcome) => {
            record.runtime.last_order_id = Some(outcome.order.id);
            if outcome.order.status == OrderStatus::Filled {
                record.runtime.orders += 1;
                record.runtime.last_signal_at = Some(Utc::now());
                record.runtime.open = None;
                record
                    .runtime
                    .record(StrategyEventLevel::Info, message, Some(outcome.order.id));
            } else {
                let note = outcome
                    .order
                    .note
                    .clone()
                    .unwrap_or_else(|| "订单未成交".to_string());
                record.runtime.record(
                    StrategyEventLevel::Warning,
                    format!("平仓未成交: {note}"),
                    Some(outcome.order.id),
                );
            }
            Ok(true)
        }
        Err(error) => Err(format!("提交平仓失败: {error:?}")),
    }
}

/// 沙箱决策：模块只输出意图，真正下单仍走统一风控网关。
///
/// 模块内容按 sha256 校验，缺失或哈希不一致时直接拒绝执行，不回退到内置规则，
/// 避免用户以为在跑自己的逻辑而实际跑的是别的策略。
async fn evaluate_sandbox(state: &AppState, record: &mut StrategyRecord) -> Result<bool, String> {
    let Some(binding) = record.sandbox.clone() else {
        return Ok(false);
    };
    let artifact = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.id == binding.artifact_id)
        .ok_or_else(|| format!("沙箱模块 {} 不存在", binding.artifact_id))?;
    if artifact.sha256 != binding.artifact_hash {
        return Err("沙箱模块哈希与绑定时不一致，已拒绝执行".to_string());
    }
    let wasm = state
        .store
        .sandbox_module(binding.artifact_id)
        .await
        .ok_or_else(|| format!("沙箱模块 {} 内容缺失", binding.artifact_id))?;

    let input = crate::sandbox::build_input(state, Some(record), None, record.id).await;

    // 网格影子挂单先按当前盘口判断触发；触发后的下单同样经过统一风控网关。
    let mut changed = trigger_pending(state, record, &input).await?;

    let report = crate::sandbox::evaluate(artifact, wasm, input.clone(), binding.limits).await;

    if report.status != SandboxRunStatus::Ok {
        return Err(format!(
            "沙箱执行失败（{:?}）: {}",
            report.status,
            report
                .error
                .clone()
                .unwrap_or_else(|| "无错误信息".to_string())
        ));
    }

    // 模块一次评估可以提交多条意图（网格会同时挂多档），逐条分发。
    for decision in report.decisions {
        let now = Utc::now();
        match decision {
            SandboxDecision::Open {
                symbol,
                notional_usd,
                side,
                venue,
                reason,
            } => {
                if record.runtime.open.is_some() {
                    continue;
                }
                if let Some(last) = record.runtime.last_signal_at {
                    let cooldown =
                        ChronoDuration::seconds(record.definition.execution.cooldown_secs as i64);
                    if now.signed_duration_since(last) < cooldown {
                        continue;
                    }
                }
                let Some(market) = find_market(&input, &symbol, venue.as_deref()) else {
                    return Err(format!(
                        "沙箱请求开仓 {symbol}，但该交易对不在当前可交易范围内"
                    ));
                };
                let notional = notional_usd
                    .min(input.limits.max_notional_usd)
                    .min(market.max_notional_usd);
                if notional <= Decimal::ZERO {
                    return Err(format!("沙箱请求开仓 {symbol}，但可用名义价值为 0"));
                }

                let note = reason.unwrap_or_else(|| "沙箱信号".to_string());
                let venue = market.venue.clone();
                let symbol = market.symbol.clone();
                changed |= submit_single_open(
                    state,
                    record,
                    &venue,
                    &symbol,
                    side,
                    notional,
                    format!("沙箱开仓 {symbol} {notional} USD（{venue}）：{note}"),
                    format!("{}-sandbox-{}-{}", record.id, symbol, now.timestamp()),
                )
                .await?;
            }
            SandboxDecision::OpenPair {
                symbol,
                notional_usd,
                reason,
            } => {
                if record.runtime.open.is_some() {
                    continue;
                }
                if let Some(last) = record.runtime.last_signal_at {
                    let cooldown =
                        ChronoDuration::seconds(record.definition.execution.cooldown_secs as i64);
                    if now.signed_duration_since(last) < cooldown {
                        continue;
                    }
                }
                let Some(market) = find_market(&input, &symbol, None) else {
                    return Err(format!(
                        "沙箱请求配对开仓 {symbol}，但该交易对不在当前可交易范围内"
                    ));
                };
                let Some(spot_venue) = record.definition.spot_venue().map(str::to_string) else {
                    return Err("策略未配置现货交易所，无法配对开仓".to_string());
                };
                let Some(perp_venue) = record.definition.perp_venue().map(str::to_string) else {
                    return Err("策略未配置永续交易所，无法配对开仓".to_string());
                };
                let notional = notional_usd
                    .min(input.limits.max_notional_usd)
                    .min(market.max_notional_usd);
                if notional <= Decimal::ZERO {
                    return Err(format!("沙箱请求配对开仓 {symbol}，但可用名义价值为 0"));
                }

                let note = reason.unwrap_or_else(|| "沙箱信号".to_string());
                let symbol = market.symbol.clone();
                changed |= submit_open(
                    state,
                    record,
                    &symbol,
                    &spot_venue,
                    &perp_venue,
                    notional,
                    Decimal::ZERO,
                    format!("沙箱配对开仓 {symbol} {notional} USD：{note}"),
                    format!("{}-sandbox-pair-{}-{}", record.id, symbol, now.timestamp()),
                )
                .await?;
            }
            SandboxDecision::Close { symbol, reason } => {
                let Some(open) = record.runtime.open.clone() else {
                    continue;
                };
                if open.symbol != symbol {
                    return Err(format!(
                        "沙箱请求平仓 {symbol}，但当前持仓为 {}",
                        open.symbol
                    ));
                }
                let note = reason.unwrap_or_else(|| "沙箱信号".to_string());
                let message = format!("沙箱平仓 {}：{note}", open.symbol);
                let key = format!("{}-sandbox-exit-{}", record.id, now.timestamp());
                changed |= if open.is_pair() {
                    submit_close(state, record, &open, message, key).await?
                } else {
                    submit_single_close(state, record, &open, message, key).await?
                };
            }
            SandboxDecision::PlaceLimit {
                symbol,
                side,
                price,
                notional_usd,
                venue,
                reason,
            } => {
                let Some(market) = find_market(&input, &symbol, venue.as_deref()) else {
                    return Err(format!(
                        "沙箱请求挂单 {symbol}，但该交易对不在当前可交易范围内"
                    ));
                };
                let notional = notional_usd
                    .min(input.limits.max_notional_usd)
                    .min(market.max_notional_usd);
                if notional <= Decimal::ZERO {
                    return Err(format!("沙箱请求挂单 {symbol}，但可用名义价值为 0"));
                }
                let note = reason.unwrap_or_else(|| "沙箱挂单".to_string());
                let symbol = market.symbol.clone();
                let order = StrategyPendingOrder {
                    symbol: symbol.clone(),
                    venue: Some(market.venue.clone()),
                    side,
                    price,
                    notional_usd: notional,
                    created_at: now,
                };
                if record.runtime.upsert_pending(order) {
                    record.runtime.record(
                        StrategyEventLevel::Info,
                        format!("沙箱挂单 {symbol} @ {price}（{note}）"),
                        None,
                    );
                    changed = true;
                }
            }
            SandboxDecision::Cancel { symbol, reason } => {
                let removed = record.runtime.clear_pending(&symbol);
                if removed > 0 {
                    let note = reason.unwrap_or_else(|| "沙箱撤单".to_string());
                    record.runtime.record(
                        StrategyEventLevel::Info,
                        format!("沙箱撤销 {symbol} 的 {removed} 笔挂单：{note}"),
                        None,
                    );
                    changed = true;
                }
            }
            SandboxDecision::Hold { .. } => {}
        }
    }
    Ok(changed)
}

/// 检查网格影子挂单是否被当前盘口触发。
///
/// 挂单不提交到交易所，只在本地登记；触发后才通过统一风控网关下单，因此不存在
/// 交易所侧遗留挂单，也不需要撤单接口。触发一次后即从列表移除。
async fn trigger_pending(
    state: &AppState,
    record: &mut StrategyRecord,
    input: &SandboxInput,
) -> Result<bool, String> {
    if record.runtime.pending.is_empty() {
        return Ok(false);
    }

    let now = Utc::now();
    let mut triggered = Vec::new();
    let mut remaining = Vec::new();
    for pending in std::mem::take(&mut record.runtime.pending) {
        let Some(market) = find_market(input, &pending.symbol, pending.venue.as_deref()) else {
            continue;
        };
        if pending_triggered(&pending, market) {
            triggered.push((pending, market.venue.clone(), market.max_notional_usd));
        } else {
            remaining.push(pending);
        }
    }
    record.runtime.pending = remaining;

    let mut changed = false;
    for (pending, venue, market_limit) in triggered {
        let notional = pending
            .notional_usd
            .min(input.limits.max_notional_usd)
            .min(market_limit);
        if notional <= Decimal::ZERO {
            record.runtime.record(
                StrategyEventLevel::Warning,
                format!("网格挂单 {} 已触发，但可用名义价值为 0", pending.symbol),
                None,
            );
            continue;
        }

        let label = match pending.side {
            PositionSide::Long => "买入",
            PositionSide::Short => "卖出",
        };
        let key = format!(
            "{}-grid-{}-{}-{}",
            record.id,
            pending.symbol,
            label,
            now.timestamp()
        );

        // 已有反向单腿持仓时，触发视为平仓（网格高抛低吸）；同向持仓则无法加仓。
        if let Some(open) = record.runtime.open.clone() {
            if open.is_pair() || open.side == Some(pending.side) {
                record.runtime.record(
                    StrategyEventLevel::Info,
                    format!(
                        "网格挂单 {} @ {} 已触发，但已有同向持仓，跳过",
                        pending.symbol, pending.price
                    ),
                    None,
                );
                continue;
            }
            let message = format!(
                "网格挂单 {} @ {} 触发，平掉{}持仓",
                pending.symbol, pending.price, pending.symbol
            );
            match submit_single_close(state, record, &open, message, key).await {
                Ok(done) => changed |= done,
                Err(message) => record
                    .runtime
                    .record(StrategyEventLevel::Warning, message, None),
            }
            continue;
        }

        let message = format!(
            "网格挂单 {} @ {} 触发，{label} {notional} USD（{venue}）",
            pending.symbol, pending.price
        );
        match submit_single_open(
            state,
            record,
            &venue,
            &pending.symbol,
            pending.side,
            notional,
            message,
            key,
        )
        .await
        {
            Ok(done) => changed |= done,
            Err(message) => record
                .runtime
                .record(StrategyEventLevel::Warning, message, None),
        }
    }
    Ok(changed)
}

/// 判断待触发挂单是否够到价格：买单看卖一跌破，卖单看买一涨过。
fn pending_triggered(order: &StrategyPendingOrder, market: &SandboxMarketSnapshot) -> bool {
    match order.side {
        PositionSide::Long => market.ask <= order.price,
        PositionSide::Short => market.bid >= order.price,
    }
}

/// 按交易对与交易所匹配行情快照；未指定交易所时取第一笔。
fn find_market<'a>(
    input: &'a SandboxInput,
    symbol: &str,
    venue: Option<&str>,
) -> Option<&'a SandboxMarketSnapshot> {
    input.markets.iter().find(|market| {
        market.symbol == symbol && venue.map(|venue| venue == market.venue).unwrap_or(true)
    })
}

/// 把运行状态合并回共享状态；只有订单/状态变化时才落库。
async fn commit(state: &AppState, record: StrategyRecord, persist: bool) {
    let merged = {
        let mut strategies = state.strategies.write().expect("策略锁不可用");
        match strategies
            .iter_mut()
            .find(|existing| existing.id == record.id)
        {
            Some(existing) => {
                existing.status = record.status;
                existing.runtime = record.runtime;
                existing.last_heartbeat_at = record.last_heartbeat_at;
                if persist {
                    existing.updated_at = Utc::now();
                }
                persist.then(|| existing.clone())
            }
            None => None,
        }
    };

    if let Some(merged) = merged {
        state.store.save_strategy(&merged).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::{
        Instrument, SandboxCandle, SandboxIndicators, SandboxLimits, SandboxOrderLimits,
        StrategyRuntime,
    };
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn market(bid: Decimal, ask: Decimal) -> SandboxMarketSnapshot {
        SandboxMarketSnapshot {
            symbol: "BTCUSDT".into(),
            venue: "binance".into(),
            instrument: Instrument::Perp,
            bid,
            ask,
            last: Some(bid),
            funding_rate: None,
            next_funding_at: None,
            max_notional_usd: dec!(1000),
            observed_at: Utc::now(),
            candles: vec![SandboxCandle {
                open_time: Utc::now(),
                open: bid,
                high: bid,
                low: bid,
                close: bid,
                volume: Decimal::ONE,
            }],
            indicators: SandboxIndicators::default(),
        }
    }

    /// 端到端：网格模板在沙箱里产出的挂单意图进入待触发队列，价格穿越后触发。
    #[tokio::test]
    async fn grid_template_pending_triggers_when_price_crosses() {
        let wasm = wat::parse_str(crate::templates::GRID_WAT).expect("wat 编译");
        let market_at = dec!(100);
        let input = SandboxInput {
            abi_version: arb_core::SANDBOX_ABI_VERSION,
            strategy_id: Uuid::new_v4(),
            strategy_version: 1,
            now: Utc::now(),
            limits: SandboxOrderLimits {
                max_notional_usd: dec!(1000),
                max_leverage: Decimal::ONE,
                max_slippage_bps: dec!(10),
                max_unhedged_secs: 3,
            },
            markets: vec![market(market_at, market_at)],
            position: None,
            events: Vec::new(),
        };
        let artifact = arb_core::SandboxArtifact {
            id: Uuid::new_v4(),
            name: "grid".into(),
            sha256: crate::sandbox::hash_wasm(&wasm),
            size_bytes: wasm.len(),
            abi_version: Some(arb_core::SANDBOX_ABI_VERSION),
            source: arb_core::SandboxArtifactSource::BuiltIn,
            notes: None,
            created_at: Utc::now(),
        };
        let report =
            crate::sandbox::evaluate(artifact, wasm, input, SandboxLimits::default()).await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);

        let mut runtime = StrategyRuntime::default();
        for decision in report.decisions {
            if let SandboxDecision::PlaceLimit {
                symbol,
                side,
                price,
                notional_usd,
                venue,
                ..
            } = decision
            {
                assert!(runtime.upsert_pending(StrategyPendingOrder {
                    symbol,
                    venue,
                    side,
                    price,
                    notional_usd,
                    created_at: Utc::now(),
                }));
            }
        }
        assert_eq!(runtime.pending.len(), 6);

        // 价格没动时不应触发任何档位。
        let flat = market(market_at, market_at);
        assert!(runtime
            .pending
            .iter()
            .all(|order| !pending_triggered(order, &flat)));

        // 价格跌到 99.4：只有 99.5 的买单触发。
        let down = market(dec!(99.4), dec!(99.4));
        let hits: Vec<_> = runtime
            .pending
            .iter()
            .filter(|order| pending_triggered(order, &down))
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].side, PositionSide::Long);
        assert_eq!(hits[0].price, dec!(99.5));

        // 价格涨到 101.1：100.5 与 101 两档卖单触发。
        let up = market(dec!(101.1), dec!(101.1));
        let sells = runtime
            .pending
            .iter()
            .filter(|order| order.side == PositionSide::Short && pending_triggered(order, &up))
            .count();
        assert_eq!(sells, 2);
    }
}
