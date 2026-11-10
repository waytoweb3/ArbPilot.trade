//! 策略分享与付费市场领域模型。
//!
//! 设计约束：
//! - 策略默认私有，只有作者主动发布后才出现在市场。
//! - 发布必须绑定一个已通过校验、且有同版本回测记录的策略版本。
//! - 表现记录按来源分开，作者自报回测与平台可复现记录不能混为一谈。
//! - 表现榜按订阅人数排序，不使用综合评分或收益率排名。
//! - 撤回发布只影响市场曝光，不影响已有仓位、订单查看与安全处置。
//! - 结算只记录金额与服务费，不保存任何收款密钥。

use std::collections::HashSet;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::strategy::{
    SkillBinding, StrategyDefinition, StrategyRecord, StrategyValidation, ValidationIssue,
    ValidationSeverity,
};

/// 策略风险等级，用于作者卡片与订阅前审阅。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// 源码可见性：开源便于复核，黑盒只公布行为说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceVisibility {
    OpenSource,
    BlackBox,
}

/// 市场可见范围。默认邀请制，作者可主动放开为公开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketVisibility {
    InviteOnly,
    Public,
}

/// 发布状态。撤回后仍保留记录，便于审计与重新发布。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStatus {
    /// 仅作者可见的私有草稿。
    Private,
    Published,
    /// 作者主动撤回或平台下架；不影响既有仓位。
    Withdrawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingModel {
    Free,
    Monthly,
}

/// 订阅与结算使用的定价信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyPricing {
    pub model: PricingModel,
    /// 月费（美元），免费策略必须为 0。
    pub monthly_price_usd: Decimal,
    /// 试用天数，0 表示不提供试用。
    #[serde(default)]
    pub trial_days: u32,
}

impl StrategyPricing {
    pub fn free() -> Self {
        Self {
            model: PricingModel::Free,
            monthly_price_usd: Decimal::ZERO,
            trial_days: 0,
        }
    }

    pub fn monthly(price: Decimal, trial_days: u32) -> Self {
        Self {
            model: PricingModel::Monthly,
            monthly_price_usd: price,
            trial_days,
        }
    }
}

/// 作者卡片：订阅前用户应先看到作者身份、历史与收款说明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyAuthorCard {
    pub author_id: Uuid,
    pub display_name: String,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub bio: String,
    /// 结算收款标识（如平台账户 ID），不得存放私钥或银行账号。
    #[serde(default)]
    pub payout_account: Option<String>,
    pub joined_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl StrategyAuthorCard {
    pub fn new(display_name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            author_id: Uuid::new_v4(),
            display_name: display_name.into(),
            headline: String::new(),
            bio: String::new(),
            payout_account: None,
            joined_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.display_name.trim().is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "display_name",
                "作者名称不能为空",
            ));
        } else if self.display_name.chars().count() > 60 {
            issues.push(issue(
                ValidationSeverity::Error,
                "display_name",
                "作者名称不能超过 60 个字符",
            ));
        }
        if self.bio.chars().count() > 2_000 {
            issues.push(issue(
                ValidationSeverity::Error,
                "bio",
                "作者简介不能超过 2000 个字符",
            ));
        }
        if self.headline.chars().count() > 120 {
            issues.push(issue(
                ValidationSeverity::Error,
                "headline",
                "作者标题不能超过 120 个字符",
            ));
        }
        issues
    }
}

/// 版本变更记录：订阅者需要知道当前版本改了什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyChangelogEntry {
    pub version: u32,
    pub at: DateTime<Utc>,
    pub summary: String,
}

