//! 技能策略：桌面端按策略实例读取与修改 SKILL.md 六章参数。

use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use arb_core::{SandboxCandle, StrategyRecord};

use crate::{
    error::ApiError,
    news::SOURCES,
    routes::strategies,
    skill::{self, SkillKind, SkillRule, SkillSettings},
    skill_check::{self, Costs, ValidationReport},
    skill_library::{self, ParamsSchema, RuleDiff, Skill, SkillVersion},
    state::AppState,
};

/// 保存前置校验拉取 K 线的超时；超时按「无样本」处理，不阻断保存。
const CANDLE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
pub struct SettingsQuery {
    pub strategy_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct SkillSettingsResponse {
    pub strategy_id: Uuid,
    pub settings: SkillSettings,
    /// 可监听的公告来源。
    pub available_event_sources: Vec<String>,
    /// 当前具备下单凭证的交易所。
    pub available_execution_venues: Vec<String>,
    /// 公告类技能的默认触发关键词，新增规则时作为初始值。
    pub default_event_keywords: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub strategy_id: Uuid,
    #[serde(flatten)]
    pub settings: SkillSettings,
}

#[derive(Debug, Deserialize)]
pub struct TestEventRequest {
    pub strategy_id: Uuid,
    /// 从新闻缓存里挑一条公告试跑；与 source + title 二选一。
    #[serde(default)]
    pub news_id: Option<String>,
    /// 公告来源，例如 `binance`。
    #[serde(default)]
    pub source: Option<String>,
    /// 公告标题，代币代码与关键词都从这里解析。
    #[serde(default)]
    pub title: Option<String>,
    /// 只试跑某一条规则；缺省时试跑全部启用的公告规则。
    #[serde(default)]
    pub rule_id: Option<Uuid>,
    /// 覆盖下单交易对，只用于验证下单链路，例如 `BTCUSDT`。
    #[serde(default)]
    pub symbol: Option<String>,
    /// 只返回匹配结果，不开仓；用于批量校验关键词。
    #[serde(default)]
    pub dry_run: bool,
    /// 按规则配置真实下单；默认关闭，仍然经过风控。
    #[serde(default)]
    pub live: bool,
    /// 只在一个指定交易所尝试，避免自动落到其他交易所。
    #[serde(default)]
    pub venue: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestEventResponse {
    pub strategy_id: Uuid,
    pub news_id: Option<String>,
    pub source: String,
    pub title: String,
    pub results: Vec<skill::EventTestResult>,
}

/// 用一条公告试跑：绕过启动基线与事件窗口，命中后强制模拟开仓。
///
/// 用于验证「来源 → 关键词 → 代币解析 → 交易所优先级 → 下单」整条链路；
/// 不会写入去重记录、不会消耗当日开仓次数，也不会提交真实订单。
pub async fn test_event(
    State(state): State<AppState>,
    Json(body): Json<TestEventRequest>,
) -> Result<Json<TestEventResponse>, ApiError> {
    let record = find_strategy(&state, body.strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;

    let (news_id, source, title) = match body
        .news_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => {
            let item = state
                .news
                .snapshot(1_000)
                .items
                .into_iter()
                .find(|item| item.id == id)
                .ok_or_else(|| ApiError::NotFound(format!("新闻 {id} 不在缓存里，可能已被清理")))?;
            (Some(item.id), item.source, item.title)
        }
        None => {
            let source = body.source.unwrap_or_default().trim().to_lowercase();
            let title = body.title.unwrap_or_default().trim().to_string();
            if source.is_empty() || title.is_empty() {
                return Err(ApiError::BadRequest(
                    "请提供 news_id，或同时提供 source 与 title".to_string(),
                ));
            }
            (None, source, title)
        }
    };

    let symbol = body
        .symbol
        .as_deref()
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty());
    let results = skill::test_event(
        &state,
        &record,
        &source,
        &title,
        body.rule_id,
        symbol,
        body.dry_run,
        body.live,
        body.venue.as_deref(),
    )
    .await
    .map_err(ApiError::BadRequest)?;
    Ok(Json(TestEventResponse {
        strategy_id: body.strategy_id,
        news_id,
        source,
        title,
        results,
    }))
}

pub async fn settings(
    State(state): State<AppState>,
    Query(query): Query<SettingsQuery>,
) -> Json<SkillSettingsResponse> {
    match find_strategy(&state, query.strategy_id) {
        Some(record) => Json(response(&state, &record)),
        None => Json(SkillSettingsResponse {
            strategy_id: query.strategy_id,
            settings: state.skill.default_settings(),
            available_event_sources: event_sources(),
            available_execution_venues: available_execution_venues(&state),
            default_event_keywords: skill::default_event_keywords(),
        }),
    }
}

/// 保存某个策略实例的技能参数：保存后立即对后续公告生效，无需重启执行器。
///
/// 写入前先跑六维校验，任一维度出现 `Error` 就拒绝保存并回传完整报告。
pub async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSettingsRequest>,
) -> Result<Json<SkillSettingsResponse>, ApiError> {
    let record = find_strategy(&state, body.strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    let mut settings = body.settings;
    if settings.rules.is_empty() {
        return Err(ApiError::BadRequest("至少保留一条技能规则".to_string()));
    }
    // 前端编辑参数时不回传来源字段；按规则 ID 从已保存设置里补齐，避免溯源丢失。
    let previous = skill::settings_for(&state, &record);
    for rule in &mut settings.rules {
        if let Some(existing) = previous.rules.iter().find(|item| item.id == rule.id) {
            rule.source_skill_id = rule.source_skill_id.or(existing.source_skill_id);
            rule.source_skill_version = rule.source_skill_version.or(existing.source_skill_version);
            rule.source_rule_id = rule.source_rule_id.or(existing.source_rule_id);
        }
    }
    normalize_settings(&state, &mut settings);

    // 保存前置校验：六维校验是唯一的判定入口。
    // 任一维度出现 Error 就拒绝写入，并把六维报告随 422 一起返回。
    let limits = state.risk_limits();
    let costs = Costs {
        fee_bps_per_leg: record.definition.costs.fee_bps_per_leg,
        slippage_bps: record.definition.costs.slippage_bps,
    };
    // 关掉的规则只保留配置，允许先停用再慢慢改参数。
    let enabled: Vec<_> = settings.rules.iter().filter(|rule| rule.enabled).collect();
    let labeled = enabled.len() > 1;
    let mut report = skill_check::Report::default();
    for rule in enabled {
        let candles = history_candles(&state, &record, rule).await;
        for mut issue in skill_check::evaluate_rule(rule, &limits, &candles, costs).issues {
            // 多条规则时在问题上标出规则名，否则界面无法判断是哪条规则出的错。
            if labeled {
                issue.message = format!("{}：{}", rule.name, issue.message);
            }
            report.push(issue);
        }
    }
    if report.has_errors() {
        tracing::warn!(
            strategy_id = %body.strategy_id,
            errors = report.errors(),
            "技能参数未通过六维校验，拒绝保存"
        );
        return Err(ApiError::SkillValidation(Box::new(report.view())));
    }

    let saved = state
        .skill
        .update_settings(body.strategy_id, settings)
        .await;
    tracing::info!(
        strategy_id = %body.strategy_id,
        rules = saved.rules.len(),
        "技能策略参数已更新"
    );
    // 运行中的实例同步刷新规则快照。
    strategies::refresh_run_note(&state, body.strategy_id).await;
    Ok(Json(response(&state, &record)))
}

fn find_strategy(state: &AppState, strategy_id: Uuid) -> Option<StrategyRecord> {
    state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .find(|record| record.id == strategy_id)
        .cloned()
}

/// 统一规整规则：补 ID、去空白、只保留当前可用的公告来源与执行交易所。
fn normalize_settings(state: &AppState, settings: &mut SkillSettings) {
    let sources = event_sources();
    let execution = available_execution_venues(state);
    for rule in &mut settings.rules {
        normalize_rule(rule, &sources, &execution);
    }
}

fn normalize_rule(rule: &mut SkillRule, sources: &[String], execution: &[String]) {
    if rule.id.is_nil() {
        rule.id = Uuid::new_v4();
    }
    rule.name = rule.name.trim().to_string();
    rule.purpose = rule.purpose.trim().to_string();
    rule.trigger.timeframe = rule.trigger.timeframe.trim().to_ascii_lowercase();
    rule.trigger.event_sources =
        normalize(std::mem::take(&mut rule.trigger.event_sources), sources);
    rule.trigger.event_keywords = keywords(std::mem::take(&mut rule.trigger.event_keywords));
    rule.steps.execution_venues =
        normalize(std::mem::take(&mut rule.steps.execution_venues), execution);
}

/// 保存前置校验用的历史 K 线。
///
/// 取策略的第一个交易对，按规则优先级找第一个有报价的交易所；公告类技能不需要。
/// 拉取失败不阻断保存，回测维度会以「未提供历史 K 线」告警提示。
async fn history_candles(
    state: &AppState,
    record: &StrategyRecord,
    rule: &skill::SkillRule,
) -> Vec<SandboxCandle> {
    if rule.kind == SkillKind::Event {
        return Vec::new();
    }
    let symbols = if record.definition.universe.symbols.is_empty() {
        state.config.market_symbols.clone()
    } else {
        record.definition.universe.symbols.clone()
    };
    let Some(symbol) = symbols.first() else {
        return Vec::new();
    };
    let limit = rule.trigger.lookback.clamp(3, 1_000);
    for venue in skill::candidate_venues(state, rule, &record.definition.universe.venues) {
        if state.market.quote(&venue, symbol).is_none() {
            continue;
        }
        // 保存是交互操作：行情不可用时宁可少校验一项，也不要让保存卡住。
        let Ok(fetched) = tokio::time::timeout(
            CANDLE_TIMEOUT,
            state
                .market
                .candles(&venue, symbol, &rule.trigger.timeframe, limit),
        )
        .await
        else {
            continue;
        };
        let Ok(candles) = fetched else {
            continue;
        };
        if candles.len() < 3 {
            continue;
        }
        return candles
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
    }
    Vec::new()
}

fn response(state: &AppState, record: &arb_core::StrategyRecord) -> SkillSettingsResponse {
    SkillSettingsResponse {
        strategy_id: record.id,
        // 已保存的设置优先；未保存时按触发方式回落，上币公告跟随会迁移历史设置。
        settings: skill::settings_for(state, record),
        available_event_sources: event_sources(),
        available_execution_venues: available_execution_venues(state),
        default_event_keywords: skill::default_event_keywords(),
    }
}

fn event_sources() -> Vec<String> {
    SOURCES.iter().map(|source| source.to_string()).collect()
}

/// 当前有下单凭证的交易所，去空、去重并统一小写。
fn available_execution_venues(state: &AppState) -> Vec<String> {
    crate::listing::tradable_venues(state)
}

/// 只保留可用项，按用户设定的顺序去重；清空表示不限。
fn normalize(values: Vec<String>, available: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        let item = value.trim().to_lowercase();
        if item.is_empty() || result.contains(&item) || !available.contains(&item) {
            continue;
        }
        result.push(item);
    }
    result
}

