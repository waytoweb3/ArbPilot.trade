//! 技能策略：桌面端按策略实例读取与修改 SKILL.md 六章参数。

use std::time::Duration;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use arb_core::{SandboxCandle, StrategyRecord};

use crate::{
    error::ApiError,
    news::SOURCES,
    routes::strategies,
    skill::{self, SkillKind, SkillSettings},
    skill_check::{self, Costs},
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

    let sources = event_sources();
    let execution = available_execution_venues(&state);
    for rule in &mut settings.rules {
        if rule.id.is_nil() {
            rule.id = Uuid::new_v4();
        }
        rule.name = rule.name.trim().to_string();
        rule.purpose = rule.purpose.trim().to_string();
        rule.trigger.timeframe = rule.trigger.timeframe.trim().to_ascii_lowercase();
        rule.trigger.event_sources =
            normalize(std::mem::take(&mut rule.trigger.event_sources), &sources);
        rule.trigger.event_keywords = keywords(std::mem::take(&mut rule.trigger.event_keywords));
        rule.steps.execution_venues =
            normalize(std::mem::take(&mut rule.steps.execution_venues), &execution);
    }

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
    let mut venues: Vec<String> = state
        .accounts
        .list()
        .into_iter()
        .filter(|account| account.can_trade)
        .map(|account| account.venue.trim().to_lowercase())
        .filter(|venue| !venue.is_empty())
        .collect();
    venues.sort();
    venues.dedup();
    venues
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