/// 一条策略发布记录，绑定到具体策略版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyPublication {
    pub id: Uuid,
    pub strategy_id: Uuid,
    /// 发布时绑定的策略版本；后续策略升级需要作者主动重新发布。
    pub strategy_version: u32,
    pub author_id: Uuid,
    pub author_name: String,
    pub title: String,
    pub summary: String,
    /// 入场、退出、阻止交易的条件与未决假设。
    pub logic_summary: String,
    pub risk_level: RiskLevel,
    /// 作者建议的订阅者单笔名义价值上限，不能超过执行器风控。
    pub suggested_max_notional_usd: Decimal,
    pub supported_venues: Vec<String>,
    pub supported_symbols: Vec<String>,
    pub data_sources: Vec<String>,
    pub runtime: String,
    pub known_limits: Vec<String>,
    pub source_visibility: SourceVisibility,
    pub visibility: MarketVisibility,
    pub pricing: StrategyPricing,
    pub status: PublicationStatus,
    /// 发布时冻结的沙箱模块哈希；代码策略据此校验交付代码。
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    /// 发布时冻结的技能参数绑定；存在时订阅者无需沙箱模块即可运行。
    #[serde(default)]
    pub skill: Option<SkillBinding>,
    /// 技能库里的 Skill ID；独立版本化发布时写入。
    #[serde(default)]
    pub skill_id: Option<Uuid>,
    /// 发布绑定的 Skill 版本号。
    #[serde(default)]
    pub skill_version: Option<u32>,
    /// 冻结规则哈希，导入时校验交付物。
    #[serde(default)]
    pub skill_sha256: Option<String>,
    /// 发布时冻结的策略定义；订阅者导入时直接使用，不受作者后续修改影响。
    #[serde(default)]
    pub definition: Option<StrategyDefinition>,
    pub changelog: Vec<StrategyChangelogEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl StrategyPublication {
    /// 发布前的静态校验：说明、范围、定价与绑定版本。
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.title.trim().is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "title",
                "策略标题不能为空",
            ));
        } else if self.title.chars().count() > 80 {
            issues.push(issue(
                ValidationSeverity::Error,
                "title",
                "策略标题不能超过 80 个字符",
            ));
        }
        if self.summary.trim().is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "summary",
                "策略摘要不能为空",
            ));
        } else if self.summary.chars().count() > 400 {
            issues.push(issue(
                ValidationSeverity::Error,
                "summary",
                "策略摘要不能超过 400 个字符",
            ));
        }
        // 订阅者必须能读到完整的逻辑说明，否则无法审阅。
        if self.logic_summary.trim().chars().count() < 40 {
            issues.push(issue(
                ValidationSeverity::Error,
                "logic_summary",
                "策略逻辑说明至少 40 个字符，需覆盖入场、退出与阻止交易条件",
            ));
        }
        if self.supported_venues.is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "supported_venues",
                "至少需要声明一个支持的交易所",
            ));
        }
        if self.supported_symbols.is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "supported_symbols",
                "至少需要声明一个支持的交易对",
            ));
        }
        if self.data_sources.is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "data_sources",
                "需要声明数据来源，便于复现",
            ));
        }
        if self.runtime.trim().is_empty() {
            issues.push(issue(
                ValidationSeverity::Error,
                "runtime",
                "需要声明运行环境（数据定义或 WASM 沙箱）",
            ));
        }
        if self.suggested_max_notional_usd <= Decimal::ZERO {
            issues.push(issue(
                ValidationSeverity::Error,
                "suggested_max_notional_usd",
                "建议仓位上限必须大于 0",
            ));
        }
        if self.source_visibility == SourceVisibility::BlackBox {
            issues.push(issue(
                ValidationSeverity::Warning,
                "source_visibility",
                "黑盒策略需在说明中写清已知限制，订阅者无法复核实现",
            ));
        }
        if self.known_limits.is_empty() {
            issues.push(issue(
                ValidationSeverity::Warning,
                "known_limits",
                "建议补充已知限制（行情依赖、失败模式、适用范围）",
            ));
        }

        match self.pricing.model {
            PricingModel::Free => {
                if self.pricing.monthly_price_usd != Decimal::ZERO {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        "pricing.monthly_price_usd",
                        "免费策略的月费必须为 0",
                    ));
                }
            }
            PricingModel::Monthly => {
                if self.pricing.monthly_price_usd <= Decimal::ZERO {
                    issues.push(issue(
                        ValidationSeverity::Error,
                        "pricing.monthly_price_usd",
                        "付费策略的月费必须大于 0",
                    ));
                } else if self.pricing.monthly_price_usd > Decimal::from(500) {
                    issues.push(issue(
                        ValidationSeverity::Warning,
                        "pricing.monthly_price_usd",
                        "月费超过 500 美元，订阅转化可能较低",
                    ));
                }
            }
        }
        if self.pricing.trial_days > 30 {
            issues.push(issue(
                ValidationSeverity::Error,
                "pricing.trial_days",
                "试用天数不能超过 30 天",
            ));
        }

        if self.changelog.is_empty() {
            issues.push(issue(
                ValidationSeverity::Warning,
                "changelog",
                "缺少版本变更记录，订阅者无法判断版本差异",
            ));
        }

        issues
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }

    pub fn is_market_visible(&self) -> bool {
        self.status == PublicationStatus::Published
    }

    /// 用最新策略版本刷新变更记录，避免订阅者看到过期版本号。
    pub fn attach_version(&mut self, record: &StrategyRecord, summary: Option<String>) {
        if self.strategy_version == record.version {
            return;
        }
        self.strategy_version = record.version;
        self.changelog.push(StrategyChangelogEntry {
            version: record.version,
            at: Utc::now(),
            summary: summary.unwrap_or_else(|| format!("升级到版本 {}", record.version)),
        });
        self.updated_at = Utc::now();
    }
}