/// 触发关键词：去空白、忽略大小写去重，保留用户输入的展示形式。
fn keywords(values: Vec<String>) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        let item = value.trim().to_string();
        if item.is_empty() || result.iter().any(|saved| saved.eq_ignore_ascii_case(&item)) {
            continue;
        }
        result.push(item);
    }
    result
}

// ---------------------------------------------------------------------------
// 技能库：独立 Skill 与不可变版本
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SkillListQuery {
    pub author_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillRequest {
    pub author_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub settings: Option<SkillSettings>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub params_schema: Option<ParamsSchema>,
    #[serde(default)]
    pub changelog: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVersionRequest {
    #[serde(default)]
    pub settings: Option<SkillSettings>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub params_schema: Option<ParamsSchema>,
    #[serde(default)]
    pub changelog: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidateSkillRequest {
    #[serde(default)]
    pub settings: Option<SkillSettings>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub params_schema: Option<ParamsSchema>,
    #[serde(default)]
    pub version: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub from: u32,
    pub to: u32,
}

#[derive(Debug, Serialize)]
pub struct SkillDetail {
    pub skill: Skill,
    pub version: Option<SkillVersion>,
}

fn find_skill(skills: Vec<Skill>, skill_id: Uuid) -> Result<Skill, ApiError> {
    skills
        .into_iter()
        .find(|skill| skill.id == skill_id)
        .ok_or_else(|| ApiError::NotFound(format!("技能 {skill_id} 不存在")))
}

/// 把提交内容解析成技能设置：结构化参数优先，其次解析 SKILL.md。
fn parse_skill_settings(
    state: &AppState,
    settings: Option<SkillSettings>,
    markdown: Option<String>,
) -> Result<SkillSettings, ApiError> {
    if let Some(settings) = settings {
        return Ok(settings);
    }
    if let Some(text) = markdown.filter(|text| !text.trim().is_empty()) {
        let evaluation = skill_check::evaluate(&text, &state.risk_limits(), &[], Costs::default());
        if let Some(rule) = evaluation.rule {
            return Ok(SkillSettings { rules: vec![rule] });
        }
        return Err(ApiError::SkillValidation(Box::new(
            evaluation.report.view(),
        )));
    }
    Err(ApiError::BadRequest(
        "请提供技能参数或 SKILL.md 内容".to_string(),
    ))
}

/// 技能库没有策略定义，校验时用第一个可用交易对与交易所取样本 K 线。
async fn skill_candles(state: &AppState, rule: &SkillRule) -> Vec<SandboxCandle> {
    if rule.kind == SkillKind::Event {
        return Vec::new();
    }
    let Some(symbol) = state.config.market_symbols.first() else {
        return Vec::new();
    };
    let limit = rule.trigger.lookback.clamp(3, 1_000);
    let mut venues: Vec<String> = rule.steps.execution_venues.clone();
    venues.extend(
        state
            .store
            .exchange_venues()
            .await
            .into_iter()
            .map(|venue| venue.venue),
    );
    for venue in venues {
        if state.market.quote(&venue, symbol).is_none() {
            continue;
        }
        let Ok(fetched) = tokio::time::timeout(
            CANDLE_TIMEOUT,
            state
                .market
                .candles(&venue, symbol, &rule.trigger.timeframe, limit),
        )
        .await
        else {
            continue;
        };
        let Ok(candles) = fetched else {
            continue;
        };
        if candles.len() < 3 {
            continue;
        }
        return candles
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
    }
    Vec::new()
}

/// 对一组规则跑七维校验；只校验启用的规则。
async fn validate_settings(
    state: &AppState,
    settings: &SkillSettings,
    schema: &ParamsSchema,
) -> ValidationReport {
    validate_with_backtests(state, settings, schema).await.0
}

/// 七维校验 + 每条启用规则的回测快照；快照只用于展示，不参与门禁判定。
async fn validate_with_backtests(
    state: &AppState,
    settings: &SkillSettings,
    schema: &ParamsSchema,
) -> (ValidationReport, Vec<RuleBacktest>) {
    let limits = state.risk_limits();
    let enabled: Vec<&SkillRule> = settings.rules.iter().filter(|rule| rule.enabled).collect();
    let labeled = enabled.len() > 1;
    let mut report = skill_check::Report::default();
    let mut backtests = Vec::new();
    for rule in enabled {
        let candles = skill_candles(state, rule).await;
        for mut issue in
            skill_check::evaluate_rule(rule, &limits, &candles, Costs::default()).issues
        {
            if labeled {
                issue.message = format!("{}：{}", rule.name, issue.message);
            }
            report.push(issue);
        }
        backtests.extend(backtest_snapshot(state, rule, &candles));
    }
    report.extend(skill_check::params::check(schema, settings));
    (report.view(), backtests)
}

/// 用与校验同一批 K 线跑一遍回放，返回收益、回撤等指标；公告类与样本不足时不产出。
fn backtest_snapshot(
    state: &AppState,
    rule: &SkillRule,
    candles: &[SandboxCandle],
) -> Option<RuleBacktest> {
    if rule.kind == SkillKind::Event || candles.len() < 3 {
        return None;
    }
    let notional = skill::capped_notional(rule);
    if notional <= Decimal::ZERO {
        return None;
    }
    let symbol = state
        .config
        .market_symbols
        .first()
        .cloned()
        .unwrap_or_default();
    let days = ((candles[candles.len() - 1].open_time - candles[0].open_time).num_seconds()
        / 86_400)
        .max(1);
    let trades = crate::skill_backtest::replay(rule, &symbol, candles, notional, Costs::default());
    let metrics = crate::skill_backtest::summarize(&trades, notional, days);
    Some(RuleBacktest {
        rule_id: rule.id,
        name: rule.name.clone(),
        symbol,
        trades: metrics.trades,
        wins: metrics.wins,
        losses: metrics.losses,
        win_rate_pct: metrics.win_rate_pct,
        net_pnl_usd: metrics.net_pnl_usd,
        return_pct: metrics.return_pct,
        max_drawdown_pct: metrics.max_drawdown_pct,
        fees_usd: metrics.fees_usd,
        slippage_usd: metrics.slippage_usd,
        trades_per_day: metrics.trades_per_day,
    })
}

pub async fn list_skills(
    State(state): State<AppState>,
    Query(query): Query<SkillListQuery>,
) -> Json<Vec<Skill>> {
    let mut skills = state.store.skills().await;
    if let Some(author_id) = query.author_id {
        skills.retain(|skill| skill.author_id == author_id);
    }
    Json(skills)
}

pub async fn create_skill(
    State(state): State<AppState>,
    Json(body): Json<CreateSkillRequest>,
) -> Result<(StatusCode, Json<SkillDetail>), ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("技能名称不能为空".to_string()));
    }
    let mut settings = parse_skill_settings(&state, body.settings, body.markdown)?;
    if settings.rules.is_empty() {
        return Err(ApiError::BadRequest("至少需要一条技能规则".to_string()));
    }
    normalize_settings(&state, &mut settings);

    let mut skill = Skill::new(body.author_id, body.name.trim(), body.description.trim());
    skill.latest_version = 1;
    skill_library::tag_rules(&mut settings, skill.id, 1);
    let schema = body
        .params_schema
        .unwrap_or_else(|| ParamsSchema::from_settings(&settings));
    let report = validate_settings(&state, &settings, &schema).await;
    let version = SkillVersion::new(
        skill.id,
        1,
        settings,
        schema,
        serde_json::to_value(&report).unwrap_or_default(),
        report.passed,
        body.changelog.unwrap_or_else(|| "初始版本".to_string()),
    );

    state.store.save_skill(&skill).await;
    state.store.save_skill_version(&version).await;
    Ok((
        StatusCode::CREATED,
        Json(SkillDetail {
            skill,
            version: Some(version),
        }),
    ))
}

