use arb_core::{
    BacktestReport, CostAssumptions, DataRules, EntryRules, ExecutionRules, ExitRules,
    FundingSample, Instrument, StrategyDefinition, StrategyEventLevel, StrategyExecutionMode,
    StrategyKind, StrategyRecord, StrategyRisk, StrategySource, StrategyStatus, StrategyUniverse,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    state::AppState,
    store::{StrategyRun, StrategyRunEvent, StrategyVersion},
};

pub async fn list(State(state): State<AppState>) -> Json<Vec<StrategyRecord>> {
    // 列表以数据库为准：没有保存过策略时返回空列表。
    let mut strategies = state.store.load_strategies().await;
    // 运行时指标只在内存里更新、按需落库；读列表时合并回来，避免「最近评估」长时间不动。
    // 数据库里有而内存里缺失的记录顺手补回内存，避免列表能点、详情和启停却报「策略不存在」。
    {
        let mut live = state.strategies.write().expect("策略锁不可用");
        for record in &mut strategies {
            match live.iter_mut().find(|item| item.id == record.id) {
                Some(current) => {
                    record.runtime = current.runtime.clone();
                    record.last_heartbeat_at = current.last_heartbeat_at;
                }
                None => live.push(record.clone()),
            }
        }
    }
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
        None => code_definition(&state, body.name.unwrap_or_default()).await,
    };
    if definition.kind == StrategyKind::FundingRate {
        return Err(ApiError::BadRequest(
            "内置资金费率套利（现货-永续）已下线，请使用永续-永续资金费率策略".to_string(),
        ));
    }
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