/// 发布前必须满足的可验证条件：校验通过、同版本回测存在且样本非空。
pub fn publish_readiness(
    record: &StrategyRecord,
    validation: &StrategyValidation,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    if validation.has_errors() {
        issues.push(issue(
            ValidationSeverity::Error,
            "strategy.validation",
            "策略校验未通过，不能发布",
        ));
    }
    // 订阅者要在自己机器上运行，发布必须带着可执行内容：沙箱模块或技能参数。
    if record.sandbox.is_none() && record.skill.is_none() {
        issues.push(issue(
            ValidationSeverity::Error,
            "strategy.runtime",
            "发布前必须先部署策略代码或配置技能参数",
        ));
    }
    if record.status == crate::strategy::StrategyStatus::Running {
        issues.push(issue(
            ValidationSeverity::Warning,
            "strategy.status",
            "策略正在运行，发布会绑定当前版本；后续修改需要重新发布",
        ));
    }

    // 回测不再是发布门槛：事件驱动策略没有历史公告可回放，只要代码可执行即可发布。
    match &record.last_backtest {
        None => issues.push(issue(
            ValidationSeverity::Warning,
            "strategy.backtest",
            "尚未回测，订阅者只能参考作者自报表现",
        )),
        Some(report) => {
            if report.definition_version != record.version {
                issues.push(issue(
                    ValidationSeverity::Warning,
                    "strategy.backtest",
                    "回测结果与当前策略版本不一致，建议重新回测",
                ));
            }
            if report.sample_start.is_none() || report.sample_end.is_none() {
                issues.push(issue(
                    ValidationSeverity::Warning,
                    "strategy.backtest",
                    "回测样本为空，无法证明策略可复现",
                ));
            }
            if report.trades == 0 {
                issues.push(issue(
                    ValidationSeverity::Warning,
                    "strategy.backtest",
                    "回测没有产生交易；事件驱动策略可忽略此项",
                ));
            }
        }
    }

    issues
}

/// 表现记录来源。作者自报与平台可复现记录必须分开统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceSource {
    /// 作者在自己机器上跑的回测，平台未复现。
    AuthorBacktest,
    /// 平台用同一份定义与成本假设重放历史数据。
    PlatformReplay,
    /// 用户显式授权的实盘结果，去标识化后聚合。
    AuthorizedLive,
}

impl PerformanceSource {
    /// 只有平台侧产生的记录才计入可复现性评分。
    pub fn is_platform_verified(self) -> bool {
        matches!(self, Self::PlatformReplay | Self::AuthorizedLive)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AuthorBacktest => "author_backtest",
            Self::PlatformReplay => "platform_replay",
            Self::AuthorizedLive => "authorized_live",
        }
    }
}

/// 一条版本化表现记录，必须绑定策略版本、区间与成本假设。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyPerformanceRecord {
    pub id: Uuid,
    pub publication_id: Uuid,
    pub strategy_id: Uuid,
    pub strategy_version: u32,
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
    /// 参与统计的样本条数（结算期数或订单数）。
    pub sample_size: u32,
    /// 是否可由第三方复现。作者自报记录一律为 false。
    pub reproducible: bool,
    pub recorded_at: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