pub async fn skill_detail(
    State(state): State<AppState>,
    Path(skill_id): Path<Uuid>,
) -> Result<Json<SkillDetail>, ApiError> {
    let skill = find_skill(state.store.skills().await, skill_id)?;
    let version = state
        .store
        .skill_version(skill_id, skill.latest_version)
        .await;
    Ok(Json(SkillDetail { skill, version }))
}

pub async fn skill_versions(
    State(state): State<AppState>,
    Path(skill_id): Path<Uuid>,
) -> Result<Json<Vec<SkillVersion>>, ApiError> {
    find_skill(state.store.skills().await, skill_id)?;
    Ok(Json(state.store.skill_versions(skill_id).await))
}

pub async fn create_skill_version(
    State(state): State<AppState>,
    Path(skill_id): Path<Uuid>,
    Json(body): Json<CreateVersionRequest>,
) -> Result<(StatusCode, Json<SkillVersion>), ApiError> {
    let mut skill = find_skill(state.store.skills().await, skill_id)?;
    let mut settings = parse_skill_settings(&state, body.settings, body.markdown)?;
    if settings.rules.is_empty() {
        return Err(ApiError::BadRequest("至少需要一条技能规则".to_string()));
    }
    normalize_settings(&state, &mut settings);

    // SKILL.md 重新解析会生成新规则 ID；按名称对齐上一版本，让差异对比落在「修改」而不是
    // 「删除 + 新增」，也让订阅者升级时能继续匹配到同一条规则实例。
    if let Some(previous) = state
        .store
        .skill_version(skill_id, skill.latest_version)
        .await
    {
        for rule in &mut settings.rules {
            if let Some(existing) = previous
                .settings
                .rules
                .iter()
                .find(|item| item.name == rule.name)
            {
                rule.id = existing.id;
                rule.source_rule_id = existing.source_rule_id.or(Some(existing.id));
            }
        }
    }

    let next = skill.latest_version.saturating_add(1).max(1);
    skill_library::tag_rules(&mut settings, skill_id, next);
    let schema = body
        .params_schema
        .unwrap_or_else(|| ParamsSchema::from_settings(&settings));
    let report = validate_settings(&state, &settings, &schema).await;
    let version = SkillVersion::new(
        skill_id,
        next,
        settings,
        schema,
        serde_json::to_value(&report).unwrap_or_default(),
        report.passed,
        body.changelog.unwrap_or_default(),
    );

    skill.latest_version = next;
    skill.updated_at = Utc::now();
    state.store.save_skill_version(&version).await;
    // 版本不可变：并发请求可能已经写入了同号版本，此时不覆盖对方，也不推进 latest_version。
    if state
        .store
        .skill_version(skill_id, next)
        .await
        .map(|saved| saved.settings_sha256 != version.settings_sha256)
        .unwrap_or(true)
    {
        return Err(ApiError::Conflict(
            "该版本号已被占用，请刷新后重试".to_string(),
        ));
    }
    state.store.save_skill(&skill).await;
    Ok((StatusCode::CREATED, Json(version)))
}

