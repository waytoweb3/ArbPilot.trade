//! 策略分享与付费市场 REST API。
//!
//! - 策略默认私有，只有作者主动发布后才出现在榜单。
//! - 发布前必须通过校验，并有与当前版本一致的回测记录。
//! - 表现记录按来源区分，作者自报回测不能标记为可复现。
//! - 订阅与结算只保存业务金额与引用，不接触任何支付凭证或私钥。

use arb_core::{
    compute_settlement_from_gross, period_bounds, publish_readiness, rank_publications,
    settlement_period, LeaderboardEntry, MarketVisibility, PerformanceSource, PublicationStatus,
    RiskLevel, SandboxArtifact, SandboxArtifactSource, SandboxBinding, SkillBinding,
    SourceVisibility, StrategyAuthorCard, StrategyChangelogEntry, StrategyDefinition,
    StrategyEventLevel, StrategyPerformanceRecord, StrategyPricing, StrategyPublication,
    StrategyRunScope, StrategySettlement, StrategyStatus, StrategySubscription, SubscriptionStatus,
    ValidationSeverity,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    skill_library::{self, ParamIssue, ParamValues, ParamsSchema, SkillVersion},
    state::AppState,
    templates::OFFICIAL_TEMPLATE_KEYS,
};

use super::strategies;

// ---------------------------------------------------------------------------
// 官方策略目录
// ---------------------------------------------------------------------------

/// 官方发布使用稳定的 UUID，重复启动不会产生新记录。
fn official_uuid(name: &str) -> Uuid {
    crate::templates::official_uuid(name)
}

fn official_author_id() -> Uuid {
    crate::templates::official_author_id()
}

fn official_publication_id(key: &str) -> Uuid {
    official_uuid(&format!("https://arbpilot.trade/market/publication/{key}"))
}

fn official_strategy_id(key: &str) -> Uuid {
    official_uuid(&format!("https://arbpilot.trade/market/strategy/{key}"))
}

fn official_risk_level(key: &str) -> RiskLevel {
    match key {
        "listing-follow" => RiskLevel::High,
        _ => RiskLevel::Medium,
    }
}

fn official_data_sources(key: &str) -> Vec<String> {
    match key {
        "listing-follow" => vec!["交易所上币公告".to_string(), "实时盘口".to_string()],
        _ => vec!["历史 K 线".to_string(), "实时盘口".to_string()],
    }
}

/// 官方只读实例启动时把四个内置模板发布到策略市场，供所有用户免费订阅。
///
/// 其他实例（桌面内置执行器、用户自托管）不会自动生成这些发布，市场仍以数据库为准。
pub(crate) async fn seed_official(state: &AppState) {
    if !state.config.public_mode {
        return;
    }

    let snapshot = state.store.market_snapshot().await;
    let author_id = official_author_id();
    let now = Utc::now();
    let mut author = snapshot
        .authors
        .iter()
        .find(|card| card.author_id == author_id)
        .cloned()
        .unwrap_or_else(|| {
            let mut card = StrategyAuthorCard::new("ArbPilot 官方");
            card.author_id = author_id;
            card
        });
    author.display_name = "ArbPilot 官方".to_string();
    author.headline = "官方内置策略，免费开放".to_string();
    author.bio =
        "由 ArbPilot 维护的内置策略模板，全部免费开放；导入后可在自己的执行器上修改参数并运行。"
            .to_string();
    author.updated_at = now;
    state.store.save_author_card(&author).await;

    let limit = state.risk_limits().max_order_notional_usd;
    for template in state
        .store
        .strategy_templates()
        .await
        .into_iter()
        .filter(|template| OFFICIAL_TEMPLATE_KEYS.contains(&template.key.as_str()))
    {
        let existing = snapshot
            .publications
            .iter()
            .find(|publication| publication.id == official_publication_id(&template.key))
            .cloned();

        let definition = template.definition.clone();
        let notes = definition.notes.clone().unwrap_or_default();
        let mut suggested_max_notional_usd = definition.risk.max_notional_usd.min(limit);
        if suggested_max_notional_usd <= Decimal::ZERO {
            suggested_max_notional_usd = Decimal::from(100);
        }
        // 内置模板已补成技能库版本：发布绑定该版本，订阅者导入时按同一份哈希校验。
        let version = crate::templates::builtin_version(state, &template.key).await;
        // Skill 版只交付逻辑骨架 + 参数空间，作者填的具体值不进交付物。
        let schema = version
            .as_ref()
            .map(|version| {
                if version.params_schema.params.is_empty() {
                    ParamsSchema::from_settings(&version.settings)
                } else {
                    version.params_schema.clone()
                }
            })
            .unwrap_or_default();
        let logic = version
            .as_ref()
            .map(|version| skill_library::normalize_settings(&version.settings, &schema));
        let frozen = logic
            .as_ref()
            .and_then(|settings| serde_json::to_value(settings).ok());
        let logic_sha256 = logic
            .as_ref()
            .map(|settings| skill_library::hash_logic(settings, &schema));

        let publication = StrategyPublication {
            id: official_publication_id(&template.key),
            strategy_id: official_strategy_id(&template.key),
            strategy_version: 1,
            author_id,
            author_name: author.display_name.clone(),
            title: definition.name.clone(),
            summary: template.description.clone(),
            logic_summary: format!("{} {}", template.description, notes),
            risk_level: official_risk_level(&template.key),
            suggested_max_notional_usd,
            supported_venues: definition.universe.venues.clone(),
            supported_symbols: definition.universe.symbols.clone(),
            data_sources: official_data_sources(&template.key),
            runtime: "技能参数运行时".to_string(),
            known_limits: vec![
                "需要足够的可用保证金".to_string(),
                "行情或公告源中断时不会开新仓".to_string(),
                "所有订单仍受本地风控与交易所限额约束".to_string(),
            ],
            source_visibility: SourceVisibility::OpenSource,
            visibility: MarketVisibility::Public,
            pricing: StrategyPricing::free(),
            status: existing
                .as_ref()
                .map(|publication| publication.status)
                .unwrap_or(PublicationStatus::Published),
            artifact_sha256: None,
            // 官方模板以「逻辑 + 参数空间」交付，订阅者无需沙箱模块即可运行。
            skill: Some(SkillBinding {
                template_key: template.key.clone(),
                settings: frozen,
                skill_id: version.as_ref().map(|version| version.skill_id),
                skill_version: version.as_ref().map(|version| version.version),
                skill_sha256: logic_sha256.clone(),
                params_schema: serde_json::to_value(&schema).ok(),
                param_constraints: None,
            }),
            skill_id: version.as_ref().map(|version| version.skill_id),
            skill_version: version.as_ref().map(|version| version.version),
            skill_sha256: logic_sha256,
            definition: Some(definition),
            changelog: existing
                .as_ref()
                .map(|publication| publication.changelog.clone())
                .filter(|changelog| !changelog.is_empty())
                .unwrap_or_else(|| {
                    vec![StrategyChangelogEntry {
                        version: 1,
                        at: now,
                        summary: "官方内置策略，免费开放".to_string(),
                    }]
                }),
            created_at: existing
                .as_ref()
                .map(|publication| publication.created_at)
                .unwrap_or(now),
            updated_at: now,
        };

        if publication.has_errors() {
            tracing::error!(key = %template.key, "官方策略发布未通过校验，已跳过");
            continue;
        }
        state.store.save_publication(&publication).await;
    }
}

