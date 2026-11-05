//! 策略分享与付费市场 REST API。
//!
//! - 策略默认私有，只有作者主动发布后才出现在榜单。
//! - 发布前必须通过校验，并有与当前版本一致的回测记录。
//! - 表现记录按来源区分，作者自报回测不计入可复现性评分。
//! - 订阅与结算只保存业务金额与引用，不接触任何支付凭证或私钥。

use arb_core::{
    compute_settlement_from_gross, period_bounds, publish_readiness, rank_publications,
    settlement_period, LeaderboardEntry, MarketVisibility, PerformanceSource, PublicationStatus,
    RiskLevel, SourceVisibility, StrategyAuthorCard, StrategyChangelogEntry,
    StrategyPerformanceRecord, StrategyPricing, StrategyPublication, StrategySettlement,
    StrategySubscription, SubscriptionStatus, ValidationSeverity,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

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

    let now = Utc::now();
    let runtime = body.runtime.unwrap_or_else(|| {
        record
            .sandbox
            .as_ref()
            .map(|binding| format!("WASM 沙箱 · {}", &binding.artifact_hash[..12]))
            .unwrap_or_else(|| "受限数据定义".to_string())
    });
    let data_sources = body.data_sources.unwrap_or_else(|| {
        vec!["已结算资金费率".to_string(), "实时盘口".to_string()]
    });
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
            // 重新读取策略当前版本，确保发布的是通过校验且有回测的版本。
            let mut record = find_strategy(&state, publication.strategy_id)?;
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
            publication.attach_version(&record, body.changelog_summary);
            publication.status = PublicationStatus::Published;
        }
        Some(PublicationAction::Withdraw) => {
            // 只撤回市场曝光；已有仓位、订单与安全处置能力保持不变。
            publication.status = PublicationStatus::Withdrawn;
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
        _ => body.reproducible.unwrap_or_else(|| body.source.is_platform_verified()),
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
        return Err(ApiError::Conflict(
            "该策略当前未发布，无法订阅".to_string(),
        ));
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

    let subscription =
        StrategySubscription::new(&publication, body.subscriber_id, body.payment_ref, Utc::now());
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
    if period_bounds(&period).is_none() {
        return Err(ApiError::BadRequest(
            "结算周期格式必须为 YYYY-MM".to_string(),
        ));
    }

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
        .filter(|subscription| subscription.monthly_price_usd > Decimal::ZERO)
        .collect();

    if billable.is_empty() {
        return Err(ApiError::Conflict("该周期没有付费订阅可结算".to_string()));
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

async fn find_publication(
    state: &AppState,
    id: Uuid,
) -> Result<StrategyPublication, ApiError> {
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