/// 单条规则的回测快照：与六维报告一起返回，发布前能直接看到收益与回撤。
#[derive(Debug, Serialize)]
pub struct RuleBacktest {
    pub rule_id: Uuid,
    pub name: String,
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: Decimal,
    pub net_pnl_usd: Decimal,
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub fees_usd: Decimal,
    pub slippage_usd: Decimal,
    pub trades_per_day: Decimal,
}

/// 技能校验结果：六维报告保持原有结构，新增回测快照数组。
#[derive(Debug, Serialize)]
pub struct SkillValidationResponse {
    #[serde(flatten)]
    pub report: ValidationReport,
    pub backtests: Vec<RuleBacktest>,
}

pub async fn validate_skill(
    State(state): State<AppState>,
    Path(skill_id): Path<Uuid>,
    Json(body): Json<ValidateSkillRequest>,
) -> Result<Json<SkillValidationResponse>, ApiError> {
    find_skill(state.store.skills().await, skill_id)?;
    let (settings, schema) = if body.settings.is_some() || body.markdown.is_some() {
        let mut settings = parse_skill_settings(&state, body.settings, body.markdown)?;
        normalize_settings(&state, &mut settings);
        let schema = body
            .params_schema
            .unwrap_or_else(|| ParamsSchema::from_settings(&settings));
        (settings, schema)
    } else if let Some(version) = body.version {
        let stored = state
            .store
            .skill_version(skill_id, version)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("技能版本 v{version} 不存在")))?;
        let schema = body
            .params_schema
            .unwrap_or_else(|| stored.params_schema.clone());
        (stored.settings, schema)
    } else {
        return Err(ApiError::BadRequest(
            "请提供技能参数、SKILL.md 或版本号".to_string(),
        ));
    };
    let (report, backtests) = validate_with_backtests(&state, &settings, &schema).await;
    Ok(Json(SkillValidationResponse { report, backtests }))
}

