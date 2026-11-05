use std::time::Duration;

use arb_core::{
    Opportunity, OpportunityKind, OrderOrigin, OrderStatus, SandboxDecision, SandboxRunStatus,
    StrategyEventLevel, StrategyOpenPosition, StrategyRecord, StrategyRunScope, StrategyStatus,
};
use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;

use crate::{
    gateway::{self, PairOrderRequest},
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
        .filter(|record| record.status == StrategyStatus::Running && runs_here(record, state))
        .cloned()
        .collect();

    for record in records {
        evaluate_one(state, record).await;
    }
    Ok(())
}

/// 官方托管实例只跑已发布到市场的策略，自托管实例只跑用户自己的策略。
fn runs_here(record: &StrategyRecord, state: &AppState) -> bool {
    if state.config.hosted {
        record.run_scope == StrategyRunScope::Platform
    } else {
        record.run_scope == StrategyRunScope::SelfHosted
    }
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

    if state.is_paused() {
        sync_run(state, &mut record, events_before).await;
        commit(state, record, false).await;
        return;
    }

    // 绑定沙箱模块时由模块决定开仓/平仓；未绑定时使用内置资金费率规则。
    let evaluated = if record.sandbox.is_some() {
        evaluate_sandbox(state, &mut record).await
    } else if record.runtime.open.is_some() {
        evaluate_exit(state, &mut record).await
    } else {
        evaluate_entry(state, &mut record).await
    };

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

/// 入场：匹配策略交易对与双腿交易所的资金费率机会，产出开仓意图。
async fn evaluate_entry(state: &AppState, record: &mut StrategyRecord) -> Result<bool, String> {
    let now = Utc::now();
    let Some(opportunity) = select_opportunity(state, record) else {
        return Ok(false);
    };

    let funding_rate = opportunity.funding_rate.unwrap_or_default();
    if funding_rate < record.definition.entry.funding_spread_min {
        return Ok(false);
    }
    let net_edge = opportunity.net_edge_bps / Decimal::from(10_000);
    if net_edge < record.definition.entry.net_edge_min {
        return Ok(false);
    }
    if let Some(last) = record.runtime.last_signal_at {
        let cooldown = ChronoDuration::seconds(record.definition.execution.cooldown_secs as i64);
        if now.signed_duration_since(last) < cooldown {
            return Ok(false);
        }
    }

    let notional = record
        .definition
        .risk
        .max_notional_usd
        .min(opportunity.max_notional_usd);
    if notional <= Decimal::ZERO {
        return Ok(false);
    }

    let reason = format!(
        "已开仓 {} {} USD，费率 {} bps，净收益 {} bps",
        opportunity.symbol,
        notional,
        opportunity.gross_spread_bps,
        opportunity.net_edge_bps
    );
    submit_open(
        state,
        record,
        &opportunity.symbol,
        &opportunity.long_venue,
        &opportunity.short_venue,
        notional,
        opportunity.net_edge_bps,
        reason,
        format!(
            "{}-{}-{}",
            record.id,
            opportunity.symbol,
            opportunity.observed_at.timestamp()
        ),
    )
    .await
}

/// 出场：费率跌破退出阈值或超过最长持仓时间时，反向平掉双腿。
async fn evaluate_exit(state: &AppState, record: &mut StrategyRecord) -> Result<bool, String> {
    let Some(open) = record.runtime.open.clone() else {
        return Ok(false);
    };
    let now = Utc::now();
    let held_secs = Decimal::from(now.signed_duration_since(open.opened_at).num_seconds());
    let timed_out = held_secs >= record.definition.exit.max_holding_hours * Decimal::from(3_600);
    let rate_dropped = current_funding_rate(state, record, &open.symbol)
        .map(|rate| rate < record.definition.exit.spread_below)
        .unwrap_or(false);

    if !timed_out && !rate_dropped {
        return Ok(false);
    }

    let reason = if timed_out {
        format!(
            "持仓已超过 {} 小时",
            record.definition.exit.max_holding_hours
        )
    } else {
        format!(
            "资金费率跌破退出阈值 {}",
            record.definition.exit.spread_below
        )
    };

    submit_close(
        state,
        record,
        &open,
        format!("已平仓 {}：{reason}", open.symbol),
        format!("{}-exit-{}", record.id, now.timestamp()),
    )
    .await
}

/// 提交开仓意图。沙箱与内置规则共用此入口，确保都经过统一风控网关。
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
                    notional_usd: notional,
                    opened_at: Utc::now(),
                    entry_net_edge_bps,
                });
                record.runtime.record(
                    StrategyEventLevel::Info,
                    message,
                    Some(outcome.order.id),
                );
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
                record.runtime.record(
                    StrategyEventLevel::Info,
                    message,
                    Some(outcome.order.id),
                );
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
async fn evaluate_sandbox(
    state: &AppState,
    record: &mut StrategyRecord,
) -> Result<bool, String> {
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

    let input = crate::sandbox::build_input(state, Some(record), None, record.id);
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

    let Some(decision) = report
        .decisions
        .iter()
        .find(|decision| !matches!(decision, SandboxDecision::Hold { .. }))
    else {
        return Ok(false);
    };

    let now = Utc::now();
    match decision {
        SandboxDecision::Open {
            symbol,
            notional_usd,
            reason,
        } => {
            if record.runtime.open.is_some() {
                return Ok(false);
            }
            if let Some(last) = record.runtime.last_signal_at {
                let cooldown =
                    ChronoDuration::seconds(record.definition.execution.cooldown_secs as i64);
                if now.signed_duration_since(last) < cooldown {
                    return Ok(false);
                }
            }
            let Some(market) = input.markets.iter().find(|market| &market.symbol == symbol) else {
                return Err(format!(
                    "沙箱请求开仓 {symbol}，但该交易对不在当前可交易范围内"
                ));
            };
            let notional = (*notional_usd)
                .min(input.limits.max_notional_usd)
                .min(market.max_notional_usd);
            if notional <= Decimal::ZERO {
                return Err(format!("沙箱请求开仓 {symbol}，但可用名义价值为 0"));
            }

            let note = reason
                .clone()
                .unwrap_or_else(|| "沙箱信号".to_string());
            submit_open(
                state,
                record,
                &market.symbol,
                &market.spot_venue,
                &market.perp_venue,
                notional,
                market.net_edge_bps,
                format!("沙箱开仓 {} {} USD：{note}", market.symbol, notional),
                format!(
                    "{}-sandbox-{}-{}",
                    record.id,
                    market.symbol,
                    now.timestamp()
                ),
            )
            .await
        }
        SandboxDecision::Close { symbol, reason } => {
            let Some(open) = record.runtime.open.clone() else {
                return Ok(false);
            };
            if &open.symbol != symbol {
                return Err(format!(
                    "沙箱请求平仓 {symbol}，但当前持仓为 {}",
                    open.symbol
                ));
            }
            let note = reason
                .clone()
                .unwrap_or_else(|| "沙箱信号".to_string());
            submit_close(
                state,
                record,
                &open,
                format!("沙箱平仓 {}：{note}", open.symbol),
                format!("{}-sandbox-exit-{}", record.id, now.timestamp()),
            )
            .await
        }
        SandboxDecision::Hold { .. } => Ok(false),
    }
}

fn select_opportunity(state: &AppState, record: &StrategyRecord) -> Option<Opportunity> {
    let spot = record.definition.spot_venue()?;
    let perp = record.definition.perp_venue()?;
    let now = Utc::now();
    state
        .market
        .opportunities()
        .into_iter()
        .find(|opportunity| {
            opportunity.kind == OpportunityKind::FundingRate
                && record
                    .definition
                    .universe
                    .symbols
                    .contains(&opportunity.symbol)
                && opportunity.long_venue == spot
                && opportunity.short_venue == perp
                && opportunity.is_fresh(now, ChronoDuration::seconds(30))
        })
}

fn current_funding_rate(
    state: &AppState,
    record: &StrategyRecord,
    symbol: &str,
) -> Option<Decimal> {
    let spot = record.definition.spot_venue()?;
    let perp = record.definition.perp_venue()?;
    state
        .market
        .opportunities()
        .into_iter()
        .find(|opportunity| {
            opportunity.kind == OpportunityKind::FundingRate
                && opportunity.symbol == symbol
                && opportunity.long_venue == spot
                && opportunity.short_venue == perp
        })
        .and_then(|opportunity| opportunity.funding_rate)
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