impl StrategyPerformanceRecord {
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.period_start >= self.period_end {
            issues.push(issue(
                ValidationSeverity::Error,
                "period",
                "统计区间开始时间必须早于结束时间",
            ));
        }
        if self.sample_size == 0 {
            issues.push(issue(
                ValidationSeverity::Error,
                "sample_size",
                "样本条数必须大于 0",
            ));
        }
        if self.win_rate_pct < Decimal::ZERO || self.win_rate_pct > Decimal::from(100) {
            issues.push(issue(
                ValidationSeverity::Error,
                "win_rate_pct",
                "胜率必须在 0 到 100 之间",
            ));
        }
        if self.max_drawdown_pct < Decimal::ZERO {
            issues.push(issue(
                ValidationSeverity::Error,
                "max_drawdown_pct",
                "最大回撤不能为负",
            ));
        }
        if self.fee_bps_per_leg < Decimal::ZERO || self.slippage_bps < Decimal::ZERO {
            issues.push(issue(
                ValidationSeverity::Error,
                "costs",
                "成本假设不能为负",
            ));
        }
        if self.reproducible && self.source == PerformanceSource::AuthorBacktest {
            issues.push(issue(
                ValidationSeverity::Error,
                "reproducible",
                "作者自报回测不能被标记为可复现",
            ));
        }
        if self.source.is_platform_verified() && !self.reproducible {
            issues.push(issue(
                ValidationSeverity::Warning,
                "reproducible",
                "平台记录建议标记为可复现，否则不会计入评分",
            ));
        }
        issues
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// 已创建但尚未完成支付。
    PendingPayment,
    Trial,
    Active,
    /// 用户取消，当前计费周期结束后失效。
    Cancelled,
    Expired,
}

impl SubscriptionStatus {
    pub fn is_entitled(self) -> bool {
        matches!(self, Self::Trial | Self::Active)
    }
}