pub async fn diff_skill_versions(
    State(state): State<AppState>,
    Path(skill_id): Path<Uuid>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<RuleDiff>, ApiError> {
    find_skill(state.store.skills().await, skill_id)?;
    let from = state
        .store
        .skill_version(skill_id, query.from)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("技能版本 v{} 不存在", query.from)))?;
    let to = state
        .store
        .skill_version(skill_id, query.to)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("技能版本 v{} 不存在", query.to)))?;
    Ok(Json(skill_library::diff_rules(
        &from.settings,
        &to.settings,
    )))
}

// ---------------------------------------------------------------------------
// 规则实例：基于策略建规则、来源标记、差异与手动升级
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub rule: SkillRule,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub rule: Option<SkillRule>,
}

#[derive(Debug, Deserialize)]
pub struct UpgradeRequest {
    pub version: u32,
}

#[derive(Debug, Deserialize)]
pub struct StrategyDiffQuery {
    pub version: u32,
}

pub async fn strategy_rules(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
) -> Result<Json<SkillSettings>, ApiError> {
    let record = find_strategy(&state, strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    if record.skill.is_none() {
        return Err(ApiError::Conflict("该策略未绑定技能参数".to_string()));
    }
    Ok(Json(skill::settings_for(&state, &record)))
}

pub async fn create_strategy_rule(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
    Json(body): Json<CreateRuleRequest>,
) -> Result<(StatusCode, Json<SkillSettings>), ApiError> {
    let record = find_strategy(&state, strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    let binding = record
        .skill
        .clone()
        .ok_or_else(|| ApiError::Conflict("该策略未绑定技能参数".to_string()))?;

    let mut rule = body.rule;
    let sources = event_sources();
    let execution = available_execution_venues(&state);
    normalize_rule(&mut rule, &sources, &execution);
    rule.source_rule_id = Some(rule.id);
    if let Some(skill_id) = binding.skill_id {
        rule.source_skill_id = Some(skill_id);
        rule.source_skill_version = binding.skill_version;
    }

    if rule.enabled {
        ensure_rule_valid(&state, &rule).await?;
    }

    let mut settings = skill::settings_for(&state, &record);
    settings.rules.push(rule);
    let saved = state.skill.update_settings(strategy_id, settings).await;
    strategies::refresh_run_note(&state, strategy_id).await;
    Ok((StatusCode::CREATED, Json(saved)))
}

pub async fn update_strategy_rule(
    State(state): State<AppState>,
    Path((strategy_id, rule_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateRuleRequest>,
) -> Result<Json<SkillSettings>, ApiError> {
    let record = find_strategy(&state, strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    if record.skill.is_none() {
        return Err(ApiError::Conflict("该策略未绑定技能参数".to_string()));
    }
    let mut settings = skill::settings_for(&state, &record);
    let index = settings
        .rules
        .iter()
        .position(|rule| rule.id == rule_id)
        .ok_or_else(|| ApiError::NotFound(format!("规则 {rule_id} 不存在")))?;

    if let Some(mut rule) = body.rule {
        // 允许改参数与名称，但不允许把来源改成别的技能。
        rule.id = rule_id;
        rule.source_skill_id = settings.rules[index].source_skill_id;
        rule.source_skill_version = settings.rules[index].source_skill_version;
        rule.source_rule_id = settings.rules[index].source_rule_id;
        let sources = event_sources();
        let execution = available_execution_venues(&state);
        normalize_rule(&mut rule, &sources, &execution);
        if rule.enabled {
            ensure_rule_valid(&state, &rule).await?;
        }
        settings.rules[index] = rule;
    }
    if let Some(enabled) = body.enabled {
        settings.rules[index].enabled = enabled;
        if enabled {
            ensure_rule_valid(&state, &settings.rules[index]).await?;
        }
    }

    let saved = state.skill.update_settings(strategy_id, settings).await;
    strategies::refresh_run_note(&state, strategy_id).await;
    Ok(Json(saved))
}

pub async fn diff_strategy_skill(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
    Query(query): Query<StrategyDiffQuery>,
) -> Result<Json<RuleDiff>, ApiError> {
    let record = find_strategy(&state, strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    let skill_id = record
        .skill
        .as_ref()
        .and_then(|binding| binding.skill_id)
        .ok_or_else(|| ApiError::Conflict("该策略未绑定技能库版本".to_string()))?;
    let version = state
        .store
        .skill_version(skill_id, query.version)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("技能版本 v{} 不存在", query.version)))?;
    let current = skill::settings_for(&state, &record);
    Ok(Json(skill_library::diff_rules(&current, &version.settings)))
}

pub async fn upgrade_strategy_skill(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
    Json(body): Json<UpgradeRequest>,
) -> Result<Json<SkillSettings>, ApiError> {
    let mut record = find_strategy(&state, strategy_id)
        .ok_or_else(|| ApiError::NotFound("策略不存在".to_string()))?;
    let skill_id = record
        .skill
        .as_ref()
        .and_then(|binding| binding.skill_id)
        .ok_or_else(|| ApiError::Conflict("该策略未绑定技能库版本".to_string()))?;
    let version = state
        .store
        .skill_version(skill_id, body.version)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("技能版本 v{} 不存在", body.version)))?;
    if !version.passed {
        return Err(ApiError::Conflict(
            "目标技能版本未通过校验，不能升级".to_string(),
        ));
    }

    let current = skill::settings_for(&state, &record);
    let merged = merge_upgrade(&current, &version.settings, skill_id, version.version);
    let binding = record.skill.as_mut().expect("前面已确认技能绑定存在");
    binding.skill_version = Some(version.version);
    binding.skill_sha256 = Some(version.settings_sha256.clone());
    binding.settings =
        Some(serde_json::to_value(&merged).map_err(|error| ApiError::Internal(error.to_string()))?);
    record.updated_at = Utc::now();

    let saved = state.skill.update_settings(strategy_id, merged).await;
    strategies::replace(&state, &record);
    strategies::persist(&state, &record).await;
    strategies::refresh_run_note(&state, strategy_id).await;
    Ok(Json(saved))
}

/// 升级时按来源规则 ID 对齐：新版本内容覆盖参数，本地启用状态保留。
fn merge_upgrade(
    current: &SkillSettings,
    target: &SkillSettings,
    skill_id: Uuid,
    version: u32,
) -> SkillSettings {
    let mut merged = target.clone();
    for rule in &mut merged.rules {
        rule.source_skill_id = Some(skill_id);
        rule.source_skill_version = Some(version);
        if rule.source_rule_id.is_none() {
            rule.source_rule_id = Some(rule.id);
        }
    }

    let by_source: HashMap<Uuid, &SkillRule> = current
        .rules
        .iter()
        .filter_map(|rule| rule.source_rule_id.map(|id| (id, rule)))
        .collect();
    let mut target_sources: HashSet<Uuid> = HashSet::new();
    for rule in &mut merged.rules {
        let source_id = rule.source_rule_id.unwrap_or(rule.id);
        target_sources.insert(source_id);
        if let Some(existing) = by_source.get(&source_id) {
            rule.id = existing.id;
            rule.enabled = existing.enabled;
        }
    }

    // 订阅者自己新增的本地规则不属于这个技能版本，升级时原样保留。
    for rule in &current.rules {
        let from_other = rule.source_skill_id != Some(skill_id);
        let source_id = rule.source_rule_id.unwrap_or(rule.id);
        if from_other && !target_sources.contains(&source_id) {
            merged.rules.push(rule.clone());
        }
    }
    merged
}

async fn ensure_rule_valid(state: &AppState, rule: &SkillRule) -> Result<(), ApiError> {
    let candles = skill_candles(state, rule).await;
    let report = skill_check::evaluate_rule(rule, &state.risk_limits(), &candles, Costs::default());
    if report.has_errors() {
        return Err(ApiError::SkillValidation(Box::new(report.view())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sourced_rule(name: &str, skill_id: Uuid, version: u32) -> SkillRule {
        let mut rule = SkillRule {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ..SkillRule::default()
        };
        rule.source_rule_id = Some(rule.id);
        rule.source_skill_id = Some(skill_id);
        rule.source_skill_version = Some(version);
        rule
    }

    #[test]
    fn upgrade_keeps_local_enabled_and_appends_new_rules() {
        let skill_id = Uuid::new_v4();
        let mut shared = sourced_rule("共享规则", skill_id, 1);
        shared.enabled = false;
        let local = SkillRule {
            id: Uuid::new_v4(),
            name: "本地规则".to_string(),
            ..SkillRule::default()
        };
        let current = SkillSettings {
            rules: vec![shared.clone(), local.clone()],
        };

        let mut target_shared = shared.clone();
        target_shared.source_skill_version = Some(2);
        target_shared.business.notional_usd = rust_decimal::Decimal::from(500);
        let target = SkillSettings {
            rules: vec![target_shared, sourced_rule("v2 新增规则", skill_id, 2)],
        };

        let merged = merge_upgrade(&current, &target, skill_id, 2);
        assert_eq!(merged.rules.len(), 3);

        let kept = merged
            .rules
            .iter()
            .find(|rule| rule.name == "共享规则")
            .expect("共享规则保留");
        assert!(!kept.enabled, "订阅者本地停用状态必须保留");
        assert_eq!(kept.id, shared.id, "规则实例 ID 保持不变");
        assert_eq!(
            kept.business.notional_usd,
            rust_decimal::Decimal::from(500),
            "参数采用新版本内容"
        );
        assert!(merged.rules.iter().any(|rule| rule.name == "v2 新增规则"));
        assert!(merged
            .rules
            .iter()
            .any(|rule| rule.name == "本地规则" && rule.id == local.id));
    }
}
