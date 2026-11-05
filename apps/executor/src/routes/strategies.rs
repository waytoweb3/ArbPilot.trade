use arb_core::{
    BacktestReport, CostAssumptions, EntryRules, ExecutionRules, ExitRules, FundingSample,
    Instrument, OpportunityKind, StrategyDefinition, StrategyExecutionMode, StrategyRecord,
    StrategyRisk, StrategySource, StrategyStatus, StrategyUniverse,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState, store::StrategyVersion};

pub async fn list(State(state): State<AppState>) -> Json<Vec<StrategyRecord>> {
    let mut strategies = state.strategies.read().expect("策略锁不可用").clone();
    strategies.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Json(strategies)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StrategyRecord>, ApiError> {
    find(&state, id).map(Json)
}

#[derive(Debug, Deserialize)]
pub struct CreateStrategyRequest {
    /// 兼容直接提交结构化定义的调用方；桌面端只提交名称，由代码定义逻辑。
    #[serde(default)]
    pub definition: Option<StrategyDefinition>,
    pub name: Option<String>,
    /// 来源：AI 生成或用户手写，默认按用户手写记录。
    pub source: Option<StrategySource>,
}

pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateStrategyRequest>,
) -> Result<(StatusCode, Json<StrategyRecord>), ApiError> {
    let definition = match body.definition {
        Some(definition) => definition,
        None => code_definition(&state, body.name.unwrap_or_default()),
    };
    let mut record = StrategyRecord::new(
        definition,
        body.source.unwrap_or(StrategySource::Manual),
        StrategyStatus::Draft,
    );
    record.validation = record.definition.validate(&state.risk_limits());

    {
        let mut strategies = state.strategies.write().expect("策略锁不可用");
        strategies.push(record.clone());
    }
    persist(&state, &record).await;

    Ok((StatusCode::CREATED, Json(record)))
}