/// 用户对某个策略发布的订阅。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySubscription {
    pub id: Uuid,
    pub publication_id: Uuid,
    pub publication_title: String,
    pub strategy_id: Uuid,
    pub author_id: Uuid,
    pub subscriber_id: Uuid,
    pub status: SubscriptionStatus,
    pub monthly_price_usd: Decimal,
    /// 试用结束时间；无试用时为 None。
    pub trial_ends_at: Option<DateTime<Utc>>,
    /// 当前计费周期结束时间；取消后仍可用到该时间。
    pub current_period_end: Option<DateTime<Utc>>,
    pub started_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 支付服务返回的会话/订单引用，不含任何支付凭证。
    #[serde(default)]
    pub payment_ref: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl StrategySubscription {
    pub fn new(
        publication: &StrategyPublication,
        subscriber_id: Uuid,
        payment_ref: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        let trial_ends_at = (publication.pricing.trial_days > 0)
            .then(|| now + chrono::Duration::days(publication.pricing.trial_days as i64));
        let (status, current_period_end) = if publication.pricing.model == PricingModel::Free {
            (SubscriptionStatus::Active, None)
        } else if trial_ends_at.is_some() {
            (SubscriptionStatus::Trial, trial_ends_at)
        } else {
            (
                SubscriptionStatus::PendingPayment,
                Some(now + chrono::Duration::days(30)),
            )
        };

        Self {
            id: Uuid::new_v4(),
            publication_id: publication.id,
            publication_title: publication.title.clone(),
            strategy_id: publication.strategy_id,
            author_id: publication.author_id,
            subscriber_id,
            status,
            monthly_price_usd: publication.pricing.monthly_price_usd,
            trial_ends_at,
            current_period_end,
            started_at: now,
            cancelled_at: None,
            payment_ref,
            updated_at: now,
        }
    }

    /// 是否仍可使用该策略：试用未过期，或付费周期未结束。
    pub fn is_entitled_at(&self, now: DateTime<Utc>) -> bool {
        match self.status {
            SubscriptionStatus::Trial => self.trial_ends_at.is_some_and(|end| now < end),
            SubscriptionStatus::Active => self.current_period_end.is_none_or(|end| now < end),
            _ => false,
        }
    }

    /// 取消订阅：保留到当前周期结束，不立即切断已有仓位管理。
    pub fn cancel(&mut self, now: DateTime<Utc>) {
        if matches!(
            self.status,
            SubscriptionStatus::Active | SubscriptionStatus::Trial
        ) {
            self.status = SubscriptionStatus::Cancelled;
            self.cancelled_at = Some(now);
            if self.current_period_end.is_none() {
                self.current_period_end = Some(now);
            }
            self.updated_at = now;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStatus {
    Pending,
    Paid,
    /// 存在退款、拒付或争议，暂缓结算。
    Disputed,
}

/// 作者月度结算单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySettlement {
    pub id: Uuid,
    pub author_id: Uuid,
    /// 结算周期，格式 `YYYY-MM`。
    pub period: String,
    pub subscription_count: u32,
    pub gross_usd: Decimal,
    pub platform_fee_bps: u32,
    pub platform_fee_usd: Decimal,
    pub net_usd: Decimal,
    pub status: SettlementStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

/// 当前时间所在结算周期（UTC）。
pub fn settlement_period(now: DateTime<Utc>) -> String {
    format!("{:04}-{:02}", now.year(), now.month())
}

/// 解析 `YYYY-MM` 为周期起止时间。
pub fn period_bounds(period: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (year, month) = period.split_once('-')?;
    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let start = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single()?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()?;
    Some((start, end))
}

/// 按订阅数与月费计算一张结算单。服务费为 bps，向下取整到分。
pub fn compute_settlement(
    author_id: Uuid,
    period: &str,
    monthly_price_usd: Decimal,
    subscription_count: u32,
    platform_fee_bps: u32,
) -> StrategySettlement {
    compute_settlement_from_gross(
        author_id,
        period,
        monthly_price_usd * Decimal::from(subscription_count),
        subscription_count,
        platform_fee_bps,
    )
}

/// 直接按毛收入（多条不同价位订阅汇总）计算结算单。
pub fn compute_settlement_from_gross(
    author_id: Uuid,
    period: &str,
    gross_usd: Decimal,
    subscription_count: u32,
    platform_fee_bps: u32,
) -> StrategySettlement {
    let platform_fee = (gross_usd * Decimal::from(platform_fee_bps) / Decimal::from(10_000))
        .round_dp_with_strategy(2, RoundingStrategy::ToZero);
    let now = Utc::now();
    StrategySettlement {
        id: Uuid::new_v4(),
        author_id,
        period: period.to_string(),
        subscription_count,
        gross_usd: gross_usd.round_dp(2),
        platform_fee_bps,
        platform_fee_usd: platform_fee,
        net_usd: (gross_usd - platform_fee).round_dp(2),
        status: SettlementStatus::Pending,
        created_at: now,
        updated_at: now,
        note: None,
    }
}

/// 表现榜条目。按订阅人数排序。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    pub publication_id: Uuid,
    pub title: String,
    pub author_name: String,
    pub strategy_version: u32,
    pub risk_level: RiskLevel,
    /// 当前仍在试用或付费中的独立订阅人数。
    pub subscriber_count: u32,
    pub sources: Vec<PerformanceSource>,
}

/// 生成表现榜。只统计已发布策略，并按订阅人数从高到低排序。
pub fn rank_publications(
    publications: &[StrategyPublication],
    records: &[StrategyPerformanceRecord],
    subscriptions: &[StrategySubscription],
) -> Vec<LeaderboardEntry> {
    let mut entries: Vec<LeaderboardEntry> = publications
        .iter()
        .filter(|publication| publication.is_market_visible())
        .map(|publication| {
            let own: Vec<&StrategyPerformanceRecord> = records
                .iter()
                .filter(|record| record.publication_id == publication.id)
                .collect();

            let mut sources: Vec<PerformanceSource> =
                own.iter().map(|record| record.source).collect();
            sources.sort_by_key(|source| source.label());
            sources.dedup();

            let subscriber_count = subscriptions
                .iter()
                .filter(|subscription| {
                    subscription.publication_id == publication.id
                        && subscription.status.is_entitled()
                })
                .map(|subscription| subscription.subscriber_id)
                .collect::<HashSet<Uuid>>()
                .len() as u32;

            LeaderboardEntry {
                publication_id: publication.id,
                title: publication.title.clone(),
                author_name: publication.author_name.clone(),
                strategy_version: publication.strategy_version,
                risk_level: publication.risk_level,
                subscriber_count,
                sources,
            }
        })
        .collect();

    entries.sort_by(|left, right| {
        right
            .subscriber_count
            .cmp(&left.subscriber_count)
            .then_with(|| left.title.cmp(&right.title))
    });
    entries
}

fn issue(severity: ValidationSeverity, path: &str, message: &str) -> ValidationIssue {
    ValidationIssue {
        path: path.to_string(),
        severity,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn publication() -> StrategyPublication {
        let now = Utc::now();
        StrategyPublication {
            id: Uuid::new_v4(),
            strategy_id: Uuid::new_v4(),
            strategy_version: 3,
            author_id: Uuid::new_v4(),
            author_name: "量化小站".into(),
            title: "资金费率基线".into(),
            summary: "现货多 + 永续空，收取正资金费率。".into(),
            logic_summary:
                "当资金费率高于阈值时开仓现货多与永续空；费率跌破退出阈值或超过最长持仓时间时平仓。"
                    .into(),
            risk_level: RiskLevel::Low,
            suggested_max_notional_usd: dec!(1000),
            supported_venues: vec!["binance-spot".into(), "binance".into()],
            supported_symbols: vec!["BTCUSDT".into()],
            data_sources: vec!["已结算资金费率".into()],
            runtime: "受限数据定义".into(),
            known_limits: vec!["仅支持正资金费率".into()],
            source_visibility: SourceVisibility::OpenSource,
            visibility: MarketVisibility::Public,
            pricing: StrategyPricing::monthly(dec!(19), 7),
            status: PublicationStatus::Published,
            artifact_sha256: Some("a".repeat(64)),
            skill: None,
            skill_id: None,
            skill_version: None,
            skill_sha256: None,
            definition: None,
            changelog: vec![StrategyChangelogEntry {
                version: 3,
                at: now,
                summary: "调整退出阈值".into(),
            }],
            created_at: now,
            updated_at: now,
        }
    }

    fn record(
        publication_id: Uuid,
        source: PerformanceSource,
        reproducible: bool,
        trades: u32,
        return_pct: Decimal,
        drawdown: Decimal,
    ) -> StrategyPerformanceRecord {
        let now = Utc::now();
        StrategyPerformanceRecord {
            id: Uuid::new_v4(),
            publication_id,
            strategy_id: Uuid::new_v4(),
            strategy_version: 3,
            source,
            period_start: now - chrono::Duration::days(30),
            period_end: now,
            trades,
            win_rate_pct: dec!(60),
            net_pnl_usd: dec!(120),
            return_pct,
            max_drawdown_pct: drawdown,
            fee_bps_per_leg: dec!(4),
            slippage_bps: dec!(1),
            sample_size: 90,
            reproducible,
            recorded_at: now,
            note: None,
        }
    }

    #[test]
    fn publication_validation_requires_logic_and_pricing() {
        let mut publication = publication();
        assert!(!publication.has_errors());

        publication.logic_summary = "太短".into();
        publication.pricing.monthly_price_usd = dec!(0);
        let issues = publication.validate();
        assert!(issues
            .iter()
            .any(|issue| issue.path == "logic_summary"
                && issue.severity == ValidationSeverity::Error));
        assert!(issues
            .iter()
            .any(|issue| issue.path == "pricing.monthly_price_usd"));
    }

    #[test]
    fn author_backtest_cannot_be_reproducible() {
        let mut record = record(
            Uuid::new_v4(),
            PerformanceSource::AuthorBacktest,
            true,
            10,
            dec!(1),
            dec!(1),
        );
        assert!(record.has_errors());
        record.reproducible = false;
        assert!(!record.has_errors());
    }

    #[test]
    fn leaderboard_ranks_by_subscribers() {
        let published = publication();
        let mut other = publication();
        other.id = Uuid::new_v4();
        other.title = "无人订阅".into();

        let now = Utc::now();
        let subscriptions = vec![
            StrategySubscription::new(&published, Uuid::new_v4(), None, now),
            StrategySubscription::new(&published, Uuid::new_v4(), None, now),
        ];
        let board = rank_publications(&[published, other], &[], &subscriptions);
        assert_eq!(board.len(), 2);
        assert_eq!(board[0].title, "资金费率基线");
        assert_eq!(board[0].subscriber_count, 2);
        assert_eq!(board[1].subscriber_count, 0);
    }

    #[test]
    fn settlement_deducts_platform_fee() {
        let settlement = compute_settlement(Uuid::new_v4(), "2026-11", dec!(20), 10, 2_000);
        assert_eq!(settlement.gross_usd, dec!(200));
        assert_eq!(settlement.platform_fee_usd, dec!(40));
        assert_eq!(settlement.net_usd, dec!(160));
    }

    #[test]
    fn period_bounds_rejects_invalid_month() {
        assert!(period_bounds("2026-13").is_none());
        let (start, end) = period_bounds("2026-12").expect("有效周期");
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn subscription_trial_expires_and_cancel_keeps_period() {
        let publication = publication();
        let now = Utc::now();
        let mut subscription = StrategySubscription::new(&publication, Uuid::new_v4(), None, now);
        assert_eq!(subscription.status, SubscriptionStatus::Trial);
        assert!(subscription.is_entitled_at(now));
        assert!(!subscription.is_entitled_at(now + chrono::Duration::days(8)));

        subscription.status = SubscriptionStatus::Active;
        subscription.current_period_end = Some(now + chrono::Duration::days(30));
        subscription.cancel(now);
        assert_eq!(subscription.status, SubscriptionStatus::Cancelled);
        assert_eq!(
            subscription.current_period_end,
            Some(now + chrono::Duration::days(30))
        );
    }
}