/// 代码优先策略的默认定义：交易所与交易对来自数据库配置，具体逻辑由策略代码决定。
async fn code_definition(state: &AppState, name: String) -> StrategyDefinition {
    let limits = state.risk_limits();
    let venues = state.store.exchange_venues().await;
    let spot = venues
        .iter()
        .find(|venue| venue.segment == "spot")
        .map(|venue| venue.venue.clone());
    let perp = venues
        .iter()
        .find(|venue| venue.segment == "perp")
        .map(|venue| venue.venue.clone());

    let mut universe_venues = Vec::new();
    if let Some(spot) = spot {
        universe_venues.push(spot);
    }
    if let Some(perp) = &perp {
        universe_venues.push(perp.clone());
    }

    let mut symbols: Vec<String> = match perp {
        Some(perp) => state
            .store
            .tradable_symbols()
            .await
            .into_iter()
            .filter(|row| row.venue == perp)
            .map(|row| row.symbol)
            .take(2)
            .collect(),
        None => Vec::new(),
    };
    if symbols.is_empty() {
        symbols = state.config.market_symbols.clone();
    }
    if symbols.is_empty() {
        symbols.push("BTCUSDT".to_string());
    }

    StrategyDefinition {
        name,
        // 代码优先策略由沙箱决定入场与出场，不再受内置资金费率规则约束。
        kind: StrategyKind::Custom,
        universe: StrategyUniverse {
            venues: universe_venues,
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
            leverage: Decimal::ONE,
            max_slippage_bps: Decimal::from(10),
            cooldown_secs: 300,
        },
        data: DataRules::default(),
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
    let previous = record.status;

    if body.status == StrategyStatus::Running {
        // 沙箱模块或技能参数都可以承载运行，两者都没有时无法启动。
        if record.sandbox.is_none() && record.skill.is_none() {
            return Err(ApiError::Conflict(
                "请先部署策略代码或配置技能参数再启动".to_string(),
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

    // 启动时建立运行实例，暂停/停止时关闭，便于按实例回看运行情况。
    if body.status == StrategyStatus::Running && record.runtime.run_id.is_none() {
        let mut run = StrategyRun::new(&record);
        // 上币公告跟随没有交易对组合，把当次生效的规则存进实例，之后改配置也不会改写历史。
        run.note = listing_note(&state, &record).await;
        state.store.save_strategy_run(&run).await;
        record.runtime.run_id = Some(run.id);
    } else if previous == StrategyStatus::Running && body.status != StrategyStatus::Running {
        close_run(&state, &mut record, body.status).await;
    }

    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

/// 下单模式：真实下单会经统一风控提交给交易所；模拟下单只记录信号与成交事件。
#[derive(Debug, Deserialize)]
pub struct UpdateExecutionModeRequest {
    pub mode: StrategyExecutionMode,
}

/// 切换下单模式。运行中或仍有持仓时禁止切换，避免真实仓位被模拟平掉。
pub async fn update_execution_mode(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateExecutionModeRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;
    if record.execution_mode == body.mode {
        return Ok(Json(record));
    }
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先暂停策略再切换下单模式".to_string()));
    }
    if record.runtime.open.is_some() {
        return Err(ApiError::Conflict(
            "策略仍有未平仓位，不能切换下单模式".to_string(),
        ));
    }

    record.execution_mode = body.mode;
    record.updated_at = Utc::now();
    let label = match body.mode {
        StrategyExecutionMode::Real => "真实下单",
        StrategyExecutionMode::Paper => "模拟下单",
    };
    record.runtime.record(
        StrategyEventLevel::Info,
        format!("下单模式已切换为{label}"),
        None,
    );

    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

/// 关闭当前运行实例：记录停止时间与最终状态，等待下一次启动重新建实例。
pub(crate) async fn close_run(
    state: &AppState,
    record: &mut StrategyRecord,
    status: StrategyStatus,
) {
    let Some(run_id) = record.runtime.run_id.take() else {
        return;
    };
    if let Some(mut run) = state.store.strategy_run(run_id).await {
        run.status = status;
        run.stopped_at = Some(Utc::now());
        run.last_heartbeat_at = Some(Utc::now());
        state.store.save_strategy_run(&run).await;
    }
}

#[derive(Debug, Serialize)]
pub struct StrategyRunsResponse {
    pub runs: Vec<StrategyRun>,
    /// 最近一次运行实例的事件，按时间正序。
    pub events: Vec<StrategyRunEvent>,
}

/// 运行实例与事件：交易所组合、状态、心跳与最近发生的信号/订单/错误。
pub async fn runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StrategyRunsResponse>, ApiError> {
    find(&state, id)?;
    let runs = state.store.strategy_runs(id, 20).await;
    let events = match runs.first() {
        Some(run) => state.store.strategy_run_events(run.id, 50).await,
        None => Vec::new(),
    };
    Ok(Json(StrategyRunsResponse { runs, events }))
}

#[derive(Debug, Serialize)]
pub struct ClearRunsResponse {
    pub removed: usize,
}

/// 清空历史运行实例：运行中的实例保留，已结束的实例一并清掉。
pub async fn clear_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ClearRunsResponse>, ApiError> {
    find(&state, id)?;
    let removed = state.store.clear_strategy_runs(id).await;
    Ok(Json(ClearRunsResponse { removed }))
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
    if body.definition.kind == StrategyKind::FundingRate {
        return Err(ApiError::BadRequest(
            "内置资金费率套利（现货-永续）已下线，请使用永续-永续资金费率策略".to_string(),
        ));
    }
    record.apply_definition(body.definition);
    record.validation = record.definition.validate(&state.risk_limits());

    replace(&state, &record);
    persist(&state, &record).await;
    Ok(Json(record))
}

#[derive(Debug, Deserialize)]
pub struct UpdateScopeRequest {
    /// 现货腿交易所，例如 `binance-spot`。
    pub spot_venue: String,
    /// 永续腿交易所，例如 `binance`。
    pub perp_venue: String,
    pub symbols: Vec<String>,
    /// 事件来源白名单；`None` 表示不修改，空数组表示接受全部来源。
    #[serde(default)]
    pub event_sources: Option<Vec<String>>,
    /// 事件关键词；`None` 表示不修改，空数组表示不按关键词过滤。
    #[serde(default)]
    pub event_keywords: Option<Vec<String>>,
    /// 事件窗口（秒）；`None` 表示不修改。
    #[serde(default)]
    pub event_window_secs: Option<u64>,
}

/// 修改策略的应用范围（交易所与代币）。产生新版本并清空旧校验与回测结果。
pub async fn update_scope(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateScopeRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut record = find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再修改应用范围".to_string()));
    }

    let spot_venue = body.spot_venue.trim();
    let perp_venue = body.perp_venue.trim();
    if !spot_venue.ends_with("-spot") {
        return Err(ApiError::BadRequest(
            "现货交易所标识必须以 -spot 结尾".to_string(),
        ));
    }
    if perp_venue.is_empty() || perp_venue.ends_with("-spot") || perp_venue == spot_venue {
        return Err(ApiError::BadRequest(
            "永续交易所标识不能为空，也不能与现货腿相同".to_string(),
        ));
    }

    let mut symbols: Vec<String> = Vec::new();
    for symbol in &body.symbols {
        let symbol = symbol.trim().to_uppercase();
        if !symbol.is_empty() && !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
    }
    if symbols.is_empty() {
        return Err(ApiError::BadRequest("请至少选择一个交易对".to_string()));
    }

    record.definition.universe = StrategyUniverse {
        venues: vec![spot_venue.to_string(), perp_venue.to_string()],
        symbols,
        instruments: vec![Instrument::Spot, Instrument::Perp],
    };

    // 事件配置只影响沙箱输入，和交易范围一起保存，避免再开一个接口。
    if let Some(sources) = body.event_sources {
        record.definition.data.event_sources = sources
            .into_iter()
            .map(|source| source.trim().to_lowercase())
            .filter(|source| !source.is_empty())
            .collect();
    }
    if let Some(keywords) = body.event_keywords {
        record.definition.data.event_keywords = keywords
            .into_iter()
            .map(|keyword| keyword.trim().to_string())
            .filter(|keyword| !keyword.is_empty())
            .collect();
    }
    if let Some(window) = body.event_window_secs {
        if !(60..=604_800).contains(&window) {
            return Err(ApiError::BadRequest(
                "事件窗口必须在 60 到 604800 秒之间".to_string(),
            ));
        }
        record.definition.data.event_window_secs = window;
    }

    record.version += 1;
    record.validation = record.definition.validate(&state.risk_limits());
    record.last_backtest = None;
    record.updated_at = Utc::now();
    record.runtime.record(
        arb_core::StrategyEventLevel::Info,
        format!(
            "应用范围已更新：{} / {} · {}",
            spot_venue,
            perp_venue,
            record.definition.universe.symbols.join(", ")
        ),
        None,
    );

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

/// 回测：内置资金费率策略回放历史费率，自定义策略逐根 K 线重放沙箱模块。
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
    // 技能策略是 `Custom` 但没有沙箱模块，回测走 SKILL.md 规则重放。
    if crate::skill::manages(&record) {
        let report = crate::skill_backtest::run(&state, &record, body.days)
            .await
            .map_err(ApiError::Conflict)?;
        record.last_backtest = Some(report.clone());
        record.updated_at = Utc::now();
        replace(&state, &record);
        persist(&state, &record).await;
        return Ok(Json(report));
    }
    if record.definition.kind == StrategyKind::Custom {
        let report = crate::wasm_backtest::run(&state, &record, body.days)
            .await
            .map_err(ApiError::Conflict)?;
        record.last_backtest = Some(report.clone());
        record.updated_at = Utc::now();
        replace(&state, &record);
        persist(&state, &record).await;
        return Ok(Json(report));
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

    // 先删数据库再清内存：库删除失败时不会留下「列表看得到、操作报不存在」的幽灵策略。
    state.store.delete_strategy(id).await;
    state
        .strategies
        .write()
        .expect("策略锁不可用")
        .retain(|existing| existing.id != id);
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

/// 事件/指标驱动策略的运行快照：
/// - 公告类：启用的规则（监听来源/关键词 → 执行交易所）；
/// - 指标类：启用的规则（周期、通道/均线参数、方向 → 执行交易所）；
/// - 网格：启用的规则（档位数、间距、每档比例 → 执行交易所）。
/// 其它策略没有这类规则，返回 None 时运行实例就不显示规则行。
pub(crate) async fn listing_note(state: &AppState, record: &StrategyRecord) -> Option<String> {
    if !crate::skill::manages(record) {
        return None;
    }
    let settings = crate::skill::settings_for(state, record);
    let rules: Vec<String> = settings
        .enabled_rules()
        .map(|rule| {
            let execution = if rule.steps.execution_venues.is_empty() {
                "全部交易所".to_string()
            } else {
                rule.steps.execution_venues.join(" / ")
            };
            let condition = match rule.kind {
                crate::skill::SkillKind::Event => {
                    let sources = if rule.trigger.event_sources.is_empty() {
                        "全部来源".to_string()
                    } else {
                        rule.trigger.event_sources.join(" / ")
                    };
                    let keywords = match rule.trigger.event_keywords.as_slice() {
                        [] => "内置关键词".to_string(),
                        [only] => only.clone(),
                        [first, ..] => {
                            format!(
                                "{} 等 {} 个关键词",
                                first,
                                rule.trigger.event_keywords.len()
                            )
                        }
                    };
                    format!("{sources}（{keywords}）")
                }
                crate::skill::SkillKind::Breakout => format!(
                    "{} · 通道 {} · ATR {}×",
                    rule.trigger.timeframe, rule.trigger.channel, rule.trigger.atr_multiple
                ),
                crate::skill::SkillKind::MaTrend => format!(
                    "{} · 均线 {}/{}",
                    rule.trigger.timeframe, rule.trigger.fast, rule.trigger.slow
                ),
                crate::skill::SkillKind::Grid => format!(
                    "{} · {} 档 · 间距 {}% · 每档 {}%",
                    rule.trigger.timeframe,
                    rule.business.grid_levels,
                    rule.business.grid_spacing_pct,
                    rule.business.grid_share_pct
                ),
            };
            match rule.name.trim() {
                "" => format!("{condition} → {execution}"),
                name => format!("{name}：{condition} → {execution}"),
            }
        })
        .collect();
    (!rules.is_empty()).then(|| rules.join("\n"))
}

/// 规则变更后刷新当前运行实例的规则快照。
///
/// 历史实例保留启动时的规则，运行中的实例展示此刻真正生效的规则，
/// 这样在页面上停止/启动某条规则时能立即反映到「运行实例」。
pub(crate) async fn refresh_run_note(state: &AppState, strategy_id: Uuid) {
    let Ok(record) = find(state, strategy_id) else {
        return;
    };
    if record.status != StrategyStatus::Running {
        return;
    }
    let Some(run_id) = record.runtime.run_id else {
        return;
    };
    if let Some(mut run) = state.store.strategy_run(run_id).await {
        if run.status != StrategyStatus::Running {
            return;
        }
        run.note = listing_note(state, &record).await;
        state.store.save_strategy_run(&run).await;
    }
}