/// 代码优先策略的默认定义：提供可交易范围与执行限额，具体逻辑由策略代码决定。
fn code_definition(state: &AppState, name: String) -> StrategyDefinition {
    let limits = state.risk_limits();
    let symbols = if state.config.market_symbols.is_empty() {
        vec!["BTCUSDT".to_string()]
    } else {
        state.config.market_symbols.clone()
    };
    StrategyDefinition {
        name,
        kind: OpportunityKind::FundingRate,
        universe: StrategyUniverse {
            venues: vec!["binance-spot".to_string(), "binance".to_string()],
            symbols,
            instruments: vec![Instrument::Spot, Instrument::Perp],
        },
        entry: EntryRules {
            funding_spread_min: Decimal::new(3, 4),
            net_edge_min: Decimal::new(1, 4),
        },
        exit: ExitRules {
            spread_below: Decimal::new(2, 4),
            max_holding_hours: Decimal::from(24),
        },
        risk: StrategyRisk {
            max_notional_usd: Decimal::from(1000).min(limits.max_order_notional_usd),
            max_unhedged_seconds: 3,
        },
        execution: ExecutionRules {
            mode: StrategyExecutionMode::Live,
            leverage: Decimal::ONE,
            max_slippage_bps: Decimal::from(10),
            cooldown_secs: 300,
        },
        costs: CostAssumptions::default(),
        notes: None,
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateStrategyRequest {
    pub status: StrategyStatus,
}

/// 启停策略。运行前会重新校验，避免用过期校验结果启动已被改坏的定义。
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStrategyRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;

    if body.status == StrategyStatus::Running {
        if record.sandbox.is_none() {
            return Err(ApiError::Conflict(
                "请先编译并部署策略代码再启动".to_string(),
            ));
        }
        record.validation = record.definition.validate(&state.risk_limits());
        if record.validation.has_errors() {
            return Err(ApiError::Conflict(
                "策略校验未通过，请先修复错误再启动".to_string(),
            ));
        }
    }

    record.status = body.status;
    record.last_heartbeat_at = Some(Utc::now());
    record.updated_at = Utc::now();
    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

#[derive(Debug, Deserialize)]
pub struct UpdateDefinitionRequest {
    pub definition: StrategyDefinition,
}

/// 更新定义会产生新版本，并清空旧校验与旧回测结果。
pub async fn update_definition(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateDefinitionRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再修改定义".to_string()));
    }
    record.apply_definition(body.definition);
    record.validation = record.definition.validate(&state.risk_limits());

    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

pub async fn validate(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;
    record.validation = record.definition.validate(&state.risk_limits());
    record.updated_at = Utc::now();
    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

#[derive(Debug, Default, Deserialize)]
pub struct BacktestRequest {
    /// 回放窗口（天），默认 30 天，最多 365 天。
    pub days: Option<u32>,
}

/// 用已入库的历史资金费率回放当前定义，成本与假设写在报告里。
pub async fn backtest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<BacktestRequest>,
) -> Result<Json<BacktestReport>, ApiError> {
    let mut record = find(&state, id)?;
    record.validation = record.definition.validate(&state.risk_limits());
    if record.validation.has_errors() {
        return Err(ApiError::Conflict("策略校验未通过，无法回测".to_string()));
    }

    let perp_venue = record
        .definition
        .perp_venue()
        .ok_or_else(|| ApiError::BadRequest("策略缺少永续腿，无法确定资金费率来源".to_string()))?
        .to_string();
    let days = body.days.unwrap_or(30).clamp(1, 365) as i64;
    let since = Utc::now() - Duration::days(days);

    let mut samples = Vec::new();
    for symbol in &record.definition.universe.symbols {
        let rates = state
            .store
            .funding_rates(&perp_venue, Some(symbol), 20_000)
            .await
            .map_err(ApiError::from)?;
        samples.extend(
            rates
                .into_iter()
                .filter(|rate| rate.funding_time >= since)
                .map(|rate| FundingSample {
                    symbol: rate.symbol,
                    at: rate.funding_time,
                    rate: rate.funding_rate,
                }),
        );
    }

    let report = arb_core::run_backtest(&record.definition, &samples, record.version);
    record.last_backtest = Some(report.clone());
    record.updated_at = Utc::now();
    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(report))
}

pub async fn versions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StrategyVersion>>, ApiError> {
    find(&state, id)?;
    Ok(Json(state.store.strategy_versions(id).await))
}

#[derive(Debug, Deserialize)]
pub struct RollbackRequest {
    pub version: u32,
}

/// 回滚到指定版本：以该版本定义生成一个新版本，历史版本保持不变。
pub async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RollbackRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;
    let versions = state.store.strategy_versions(id).await;
    let target = versions
        .iter()
        .find(|version| version.version == body.version)
        .ok_or_else(|| ApiError::NotFound(format!("策略 {id} 不存在版本 {}", body.version)))?;

    record.apply_definition(target.definition.clone());
    record.validation = record.definition.validate(&state.risk_limits());

    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

/// 删除策略定义与版本历史。已有仓位与订单保留，不影响查看与手动处置。
pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let record = find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再删除".to_string()));
    }
    if record.runtime.open.is_some() {
        return Err(ApiError::Conflict(
            "策略仍有未平仓位，请先平仓再删除".to_string(),
        ));
    }

    state
        .strategies
        .write()
        .expect("策略锁不可用")
        .retain(|existing| existing.id != id);
    state.store.delete_strategy(id).await;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn find(state: &AppState, id: Uuid) -> Result<StrategyRecord, ApiError> {
    state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .find(|record| record.id == id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("策略 {id} 不存在")))
}

pub(crate) fn replace(state: &AppState, record: &StrategyRecord) {
    let mut strategies = state.strategies.write().expect("策略锁不可用");
    match strategies
        .iter_mut()
        .find(|existing| existing.id == record.id)
    {
        Some(existing) => *existing = record.clone(),
        None => strategies.push(record.clone()),
    }
}

/// 持久化策略快照与对应版本；同一版本重复写入时更新为最新校验/回测结果。
pub(crate) async fn persist(state: &AppState, record: &StrategyRecord) {
    state.store.save_strategy(record).await;
    state.store.save_strategy_version(record).await;
}