// ---------------------------------------------------------------------------
// 作者卡片
// ---------------------------------------------------------------------------

pub async fn list_authors(State(state): State<AppState>) -> Json<Vec<StrategyAuthorCard>> {
    let mut authors = state.store.market_snapshot().await.authors;
    authors.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    Json(authors)
}

#[derive(Debug, Deserialize)]
pub struct CreateAuthorRequest {
    pub display_name: String,
    #[serde(default)]
    pub headline: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub payout_account: Option<String>,
}

pub async fn create_author(
    State(state): State<AppState>,
    Json(body): Json<CreateAuthorRequest>,
) -> Result<(StatusCode, Json<StrategyAuthorCard>), ApiError> {
    let mut card = StrategyAuthorCard::new(body.display_name.trim());
    card.headline = body.headline.unwrap_or_default();
    card.bio = body.bio.unwrap_or_default();
    card.payout_account = body.payout_account.filter(|value| !value.trim().is_empty());

    ensure_valid(&card.validate())?;
    state.store.save_author_card(&card).await;
    Ok((StatusCode::CREATED, Json(card)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateAuthorRequest {
    pub display_name: Option<String>,
    pub headline: Option<String>,
    pub bio: Option<String>,
    pub payout_account: Option<String>,
}

pub async fn update_author(
    State(state): State<AppState>,
    Path(author_id): Path<Uuid>,
    Json(body): Json<UpdateAuthorRequest>,
) -> Result<Json<StrategyAuthorCard>, ApiError> {
    let mut card = state
        .store
        .market_snapshot()
        .await
        .authors
        .into_iter()
        .find(|card| card.author_id == author_id)
        .ok_or_else(|| ApiError::NotFound(format!("作者 {author_id} 不存在")))?;

    if let Some(value) = body.display_name {
        card.display_name = value.trim().to_string();
    }
    if let Some(value) = body.headline {
        card.headline = value;
    }
    if let Some(value) = body.bio {
        card.bio = value;
    }
    if let Some(value) = body.payout_account {
        card.payout_account = (!value.trim().is_empty()).then_some(value);
    }
    card.updated_at = Utc::now();

    ensure_valid(&card.validate())?;
    state.store.save_author_card(&card).await;
    Ok(Json(card))
}

// ---------------------------------------------------------------------------
// 发布
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct PublicationQuery {
    pub author_id: Option<Uuid>,
    pub strategy_id: Option<Uuid>,
    /// 是否包含私有与已撤回记录；仅在作者自己的设备上使用。
    #[serde(default)]
    pub include_hidden: bool,
}

pub async fn list_publications(
    State(state): State<AppState>,
    Query(query): Query<PublicationQuery>,
) -> Json<Vec<StrategyPublication>> {
    let snapshot = state.store.market_snapshot().await;
    let mut publications: Vec<StrategyPublication> = snapshot
        .publications
        .into_iter()
        .filter(|publication| {
            query
                .author_id
                .map(|id| publication.author_id == id)
                .unwrap_or(true)
        })
        .filter(|publication| {
            query
                .strategy_id
                .map(|id| publication.strategy_id == id)
                .unwrap_or(true)
        })
        .filter(|publication| query.include_hidden || publication.is_market_visible())
        .collect();
    publications.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Json(publications)
}

#[derive(Debug, Deserialize)]
pub struct CreatePublicationRequest {
    pub strategy_id: Uuid,
    pub author_id: Uuid,
    pub title: String,
    pub summary: String,
    pub logic_summary: String,
    pub risk_level: RiskLevel,
    pub suggested_max_notional_usd: Decimal,
    #[serde(default)]
    pub supported_venues: Option<Vec<String>>,
    #[serde(default)]
    pub supported_symbols: Option<Vec<String>>,
    #[serde(default)]
    pub data_sources: Option<Vec<String>>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub known_limits: Option<Vec<String>>,
    pub source_visibility: SourceVisibility,
    pub visibility: MarketVisibility,
    pub pricing: StrategyPricing,
    /// 技能库版本：提供时以该版本冻结规则发布，策略定义仍来自 `strategy_id`。
    #[serde(default)]
    pub skill_id: Option<Uuid>,
    #[serde(default)]
    pub skill_version: Option<u32>,
    /// 作者加在参数空间之上的更严格约束；必须落在版本参数空间内。
    #[serde(default)]
    pub param_constraints: Option<ParamsSchema>,
}

pub async fn create_publication(
    State(state): State<AppState>,
    Json(body): Json<CreatePublicationRequest>,
) -> Result<(StatusCode, Json<StrategyPublication>), ApiError> {
    let snapshot = state.store.market_snapshot().await;
    let author = snapshot
        .authors
        .iter()
        .find(|card| card.author_id == body.author_id)
        .ok_or_else(|| ApiError::NotFound(format!("作者 {} 不存在", body.author_id)))?;
    let record = find_strategy(&state, body.strategy_id)?;
    // 订阅导入的策略属于他人发布，不允许再次对外发布。
    if record.source_publication_id.is_some() {
        return Err(ApiError::Conflict("订阅导入的策略不能再次发布".to_string()));
    }
    let mut skill_parts = None;
    let skill_version = match (body.skill_id, body.skill_version) {
        (Some(skill_id), Some(version)) => {
            let skill = state
                .store
                .skills()
                .await
                .into_iter()
                .find(|skill| skill.id == skill_id)
                .ok_or_else(|| ApiError::NotFound(format!("技能 {skill_id} 不存在")))?;
            if skill.author_id != body.author_id {
                return Err(ApiError::Forbidden(
                    "只能发布自己创建的技能版本".to_string(),
                ));
            }
            let version = state
                .store
                .skill_version(skill_id, version)
                .await
                .ok_or_else(|| {
                    ApiError::NotFound(format!("技能 {skill_id} 的版本 v{version} 不存在"))
                })?;
            if !version.passed {
                return Err(ApiError::Conflict(
                    "技能版本未通过校验，不能发布".to_string(),
                ));
            }
            skill_parts = Some(skill_bundle_parts(&version, body.param_constraints.as_ref())?);
            Some(version)
        }
        (None, None) => None,
        _ => {
            return Err(ApiError::BadRequest(
                "skill_id 与 skill_version 必须同时提供".to_string(),
            ))
        }
    };
    if skill_version.is_none() {
        // 市场只卖 Skill：发布必须绑定技能库版本，参数空间随版本交付。
        return Err(ApiError::BadRequest(
            "请选择要发布的技能与版本".to_string(),
        ));
    }

    let now = Utc::now();
    let runtime = body.runtime.unwrap_or_else(|| {
        if let Some(binding) = record.sandbox.as_ref() {
            format!("WASM 沙箱 · {}", &binding.artifact_hash[..12])
        } else if record.skill.is_some() {
            "技能参数运行时".to_string()
        } else {
            "受限数据定义".to_string()
        }
    });
    let data_sources = body
        .data_sources
        .unwrap_or_else(|| vec!["已结算资金费率".to_string(), "实时盘口".to_string()]);
    let changelog = vec![StrategyChangelogEntry {
        version: record.version,
        at: now,
        summary: "初始发布".to_string(),
    }];

    let mut publication = StrategyPublication {
        id: Uuid::new_v4(),
        strategy_id: record.id,
        strategy_version: record.version,
        author_id: author.author_id,
        author_name: author.display_name.clone(),
        title: body.title.trim().to_string(),
        summary: body.summary.trim().to_string(),
        logic_summary: body.logic_summary.trim().to_string(),
        risk_level: body.risk_level,
        suggested_max_notional_usd: body.suggested_max_notional_usd,
        supported_venues: body
            .supported_venues
            .unwrap_or_else(|| record.definition.universe.venues.clone()),
        supported_symbols: body
            .supported_symbols
            .unwrap_or_else(|| record.definition.universe.symbols.clone()),
        data_sources,
        runtime,
        known_limits: body.known_limits.unwrap_or_default(),
        source_visibility: body.source_visibility,
        visibility: body.visibility,
        pricing: body.pricing,
        // 新建发布始终是私有草稿，发布需要显式调用 publish。
        status: PublicationStatus::Private,
        artifact_sha256: None,
        skill: skill_parts.as_ref().map(|parts| SkillBinding {
            template_key: "skill".to_string(),
            settings: serde_json::to_value(&parts.logic).ok(),
            skill_id: skill_version.as_ref().map(|version| version.skill_id),
            skill_version: skill_version.as_ref().map(|version| version.version),
            skill_sha256: Some(parts.sha256.clone()),
            params_schema: serde_json::to_value(&parts.schema).ok(),
            param_constraints: body
                .param_constraints
                .as_ref()
                .and_then(|constraints| serde_json::to_value(constraints).ok()),
        }),
        skill_id: skill_version.as_ref().map(|version| version.skill_id),
        skill_version: skill_version.as_ref().map(|version| version.version),
        skill_sha256: skill_parts.as_ref().map(|parts| parts.sha256.clone()),
        definition: None,
        changelog,
        created_at: now,
        updated_at: now,
    };
    // 作者建议仓位不能超过执行器本地风控，否则订阅者也无法执行。
    let local_limit = state.risk_limits().max_order_notional_usd;
    if publication.suggested_max_notional_usd > local_limit {
        publication.suggested_max_notional_usd = local_limit;
    }

    ensure_valid(&publication.validate())?;
    state.store.save_publication(&publication).await;
    Ok((StatusCode::CREATED, Json(publication)))
}

pub async fn publication_detail(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StrategyPublication>, ApiError> {
    find_publication(&state, id).await.map(Json)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationAction {
    Publish,
    Withdraw,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePublicationRequest {
    pub action: Option<PublicationAction>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub logic_summary: Option<String>,
    pub risk_level: Option<RiskLevel>,
    pub suggested_max_notional_usd: Option<Decimal>,
    pub known_limits: Option<Vec<String>>,
    pub visibility: Option<MarketVisibility>,
    pub pricing: Option<StrategyPricing>,
    /// 重新发布时记录版本变更说明。
    #[serde(default)]
    pub changelog_summary: Option<String>,
}

pub async fn update_publication(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdatePublicationRequest>,
) -> Result<Json<StrategyPublication>, ApiError> {
    let mut publication = find_publication(&state, id).await?;

    if let Some(value) = body.title {
        publication.title = value.trim().to_string();
    }
    if let Some(value) = body.summary {
        publication.summary = value.trim().to_string();
    }
    if let Some(value) = body.logic_summary {
        publication.logic_summary = value.trim().to_string();
    }
    if let Some(value) = body.risk_level {
        publication.risk_level = value;
    }
    if let Some(value) = body.suggested_max_notional_usd {
        publication.suggested_max_notional_usd = value;
    }
    if let Some(value) = body.known_limits {
        publication.known_limits = value;
    }
    if let Some(value) = body.visibility {
        publication.visibility = value;
    }
    if let Some(value) = body.pricing {
        publication.pricing = value;
    }
    publication.updated_at = Utc::now();

    match body.action {
        Some(PublicationAction::Publish) => {
            // 重新读取策略当前版本，确保发布的是通过校验且代码已部署的版本。
            let mut record = find_strategy(&state, publication.strategy_id)?;
            if record.source_publication_id.is_some() {
                return Err(ApiError::Conflict("订阅导入的策略不能再次发布".to_string()));
            }
            record.validation = record.definition.validate(&state.config.risk);
            let readiness = publish_readiness(&record, &record.validation);
            ensure_valid(&readiness)?;

            // 作者卡片信息可能已更新，发布时同步展示名。
            if let Some(author) = state
                .store
                .market_snapshot()
                .await
                .authors
                .iter()
                .find(|card| card.author_id == publication.author_id)
            {
                publication.author_name = author.display_name.clone();
            }
            // 冻结发布版本的运行载体：沙箱模块或技能参数，订阅者据此在自己机器上运行。
            publication.artifact_sha256 = record
                .sandbox
                .as_ref()
                .map(|binding| binding.artifact_hash.clone());
            // 绑定技能库版本时，发布冻结的是该不可变版本，而不是策略当前参数。
            if let Some(skill_id) = publication.skill_id {
                let version_no = publication
                    .skill_version
                    .ok_or_else(|| ApiError::Conflict("发布记录缺少技能版本号".to_string()))?;
                let version = state
                    .store
                    .skill_version(skill_id, version_no)
                    .await
                    .ok_or_else(|| {
                        ApiError::NotFound(format!("技能 {skill_id} 的版本 v{version_no} 不存在"))
                    })?;
                if !version.passed {
                    return Err(ApiError::Conflict(
                        "技能版本未通过校验，不能发布".to_string(),
                    ));
                }
                // 约束在创建发布时确定，重新发布沿用同一组约束。
                let retained = publication
                    .skill
                    .as_ref()
                    .and_then(|binding| binding.param_constraints.clone());
                let constraints = schema_from_value(retained.as_ref())?;
                let parts = skill_bundle_parts(&version, constraints.as_ref())?;
                publication.skill_sha256 = Some(parts.sha256.clone());
                publication.skill = Some(SkillBinding {
                    template_key: "skill".to_string(),
                    settings: serde_json::to_value(&parts.logic).ok(),
                    skill_id: Some(skill_id),
                    skill_version: Some(version.version),
                    skill_sha256: Some(parts.sha256),
                    params_schema: serde_json::to_value(&parts.schema).ok(),
                    param_constraints: retained,
                });
            } else {
                // 路径 A：技能参数以作者当前生效的规则为准。
                publication.skill = record.skill.clone().map(|mut binding| {
                    if let Ok(value) =
                        serde_json::to_value(crate::skill::settings_for(&state, &record))
                    {
                        binding.settings = Some(value);
                    }
                    binding
                });
            }
            if publication.artifact_sha256.is_none() && publication.skill.is_none() {
                return Err(ApiError::Conflict(
                    "策略既未部署沙箱模块也未配置技能参数，无法发布".to_string(),
                ));
            }
            publication.definition = Some(record.definition.clone());
            publication.attach_version(&record, body.changelog_summary);
            publication.status = PublicationStatus::Published;

            // 发布只影响市场曝光；策略仍由作者自己的执行器继续运行。
        }
        Some(PublicationAction::Withdraw) => {
            // 只撤回市场曝光；已有仓位、订单与安全处置能力保持不变。
            publication.status = PublicationStatus::Withdrawn;

            // 撤回后回到用户自己的执行器运行。
            let mut record = find_strategy(&state, publication.strategy_id)?;
            if record.run_scope != StrategyRunScope::SelfHosted {
                record.run_scope = StrategyRunScope::SelfHosted;
                strategies::close_run(&state, &mut record, StrategyStatus::Stopped).await;
                strategies::replace(&state, &record);
                strategies::persist(&state, &record).await;
            }
        }
        None => {}
    }

    ensure_valid(&publication.validate())?;
    state.store.save_publication(&publication).await;
    Ok(Json(publication))
}

// ---------------------------------------------------------------------------
// 表现记录与榜单
// ---------------------------------------------------------------------------

pub async fn leaderboard(State(state): State<AppState>) -> Json<Vec<LeaderboardEntry>> {
    let snapshot = state.store.market_snapshot().await;
    Json(rank_publications(
        &snapshot.publications,
        &snapshot.performance,
        &snapshot.subscriptions,
    ))
}

pub async fn list_performance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StrategyPerformanceRecord>>, ApiError> {
    find_publication(&state, id).await?;
    let mut records: Vec<StrategyPerformanceRecord> = state
        .store
        .market_snapshot()
        .await
        .performance
        .into_iter()
        .filter(|record| record.publication_id == id)
        .collect();
    records.sort_by(|left, right| right.recorded_at.cmp(&left.recorded_at));
    Ok(Json(records))
}

#[derive(Debug, Deserialize)]
pub struct CreatePerformanceRequest {
    pub source: PerformanceSource,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub trades: u32,
    pub win_rate_pct: Decimal,
    pub net_pnl_usd: Decimal,
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    pub fee_bps_per_leg: Decimal,
    pub slippage_bps: Decimal,
    pub sample_size: u32,
    #[serde(default)]
    pub reproducible: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn create_performance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreatePerformanceRequest>,
) -> Result<(StatusCode, Json<StrategyPerformanceRecord>), ApiError> {
    let publication = find_publication(&state, id).await?;
    // 作者自报记录一律不可标记为可复现，避免污染榜单。
    let reproducible = match body.source {
        PerformanceSource::AuthorBacktest => false,
        _ => body
            .reproducible
            .unwrap_or_else(|| body.source.is_platform_verified()),
    };

    let record = StrategyPerformanceRecord {
        id: Uuid::new_v4(),
        publication_id: publication.id,
        strategy_id: publication.strategy_id,
        strategy_version: publication.strategy_version,
        source: body.source,
        period_start: body.period_start,
        period_end: body.period_end,
        trades: body.trades,
        win_rate_pct: body.win_rate_pct,
        net_pnl_usd: body.net_pnl_usd,
        return_pct: body.return_pct,
        max_drawdown_pct: body.max_drawdown_pct,
        fee_bps_per_leg: body.fee_bps_per_leg,
        slippage_bps: body.slippage_bps,
        sample_size: body.sample_size,
        reproducible,
        recorded_at: Utc::now(),
        note: body.note,
    };

    ensure_valid(&record.validate())?;
    state.store.save_performance(&record).await;
    Ok((StatusCode::CREATED, Json(record)))
}

// ---------------------------------------------------------------------------
// 订阅
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SubscriptionQuery {
    pub subscriber_id: Option<Uuid>,
    pub publication_id: Option<Uuid>,
}

pub async fn list_subscriptions(
    State(state): State<AppState>,
    Query(query): Query<SubscriptionQuery>,
) -> Json<Vec<StrategySubscription>> {
    let mut subscriptions = state.store.market_snapshot().await.subscriptions;
    subscriptions.retain(|subscription| {
        query
            .subscriber_id
            .map(|id| subscription.subscriber_id == id)
            .unwrap_or(true)
            && query
                .publication_id
                .map(|id| subscription.publication_id == id)
                .unwrap_or(true)
    });
    subscriptions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Json(subscriptions)
}

#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    /// 订阅者身份；桌面端本地生成并持久化，不上传任何账号密码。
    pub subscriber_id: Uuid,
    #[serde(default)]
    pub payment_ref: Option<String>,
}

pub async fn subscribe(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SubscribeRequest>,
) -> Result<(StatusCode, Json<StrategySubscription>), ApiError> {
    let publication = find_publication(&state, id).await?;
    if publication.status != PublicationStatus::Published {
        return Err(ApiError::Conflict("该策略当前未发布，无法订阅".to_string()));
    }

    let snapshot = state.store.market_snapshot().await;
    if let Some(existing) = snapshot.subscriptions.iter().find(|subscription| {
        subscription.publication_id == id
            && subscription.subscriber_id == body.subscriber_id
            && (subscription.status.is_entitled()
                || subscription.status == SubscriptionStatus::PendingPayment)
    }) {
        // 重复订阅按幂等处理，避免重复计费。
        return Ok((StatusCode::OK, Json(existing.clone())));
    }

    let subscription = StrategySubscription::new(
        &publication,
        body.subscriber_id,
        body.payment_ref,
        Utc::now(),
    );
    state.store.save_subscription(&subscription).await;
    Ok((StatusCode::CREATED, Json(subscription)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubscriptionRequest {
    pub status: SubscriptionStatus,
    /// 当前计费周期结束时间；`None` 表示保持原值。
    #[serde(default)]
    pub current_period_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub trial_ends_at: Option<DateTime<Utc>>,
    /// 支付服务返回的订单/会话引用，不含任何支付凭证。
    #[serde(default)]
    pub payment_ref: Option<String>,
}

/// 支付服务回调更新订阅状态。
///
/// 只修改订阅授权元数据，不触碰订单、仓位或策略运行状态；即使订阅到期，
/// 已有仓位仍可通过执行器查看、撤单和安全处置。
pub async fn update_subscription(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateSubscriptionRequest>,
) -> Result<Json<StrategySubscription>, ApiError> {
    let mut subscription = state
        .store
        .market_snapshot()
        .await
        .subscriptions
        .into_iter()
        .find(|subscription| subscription.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("订阅 {id} 不存在")))?;

    subscription.status = body.status;
    if let Some(end) = body.current_period_end {
        subscription.current_period_end = Some(end);
    }
    if let Some(end) = body.trial_ends_at {
        subscription.trial_ends_at = Some(end);
    }
    if let Some(reference) = body.payment_ref {
        subscription.payment_ref = Some(reference);
    }
    let now = Utc::now();
    if matches!(
        body.status,
        SubscriptionStatus::Cancelled | SubscriptionStatus::Expired
    ) {
        subscription.cancelled_at.get_or_insert(now);
    } else {
        subscription.cancelled_at = None;
    }
    subscription.updated_at = now;

    state.store.save_subscription(&subscription).await;
    Ok(Json(subscription))
}

pub async fn cancel_subscription(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<StrategySubscription>, ApiError> {
    let mut subscription = state
        .store
        .market_snapshot()
        .await
        .subscriptions
        .into_iter()
        .find(|subscription| subscription.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("订阅 {id} 不存在")))?;

    subscription.cancel(Utc::now());
    state.store.save_subscription(&subscription).await;
    Ok(Json(subscription))
}

// ---------------------------------------------------------------------------
// 订阅交付：把发布版本的代码与定义交付到订阅者自己的执行器
// ---------------------------------------------------------------------------

/// 一次发布的可运行交付物：冻结的定义 + 沙箱模块或「逻辑 + 参数空间」。
#[derive(Debug, Serialize, Deserialize)]
pub struct PublicationBundle {
    pub publication_id: Uuid,
    pub strategy_version: u32,
    pub definition: StrategyDefinition,
    /// 沙箱模块哈希；技能参数交付时为空。
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    /// 沙箱模块字节（Base64）；技能参数交付时为空。
    #[serde(default)]
    pub wasm_base64: Option<String>,
    /// 发布时冻结的技能基线（逻辑骨架与默认值）；存在时订阅者无需沙箱模块即可运行。
    #[serde(default)]
    pub skill: Option<SkillBinding>,
    /// 买家可填的参数空间；与 `skill` 同时出现时按参数空间生成实例。
    #[serde(default)]
    pub params_schema: Option<ParamsSchema>,
}

#[derive(Debug, Default, Deserialize)]
pub struct BundleQuery {
    /// 订阅者身份；作者本人可省略。
    pub subscriber_id: Option<Uuid>,
}

pub async fn publication_bundle(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<BundleQuery>,
) -> Result<Json<PublicationBundle>, ApiError> {
    let publication = find_publication(&state, id).await?;
    ensure_entitled(&state, &publication, query.subscriber_id).await?;
    Ok(Json(load_bundle(&state, &publication).await?))
}

#[derive(Debug, Deserialize)]
pub struct ImportPublicationRequest {
    pub subscriber_id: Uuid,
    /// 买家填写的一组参数；Skill 版缺省时按默认值生成。
    #[serde(default)]
    pub param_values: Option<ParamValues>,
}

#[derive(Debug, Deserialize)]
pub struct ImportBundleRequest {
    #[serde(flatten)]
    pub bundle: PublicationBundle,
    /// 买家填写的一组参数；技能版缺省时按默认值生成。
    #[serde(default)]
    pub param_values: Option<ParamValues>,
}

/// 本地执行器导入官方交付物：官方发布不在本地数据库，因此直接接收交付物本身。
///
/// 模块哈希必须与交付物声明一致，导入后作为一条自托管策略落库，可编辑、配置与执行。
pub async fn import_bundle(
    State(state): State<AppState>,
    Json(body): Json<ImportBundleRequest>,
) -> Result<(StatusCode, Json<arb_core::StrategyRecord>), ApiError> {
    let title = body.bundle.definition.name.clone();
    let record = install_bundle(
        &state,
        body.bundle,
        body.param_values,
        title,
        state.risk_limits().max_order_notional_usd,
        "从策略市场导入",
    )
    .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

/// 订阅者在自己的执行器导入发布版本：校验哈希后落库为一条自托管策略。
pub async fn import_publication(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ImportPublicationRequest>,
) -> Result<(StatusCode, Json<arb_core::StrategyRecord>), ApiError> {
    let publication = find_publication(&state, id).await?;
    ensure_entitled(&state, &publication, Some(body.subscriber_id)).await?;
    let bundle = load_bundle(&state, &publication).await?;

    let title = publication.title.clone();
    let suggested_max_notional_usd = publication.suggested_max_notional_usd;
    let record = install_bundle(
        &state,
        bundle,
        body.param_values,
        title,
        suggested_max_notional_usd,
        "从策略市场导入",
    )
    .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

/// 买家提交自己的参数并生成实例；未填写的参数取默认值。
pub async fn instantiate_publication(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ImportPublicationRequest>,
) -> Result<(StatusCode, Json<arb_core::StrategyRecord>), ApiError> {
    let publication = find_publication(&state, id).await?;
    ensure_entitled(&state, &publication, Some(body.subscriber_id)).await?;
    let bundle = load_bundle(&state, &publication).await?;

    let title = publication.title.clone();
    let suggested_max_notional_usd = publication.suggested_max_notional_usd;
    let record = install_bundle(
        &state,
        bundle,
        body.param_values,
        title,
        suggested_max_notional_usd,
        "从策略市场导入",
    )
    .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

/// 校验交付载体并把冻结定义落库为一条可自托管的策略。
///
/// 交付物可以是沙箱模块，也可以是「逻辑 + 参数空间」；至少要有其一，否则导入后无法运行。
async fn install_bundle(
    state: &AppState,
    bundle: PublicationBundle,
    param_values: Option<ParamValues>,
    title: String,
    suggested_max_notional_usd: Decimal,
    note: &str,
) -> Result<arb_core::StrategyRecord, ApiError> {
    let PublicationBundle {
        publication_id,
        definition: frozen,
        artifact_sha256,
        wasm_base64,
        skill,
        params_schema,
        ..
    } = bundle;

    let sandbox = match (artifact_sha256, wasm_base64) {
        (Some(sha256), Some(encoded)) => {
            let wasm = BASE64
                .decode(encoded.as_bytes())
                .map_err(|error| ApiError::BadRequest(format!("交付代码无法解码: {error}")))?;
            if crate::sandbox::hash_wasm(&wasm) != sha256 {
                return Err(ApiError::Conflict(
                    "交付代码与发布哈希不一致，已拒绝导入".to_string(),
                ));
            }
            let abi_version =
                crate::sandbox::inspect(&wasm, &state.config.sandbox).map_err(|error| {
                    ApiError::BadRequest(format!("交付代码未通过沙箱检查: {error}"))
                })?;

            let short_hash = &sha256[..sha256.len().min(12)];
            // 官方模板与本机内置模板字节完全相同，哈希已入库时直接复用，避免重复插入。
            let artifact = match state
                .store
                .sandbox_artifacts()
                .await
                .into_iter()
                .find(|artifact| artifact.sha256 == sha256)
            {
                Some(artifact) => artifact,
                None => {
                    let artifact = SandboxArtifact {
                        id: Uuid::new_v4(),
                        name: format!("market-{short_hash}"),
                        sha256: sha256.clone(),
                        size_bytes: wasm.len(),
                        abi_version: Some(abi_version),
                        source: SandboxArtifactSource::Uploaded,
                        notes: Some(format!("{note}：{title}")),
                        created_at: Utc::now(),
                    };
                    state
                        .store
                        .save_sandbox_artifact(&artifact, &wasm)
                        .await
                        .map_err(|error| {
                            ApiError::Internal(format!("保存沙箱模块失败: {error}"))
                        })?;
                    artifact
                }
            };
            Some(SandboxBinding::new(&artifact, state.config.sandbox))
        }
        _ => None,
    };

    if sandbox.is_none() && skill.is_none() {
        return Err(ApiError::BadRequest(
            "交付物既不含沙箱模块也不含技能参数，无法导入".to_string(),
        ));
    }

    let mut definition = frozen;
    definition.name = title.clone();
    definition.risk.max_notional_usd = definition
        .risk
        .max_notional_usd
        .min(suggested_max_notional_usd)
        .min(state.risk_limits().max_order_notional_usd);

    let mut record = arb_core::StrategyRecord::new(
        definition,
        arb_core::StrategySource::Manual,
        StrategyStatus::Stopped,
    );
    record.source_publication_id = Some(publication_id);
    record.sandbox = sandbox;
    let mut skill = skill;
    // 参数空间随交付物下发；缺失时回落到绑定里的副本。
    let schema = params_schema
        .or_else(|| {
            skill
                .as_ref()
                .and_then(|binding| binding.params_schema.clone())
                .and_then(|value| serde_json::from_value::<ParamsSchema>(value).ok())
        })
        .unwrap_or_default();
    let mut frozen_skill_settings = skill
        .as_ref()
        .and_then(|binding| binding.settings.clone())
        .map(|value| {
            serde_json::from_value::<crate::skill::SkillSettings>(value)
                .map_err(|error| ApiError::BadRequest(format!("交付技能参数无法解析: {error}")))
        })
        .transpose()?;
    if let (Some(settings), Some(binding)) = (frozen_skill_settings.as_mut(), skill.as_ref()) {
        if let Some(skill_id) = binding.skill_id {
            let expected = binding.skill_sha256.as_deref().ok_or_else(|| {
                ApiError::Conflict("技能库版本缺少逻辑哈希，已拒绝导入".to_string())
            })?;
            crate::skill_library::tag_rules(settings, skill_id, binding.skill_version.unwrap_or(1));
            // 先还原逻辑基线（默认值），再套用买家参数；哈希只覆盖逻辑与参数空间。
            let baseline = crate::skill_library::normalize_settings(settings, &schema);
            let applied = match param_values.as_ref() {
                Some(values) => crate::skill_library::apply_params(&baseline, &schema, values)
                    .map_err(param_error)?,
                None => baseline,
            };
            let actual = crate::skill_library::hash_logic(&applied, &schema);
            if actual != expected {
                return Err(ApiError::Conflict(
                    "技能逻辑与发布哈希不一致，已拒绝导入".to_string(),
                ));
            }
            *settings = applied;
        }
    }
    if let (Some(settings), Some(binding)) = (frozen_skill_settings.as_ref(), skill.as_mut()) {
        binding.settings = Some(
            serde_json::to_value(settings)
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        );
    }
    record.skill = skill;
    // 导入后的规则实例默认停用：订阅者逐条确认参数后再启用，启动门禁仍按「至少一条启用」判断。
    if let Some(settings) = frozen_skill_settings.as_mut() {
        for rule in &mut settings.rules {
            rule.enabled = false;
        }
    }
    record
        .runtime
        .record(StrategyEventLevel::Info, format!("{note}：{title}"), None);
    record.validation = record.definition.validate(&state.risk_limits());

    strategies::replace(state, &record);
    strategies::persist(state, &record).await;
    if let Some(settings) = frozen_skill_settings {
        state.skill.update_settings(record.id, settings).await;
    }
    Ok(record)
}

/// 只有作者本人或持有效订阅的订阅者可以拿到交付物。
async fn ensure_entitled(
    state: &AppState,
    publication: &StrategyPublication,
    subscriber_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(subscriber_id) = subscriber_id else {
        return Err(ApiError::BadRequest("缺少订阅者身份".to_string()));
    };
    if publication.author_id == subscriber_id {
        return Ok(());
    }
    let entitled = state
        .store
        .market_snapshot()
        .await
        .subscriptions
        .iter()
        .any(|subscription| {
            subscription.publication_id == publication.id
                && subscription.subscriber_id == subscriber_id
                && subscription.status.is_entitled()
        });
    if entitled {
        Ok(())
    } else {
        Err(ApiError::PaymentRequired(
            "该发布尚未订阅或订阅已失效".to_string(),
        ))
    }
}

async fn load_bundle(
    state: &AppState,
    publication: &StrategyPublication,
) -> Result<PublicationBundle, ApiError> {
    if !matches!(
        publication.status,
        PublicationStatus::Published | PublicationStatus::Withdrawn
    ) {
        return Err(ApiError::Conflict(
            "该发布尚未公开发布，暂无可交付内容".to_string(),
        ));
    }
    let definition = publication.definition.clone().ok_or_else(|| {
        ApiError::Conflict("该发布缺少冻结的策略定义，请作者重新发布".to_string())
    })?;
    // 「逻辑 + 参数空间」是完整的运行载体，不需要沙箱模块字节。
    if let Some(skill) = publication.skill.clone() {
        let schema = schema_from_value(skill.params_schema.as_ref())?.unwrap_or_default();
        return Ok(PublicationBundle {
            publication_id: publication.id,
            strategy_version: publication.strategy_version,
            definition,
            artifact_sha256: None,
            wasm_base64: None,
            skill: Some(skill),
            params_schema: Some(schema),
        });
    }
    let sha256 = publication.artifact_sha256.clone().ok_or_else(|| {
        ApiError::Conflict("该发布没有可交付的运行载体，请作者重新发布".to_string())
    })?;
    let artifact = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.sha256 == sha256)
        .ok_or_else(|| ApiError::NotFound("发布对应的沙箱模块不存在".to_string()))?;
    let wasm = state
        .store
        .sandbox_module(artifact.id)
        .await
        .ok_or_else(|| ApiError::NotFound("沙箱模块内容缺失".to_string()))?;

    Ok(PublicationBundle {
        publication_id: publication.id,
        strategy_version: publication.strategy_version,
        definition,
        artifact_sha256: Some(sha256),
        wasm_base64: Some(BASE64.encode(wasm)),
        skill: None,
        params_schema: None,
    })
}

// ---------------------------------------------------------------------------
// 结算
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SettlementQuery {
    pub author_id: Option<Uuid>,
}

pub async fn list_settlements(
    State(state): State<AppState>,
    Query(query): Query<SettlementQuery>,
) -> Json<Vec<StrategySettlement>> {
    let mut settlements = state.store.market_snapshot().await.settlements;
    settlements.retain(|settlement| {
        query
            .author_id
            .map(|id| settlement.author_id == id)
            .unwrap_or(true)
    });
    settlements.sort_by(|left, right| right.period.cmp(&left.period));
    Json(settlements)
}

#[derive(Debug, Deserialize)]
pub struct CreateSettlementRequest {
    pub author_id: Uuid,
    /// 结算周期 `YYYY-MM`，默认当前月。
    #[serde(default)]
    pub period: Option<String>,
}

/// 按作者汇总当月付费订阅生成结算单。平台服务费按 bps 扣除。
pub async fn create_settlement(
    State(state): State<AppState>,
    Json(body): Json<CreateSettlementRequest>,
) -> Result<(StatusCode, Json<StrategySettlement>), ApiError> {
    let period = body.period.unwrap_or_else(|| settlement_period(Utc::now()));
    let Some((period_start, period_end)) = period_bounds(&period) else {
        return Err(ApiError::BadRequest(
            "结算周期格式必须为 YYYY-MM".to_string(),
        ));
    };

    let snapshot = state.store.market_snapshot().await;
    if let Some(existing) = snapshot
        .settlements
        .iter()
        .find(|settlement| settlement.author_id == body.author_id && settlement.period == period)
    {
        return Ok((StatusCode::OK, Json(existing.clone())));
    }

    let publication_ids: Vec<Uuid> = snapshot
        .publications
        .iter()
        .filter(|publication| publication.author_id == body.author_id)
        .map(|publication| publication.id)
        .collect();
    if publication_ids.is_empty() {
        return Err(ApiError::NotFound(format!(
            "作者 {} 没有发布记录",
            body.author_id
        )));
    }

    // 买断收入只在购买当月计入一次，避免每个周期重复结算同一笔永久授权。
    let billable: Vec<&StrategySubscription> = snapshot
        .subscriptions
        .iter()
        .filter(|subscription| publication_ids.contains(&subscription.publication_id))
        .filter(|subscription| {
            matches!(
                subscription.status,
                SubscriptionStatus::Active | SubscriptionStatus::Cancelled
            )
        })
        .filter(|subscription| {
            subscription.started_at >= period_start && subscription.started_at < period_end
        })
        .filter(|subscription| subscription.monthly_price_usd > Decimal::ZERO)
        .collect();

    if billable.is_empty() {
        return Err(ApiError::Conflict("该周期没有买断订单可结算".to_string()));
    }

    let gross: Decimal = billable
        .iter()
        .map(|subscription| subscription.monthly_price_usd)
        .sum();
    let settlement = compute_settlement_from_gross(
        body.author_id,
        &period,
        gross,
        billable.len() as u32,
        state.config.market_platform_fee_bps,
    );

    state.store.save_settlement(&settlement).await;
    Ok((StatusCode::CREATED, Json(settlement)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettlementRequest {
    pub status: arb_core::SettlementStatus,
    #[serde(default)]
    pub note: Option<String>,
}

/// 支付服务回调或作者确认后更新结算状态。
pub async fn update_settlement(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateSettlementRequest>,
) -> Result<Json<StrategySettlement>, ApiError> {
    let mut settlement = state
        .store
        .market_snapshot()
        .await
        .settlements
        .into_iter()
        .find(|settlement| settlement.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("结算单 {id} 不存在")))?;

    settlement.status = body.status;
    settlement.note = body.note;
    settlement.updated_at = Utc::now();
    state.store.save_settlement(&settlement).await;
    Ok(Json(settlement))
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

fn find_strategy(state: &AppState, id: Uuid) -> Result<arb_core::StrategyRecord, ApiError> {
    state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .find(|record| record.id == id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("策略 {id} 不存在")))
}

async fn find_publication(state: &AppState, id: Uuid) -> Result<StrategyPublication, ApiError> {
    state
        .store
        .market_snapshot()
        .await
        .publications
        .into_iter()
        .find(|publication| publication.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("策略发布 {id} 不存在")))
}

/// 把校验错误转成 400，忽略 Warning。
fn ensure_valid(issues: &[arb_core::ValidationIssue]) -> Result<(), ApiError> {
    let errors: Vec<&arb_core::ValidationIssue> = issues
        .iter()
        .filter(|issue| issue.severity == ValidationSeverity::Error)
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    let message = errors
        .iter()
        .map(|issue| format!("{}: {}", issue.path, issue.message))
        .collect::<Vec<_>>()
        .join("；");
    Err(ApiError::BadRequest(message))
}

/// 发布的交付物：收窄后的参数空间、逻辑基线、以及对应的哈希。
struct SkillParts {
    schema: ParamsSchema,
    logic: crate::skill::SkillSettings,
    sha256: String,
}

/// Skill 版只交付逻辑骨架与参数空间；买家取值在导入时再应用。
fn skill_bundle_parts(
    version: &SkillVersion,
    constraints: Option<&ParamsSchema>,
) -> Result<SkillParts, ApiError> {
    let base = if version.params_schema.params.is_empty() {
        // 旧版本没有显式参数空间：按「全部锁死」推导，等价于原配置版。
        ParamsSchema::from_settings(&version.settings)
    } else {
        version.params_schema.clone()
    };
    let effective = match constraints {
        Some(constraints) if !constraints.params.is_empty() => {
            base.intersect(constraints).map_err(param_error)?
        }
        _ => base,
    };
    let logic = skill_library::normalize_settings(&version.settings, &effective);
    let sha256 = skill_library::hash_logic(&logic, &effective);
    Ok(SkillParts {
        schema: effective,
        logic,
        sha256,
    })
}

fn schema_from_value(value: Option<&serde_json::Value>) -> Result<Option<ParamsSchema>, ApiError> {
    value
        .map(|value| {
            serde_json::from_value::<ParamsSchema>(value.clone())
                .map_err(|error| ApiError::BadRequest(format!("参数空间无法解析: {error}")))
        })
        .transpose()
}

fn param_error(issues: Vec<ParamIssue>) -> ApiError {
    let message = issues
        .iter()
        .map(|issue| format!("{} · {}：{}", issue.rule_key, issue.path, issue.message))
        .collect::<Vec<_>>()
        .join("；");
    ApiError::BadRequest(message)
}
