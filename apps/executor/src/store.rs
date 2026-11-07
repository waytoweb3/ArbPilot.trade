use std::{collections::HashMap, sync::RwLock};

use arb_core::{
    BacktestReport, Order, SandboxArtifact, StrategyAuthorCard, StrategyCode, StrategyDefinition,
    StrategyEventLevel, StrategyPerformanceRecord, StrategyPublication, StrategyRecord,
    StrategyRunScope, StrategySettlement, StrategySource, StrategyStatus, StrategySubscription,
    StrategyValidation,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use uuid::Uuid;

use crate::{exchange::FundingRate, sqlite_store::SqliteStore, templates::StrategyTemplate};

/// 有本地 SQLite 时优先走本地库，其余分支保持 PostgreSQL / 内存逻辑不变。
macro_rules! sqlite_branch {
    ($self:expr, $store:ident => $call:expr) => {
        if let Some($store) = &$self.sqlite {
            return $call;
        }
    };
}

/// 某个时间窗口内某交易所某交易对的资金费率累计值。
#[derive(Debug, Clone, Serialize)]
pub struct FundingSummary {
    pub venue: String,
    pub symbol: String,
    /// 窗口内各期费率之和。
    pub rate: Decimal,
    /// 窗口内的结算期数。
    pub count: i64,
}

/// 策略可选的交易所：`segment` 区分现货腿与永续腿。
#[derive(Debug, Clone, Serialize)]
pub struct ExchangeVenueConfig {
    pub venue: String,
    pub segment: String,
    pub display_name: String,
}

/// 某个交易所配置的可交易对。
#[derive(Debug, Clone, Serialize)]
pub struct TradableSymbolConfig {
    pub venue: String,
    pub symbol: String,
}

/// 消息通知配置：新闻推送与资金费率套利提醒的开关、阈值与渠道。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct NotificationSettings {
    pub news_enabled: bool,
    /// 参与推送的新闻来源；空列表表示全部来源。
    pub news_sources: Vec<String>,
    pub funding_enabled: bool,
    /// 触发提醒的最小年化资金费率（百分比数值，例如 10 表示 10%）。
    pub funding_min_annualized_pct: Decimal,
    /// 同一交易对再次提醒的最小间隔（秒）。
    pub funding_cooldown_secs: i64,
    pub telegram_enabled: bool,
    pub desktop_enabled: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            news_enabled: true,
            news_sources: crate::news::SOURCES.iter().map(|item| item.to_string()).collect(),
            funding_enabled: true,
            funding_min_annualized_pct: Decimal::from(10),
            funding_cooldown_secs: 6 * 3600,
            telegram_enabled: true,
            desktop_enabled: true,
        }
    }
}

/// 某个交易账户在某一时刻的美元权益快照。
#[derive(Debug, Clone, Serialize)]
pub struct EquityPoint {
    pub account_id: Uuid,
    pub venue: String,
    pub equity_usd: Decimal,
    pub observed_at: DateTime<Utc>,
}

/// 一次策略版本快照：完整定义、校验、回测与部署状态，便于复现与回滚。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StrategyVersion {
    pub strategy_id: Uuid,
    pub version: u32,
    pub status: StrategyStatus,
    pub source: StrategySource,
    pub definition: StrategyDefinition,
    pub validation: StrategyValidation,
    pub backtest: Option<BacktestReport>,
    pub saved_at: DateTime<Utc>,
}

impl StrategyVersion {
    pub(crate) fn from_record(record: &StrategyRecord) -> Self {
        Self {
            strategy_id: record.id,
            version: record.version,
            status: record.status,
            source: record.source,
            definition: record.definition.clone(),
            validation: record.validation.clone(),
            backtest: record.last_backtest.clone(),
            saved_at: Utc::now(),
        }
    }
}

/// 一次策略运行实例：策略 × 交易所组合 × 运行位置，用于查看「当前策略跑在哪、跑了多久」。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StrategyRun {
    pub id: Uuid,
    pub strategy_id: Uuid,
    pub strategy_version: u32,
    pub scope: StrategyRunScope,
    pub status: StrategyStatus,
    pub long_venue: String,
    pub short_venue: String,
    pub symbols: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

impl StrategyRun {
    pub fn new(record: &StrategyRecord) -> Self {
        let now = Utc::now();
        let venues = &record.definition.universe.venues;
        Self {
            id: Uuid::new_v4(),
            strategy_id: record.id,
            strategy_version: record.version,
            scope: record.run_scope,
            status: StrategyStatus::Running,
            long_venue: venues.first().cloned().unwrap_or_default(),
            short_venue: venues.get(1).cloned().unwrap_or_default(),
            symbols: record.definition.universe.symbols.clone(),
            started_at: now,
            stopped_at: None,
            last_heartbeat_at: Some(now),
        }
    }
}

/// 运行事件：策略侧产生的评估、信号、订单与错误，不含任何凭证。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StrategyRunEvent {
    pub run_id: Uuid,
    pub strategy_id: Uuid,
    pub level: StrategyEventLevel,
    pub message: String,
    pub order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// 内存模式下最多保留的资金费率条数，避免长时间运行无限增长。
const MAX_MEMORY_FUNDING_ROWS: usize = 50_000;
/// 内存模式下最多保留的权益快照条数。
const MAX_MEMORY_EQUITY_ROWS: usize = 20_000;
/// 内存模式下最多保留的运行事件条数。
const MAX_MEMORY_RUN_EVENTS: usize = 2_000;

/// 策略市场的一次完整读取结果，供 API 计算榜单与结算。
#[derive(Debug, Clone, Default)]
pub struct MarketSnapshot {
    pub authors: Vec<StrategyAuthorCard>,
    pub publications: Vec<StrategyPublication>,
    pub performance: Vec<StrategyPerformanceRecord>,
    pub subscriptions: Vec<StrategySubscription>,
    pub settlements: Vec<StrategySettlement>,
}

/// 业务状态存储。
///
/// - 未配置 DATABASE_URL 或连接失败时降级为内存模式，执行器仍可运行（便于本地开发）。
/// - 只会写入订单、风控告警、资金费率等业务数据，不保存任何交易所密钥或签名材料。
pub struct Store {
    pool: Option<PgPool>,
    /// 桌面端随包启动时使用的本地 SQLite；配置后所有业务数据都落在这里。
    sqlite: Option<SqliteStore>,
    /// 内存降级时的资金费率缓存，重启即丢。
    funding: RwLock<Vec<FundingRate>>,
    /// 内存降级时的账户权益快照，重启即丢。
    equity: RwLock<Vec<EquityPoint>>,
    /// 内存降级时的策略快照，重启即丢。
    strategies: RwLock<Vec<StrategyRecord>>,
    /// 内存降级时的策略版本历史，重启即丢。
    strategy_versions: RwLock<Vec<StrategyVersion>>,
    /// 内存降级时的策略运行实例，重启即丢。
    strategy_runs: RwLock<Vec<StrategyRun>>,
    /// 内存降级时的运行事件，重启即丢。
    strategy_run_events: RwLock<Vec<StrategyRunEvent>>,
    /// 内存降级时的策略代码，重启即丢。
    strategy_codes: RwLock<HashMap<Uuid, StrategyCode>>,
    /// 内存降级时的市场数据，重启即丢。
    authors: RwLock<Vec<StrategyAuthorCard>>,
    publications: RwLock<Vec<StrategyPublication>>,
    performance: RwLock<Vec<StrategyPerformanceRecord>>,
    subscriptions: RwLock<Vec<StrategySubscription>>,
    settlements: RwLock<Vec<StrategySettlement>>,
    /// 内存降级时的沙箱模块元数据。
    sandbox_artifacts: RwLock<Vec<SandboxArtifact>>,
    /// 沙箱模块字节；仅保存在执行器本机。
    sandbox_modules: RwLock<HashMap<Uuid, Vec<u8>>>,
    /// 内置策略模板（源码 + 默认定义），重启后从数据库恢复。
    strategy_templates: RwLock<Vec<StrategyTemplate>>,
}

impl Store {
    /// 连接业务库：优先 `DATABASE_URL`，其次桌面端注入的本地 SQLite 文件。
    ///
    /// 两者都没有时降级为内存模式，便于在浏览器里调试界面。
    pub async fn connect_if_configured(
        database_url: Option<&str>,
        local_db: Option<&std::path::Path>,
    ) -> Self {
        if let Some(url) = database_url {
            if url.starts_with("sqlite:") {
                let path = url
                    .strip_prefix("sqlite://")
                    .or_else(|| url.strip_prefix("sqlite:"))
                    .unwrap_or(url);
                return Self::connect_sqlite(std::path::Path::new(path)).await;
            }
        }

        if let Some(url) = database_url {
            let pool = match PgPoolOptions::new().max_connections(8).connect(url).await {
                Ok(pool) => pool,
                Err(error) => {
                    tracing::warn!(%error, "连接 PostgreSQL 失败，执行器以内存模式运行");
                    return Self::memory();
                }
            };

            match sqlx::migrate!("./migrations").run(&pool).await {
                Ok(_) => {
                    tracing::info!("PostgreSQL 已连接并完成迁移");
                    return Self::with_pool(Some(pool));
                }
                Err(error) => {
                    tracing::error!(%error, "数据库迁移失败，执行器以内存模式运行");
                    return Self::memory();
                }
            }
        }

        if let Some(path) = local_db {
            return Self::connect_sqlite(path).await;
        }

        tracing::info!("未配置数据库，执行器以内存模式运行");
        Self::memory()
    }

    async fn connect_sqlite(path: &std::path::Path) -> Self {
        match SqliteStore::connect(path).await {
            Ok(store) => {
                tracing::info!(path = %path.display(), "本地 SQLite 已就绪");
                Self::with_sqlite(store)
            }
            Err(error) => {
                tracing::error!(%error, path = %path.display(), "打开本地 SQLite 失败，执行器以内存模式运行");
                Self::memory()
            }
        }
    }

    fn memory() -> Self {
        Self::with_pool(None)
    }

    fn with_pool(pool: Option<PgPool>) -> Self {
        Self {
            pool,
            sqlite: None,
            funding: RwLock::new(Vec::new()),
            equity: RwLock::new(Vec::new()),
            strategies: RwLock::new(Vec::new()),
            strategy_versions: RwLock::new(Vec::new()),
            strategy_runs: RwLock::new(Vec::new()),
            strategy_run_events: RwLock::new(Vec::new()),
            strategy_codes: RwLock::new(HashMap::new()),
            authors: RwLock::new(Vec::new()),
            publications: RwLock::new(Vec::new()),
            performance: RwLock::new(Vec::new()),
            subscriptions: RwLock::new(Vec::new()),
            settlements: RwLock::new(Vec::new()),
            sandbox_artifacts: RwLock::new(Vec::new()),
            sandbox_modules: RwLock::new(HashMap::new()),
            strategy_templates: RwLock::new(Vec::new()),
        }
    }

    fn with_sqlite(sqlite: SqliteStore) -> Self {
        let mut store = Self::with_pool(None);
        store.sqlite = Some(sqlite);
        store
    }

    pub fn is_connected(&self) -> bool {
        self.pool.is_some() || self.sqlite.is_some()
    }

    pub async fn save_order(&self, order: &Order) {
        sqlite_branch!(self, db => db.save_order(order).await);
        let Some(pool) = &self.pool else {
            return;
        };

        let payload = match serde_json::to_value(order) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "订单序列化失败，未写入数据库");
                return;
            }
        };
        let symbol = order
            .intent
            .symbols()
            .first()
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());

        let result = sqlx::query(
            r#"
            insert into orders (id, status, origin, symbol, payload)
            values ($1, $2, $3, $4, $5)
            on conflict (id) do update
                set status = excluded.status,
                    payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(order.id)
        .bind(format!("{:?}", order.status).to_lowercase())
        .bind(format!("{:?}", order.intent.origin).to_lowercase())
        .bind(symbol)
        .bind(payload)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, order_id = %order.id, "写入订单失败");
        }
    }

    /// 追加一条订单生命周期事件（下单意图、风控、逐腿提交、成交、失败、补偿等）。
    ///
    /// 未连接数据库时只写本地日志；事件依赖 `orders` 中存在对应主键，
    /// 调用方必须先 `save_order`。
    pub async fn append_order_event(
        &self,
        order_id: Uuid,
        event_type: &str,
        detail: serde_json::Value,
    ) {
        sqlite_branch!(self, db => db.append_order_event(order_id, event_type, detail.clone()).await);
        let Some(pool) = &self.pool else {
            tracing::info!(%order_id, event_type, detail = %detail, "订单事件（内存模式，未落库）");
            return;
        };

        let result = sqlx::query(
            r#"
            insert into order_events (order_id, event_type, detail)
            values ($1, $2, $3)
            "#,
        )
        .bind(order_id)
        .bind(event_type)
        .bind(detail)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, %order_id, event_type, "写入订单事件失败");
        }
    }

    /// 写入审计日志，用于记录尚未生成订单就被拒绝的请求。
    pub async fn append_audit_log(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: serde_json::Value,
    ) {
        sqlite_branch!(self, db => db.append_audit_log(actor, action, target, detail.clone()).await);
        let Some(pool) = &self.pool else {
            tracing::info!(
                actor,
                action,
                target = target.unwrap_or("-"),
                detail = %detail,
                "审计日志（内存模式，未落库）"
            );
            return;
        };

        let result = sqlx::query(
            r#"
            insert into audit_log (actor, action, target, detail)
            values ($1, $2, $3, $4)
            "#,
        )
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, actor, action, "写入审计日志失败");
        }
    }

    /// 写入资金费率，按 (venue, symbol, funding_time) 幂等。
    ///
    /// 返回实际写入或更新的条数；未连接数据库时写入内存缓存（重启即丢）。
    pub async fn upsert_funding_rates(&self, rates: &[FundingRate]) -> anyhow::Result<usize> {
        if rates.is_empty() {
            return Ok(0);
        }

        sqlite_branch!(self, db => db.upsert_funding_rates(rates).await);
        let Some(pool) = &self.pool else {
            return Ok(self.upsert_funding_memory(rates));
        };

        let mut written = 0;
        let mut failures = 0;
        for rate in rates {
            let result = sqlx::query(
                r#"
                insert into funding_rates
                    (venue, symbol, funding_time, funding_rate, mark_price)
                values ($1, $2, $3, $4::numeric, $5::numeric)
                on conflict (venue, symbol, funding_time) do update
                    set funding_rate = excluded.funding_rate,
                        mark_price = excluded.mark_price,
                        ingested_at = now()
                "#,
            )
            .bind(&rate.venue)
            .bind(&rate.symbol)
            .bind(rate.funding_time)
            .bind(rate.funding_rate.to_string())
            .bind(rate.mark_price.map(|price| price.to_string()))
            .execute(pool)
            .await;

            match result {
                Ok(_) => written += 1,
                Err(error) => {
                    failures += 1;
                    tracing::error!(%error, symbol = %rate.symbol, "写入资金费率失败");
                }
            }
        }

        if failures > 0 {
            anyhow::bail!("{failures} 条资金费率写入失败");
        }
        Ok(written)
    }

    /// 按交易所查询资金费率历史，按结算时间倒序返回。
    pub async fn funding_rates(
        &self,
        venue: &str,
        symbol: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<FundingRate>> {
        sqlite_branch!(self, db => db.funding_rates(venue, symbol, limit).await);
        let Some(pool) = &self.pool else {
            return Ok(self.funding_rates_memory(venue, symbol, limit));
        };

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            r#"
            select symbol, funding_time, funding_rate::text as funding_rate,
                   mark_price::text as mark_price
            from funding_rates
            where venue = $1 and ($2::text is null or symbol = $2)
            order by funding_time desc
            limit $3
            "#,
        )
        .bind(venue)
        .bind(symbol)
        .bind(limit)
        .fetch_all(pool)
        .await?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            rates.push(decode_funding_row(&row, venue)?);
        }
        Ok(rates)
    }

    /// 取每个交易对最新的一条资金费率。
    pub async fn funding_latest(
        &self,
        venue: &str,
        symbols: &[String],
    ) -> anyhow::Result<Vec<FundingRate>> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }

        sqlite_branch!(self, db => db.funding_latest(venue, symbols).await);
        let Some(pool) = &self.pool else {
            let rows = self.funding.read().expect("资金费率锁不可用");
            return Ok(symbols
                .iter()
                .filter_map(|symbol| {
                    rows.iter()
                        .filter(|rate| rate.venue == venue && &rate.symbol == symbol)
                        .max_by_key(|rate| rate.funding_time)
                        .cloned()
                })
                .collect());
        };

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            r#"
            select distinct on (symbol) symbol, funding_time,
                   funding_rate::text as funding_rate, mark_price::text as mark_price
            from funding_rates
            where venue = $1 and symbol = any($2)
            order by symbol, funding_time desc
            "#,
        )
        .bind(venue)
        .bind(symbols)
        .fetch_all(pool)
        .await?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            rates.push(decode_funding_row(&row, venue)?);
        }
        Ok(rates)
    }

    /// 时间窗口内的资金费率累计，按 (venue, symbol) 汇总。
    ///
    /// 币种很多时前端无法逐币拉历史，用一条聚合查询替代。
    pub async fn funding_summary(
        &self,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<FundingSummary>> {
        sqlite_branch!(self, db => db.funding_summary(since).await);
        let Some(pool) = &self.pool else {
            let rows = self.funding.read().expect("资金费率锁不可用");
            let mut grouped: std::collections::HashMap<(String, String), (Decimal, i64)> =
                std::collections::HashMap::new();
            for rate in rows.iter().filter(|rate| rate.funding_time >= since) {
                let entry = grouped
                    .entry((rate.venue.clone(), rate.symbol.clone()))
                    .or_default();
                entry.0 += rate.funding_rate;
                entry.1 += 1;
            }
            return Ok(grouped
                .into_iter()
                .map(|((venue, symbol), (rate, count))| FundingSummary {
                    venue,
                    symbol,
                    rate,
                    count,
                })
                .collect());
        };

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            r#"
            select venue, symbol,
                   sum(funding_rate)::text as rate,
                   count(*) as count
            from funding_rates
            where funding_time >= $1
            group by venue, symbol
            "#,
        )
        .bind(since)
        .fetch_all(pool)
        .await?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let rate: String = row.try_get("rate")?;
            summaries.push(FundingSummary {
                venue: row.try_get("venue")?,
                symbol: row.try_get("symbol")?,
                rate: rate.parse::<Decimal>()?,
                count: row.try_get("count")?,
            });
        }
        Ok(summaries)
    }

    /// 已入库的资金费率条数。
    pub async fn funding_count(&self, venue: &str) -> anyhow::Result<usize> {
        sqlite_branch!(self, db => db.funding_count(venue).await);
        let Some(pool) = &self.pool else {
            let rows = self.funding.read().expect("资金费率锁不可用");
            return Ok(rows.iter().filter(|rate| rate.venue == venue).count());
        };

        let count: i64 = sqlx::query_scalar("select count(*) from funding_rates where venue = $1")
            .bind(venue)
            .fetch_one(pool)
            .await?;
        Ok(count as usize)
    }

    /// 删除结算时间早于 `before` 的资金费率，返回删除条数。
    pub async fn prune_funding_rates(&self, before: DateTime<Utc>) -> anyhow::Result<usize> {
        sqlite_branch!(self, db => db.prune_funding_rates(before).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.funding.write().expect("资金费率锁不可用");
            let total = rows.len();
            rows.retain(|rate| rate.funding_time >= before);
            return Ok(total - rows.len());
        };

        let result = sqlx::query("delete from funding_rates where funding_time < $1")
            .bind(before)
            .execute(pool)
            .await?;
        Ok(result.rows_affected() as usize)
    }

    /// 记录一条账户权益快照。
    pub async fn save_equity_point(&self, point: &EquityPoint) -> anyhow::Result<()> {
        sqlite_branch!(self, db => db.save_equity_point(point).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.equity.write().expect("权益锁不可用");
            rows.push(point.clone());
            if rows.len() > MAX_MEMORY_EQUITY_ROWS {
                rows.sort_by_key(|row| row.observed_at);
                let excess = rows.len() - MAX_MEMORY_EQUITY_ROWS;
                rows.drain(0..excess);
            }
            return Ok(());
        };

        sqlx::query(
            r#"
            insert into account_equity_points (account_id, venue, equity_usd, observed_at)
            values ($1, $2, $3::numeric, $4)
            "#,
        )
        .bind(point.account_id)
        .bind(&point.venue)
        .bind(point.equity_usd.to_string())
        .bind(point.observed_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 时间窗口内的权益快照，按时间升序返回。
    pub async fn equity_history(
        &self,
        since: DateTime<Utc>,
        account_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<EquityPoint>> {
        sqlite_branch!(self, db => db.equity_history(since, account_id).await);
        let Some(pool) = &self.pool else {
            let rows = self.equity.read().expect("权益锁不可用");
            let mut points: Vec<EquityPoint> = rows
                .iter()
                .filter(|point| point.observed_at >= since)
                .filter(|point| account_id.map(|id| point.account_id == id).unwrap_or(true))
                .cloned()
                .collect();
            points.sort_by_key(|point| point.observed_at);
            return Ok(points);
        };

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            r#"
            select account_id, venue, equity_usd::text as equity_usd, observed_at
            from account_equity_points
            where observed_at >= $1 and ($2::uuid is null or account_id = $2)
            order by observed_at asc
            "#,
        )
        .bind(since)
        .bind(account_id)
        .fetch_all(pool)
        .await?;

        let mut points = Vec::with_capacity(rows.len());
        for row in rows {
            let equity: String = row.try_get("equity_usd")?;
            points.push(EquityPoint {
                account_id: row.try_get("account_id")?,
                venue: row.try_get("venue")?,
                equity_usd: equity.parse::<Decimal>()?,
                observed_at: row.try_get("observed_at")?,
            });
        }
        Ok(points)
    }

    /// 删除账户时一并清空其历史快照。
    pub async fn clear_equity_points(&self, account_id: Uuid) -> anyhow::Result<()> {
        sqlite_branch!(self, db => db.clear_equity_points(account_id).await);
        let Some(pool) = &self.pool else {
            self.equity
                .write()
                .expect("权益锁不可用")
                .retain(|point| point.account_id != account_id);
            return Ok(());
        };

        sqlx::query("delete from account_equity_points where account_id = $1")
            .bind(account_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// 读取全部策略定义。未连接数据库时返回内存快照。
    pub async fn load_strategies(&self) -> Vec<StrategyRecord> {
        sqlite_branch!(self, db => db.load_strategies().await);
        let Some(pool) = &self.pool else {
            return self.strategies.read().expect("策略锁不可用").clone();
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select payload from strategies where payload is not null order by created_at asc",
        )
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "读取策略失败");
                return Vec::new();
            }
        };

        rows.iter().filter_map(decode_strategy_row).collect()
    }

    /// 读取已启用的现货 / 永续交易所配置。未连接数据库时返回空列表。
    pub async fn exchange_venues(&self) -> Vec<ExchangeVenueConfig> {
        sqlite_branch!(self, db => db.exchange_venues().await);
        let Some(pool) = &self.pool else {
            return Vec::new();
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select venue, segment, display_name from exchange_venues where enabled order by sort_order asc, venue asc",
        )
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "读取交易所配置失败");
                return Vec::new();
            }
        };

        rows.iter()
            .filter_map(|row| {
                Some(ExchangeVenueConfig {
                    venue: row.try_get("venue").ok()?,
                    segment: row.try_get("segment").ok()?,
                    display_name: row.try_get("display_name").ok()?,
                })
            })
            .collect()
    }

    /// 读取已启用的交易对配置；同一交易所的现货与永续共享这份清单。
    pub async fn tradable_symbols(&self) -> Vec<TradableSymbolConfig> {
        sqlite_branch!(self, db => db.tradable_symbols().await);
        let Some(pool) = &self.pool else {
            return Vec::new();
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select venue, symbol from tradable_symbols where enabled order by venue asc, sort_order asc, symbol asc",
        )
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "读取交易对配置失败");
                return Vec::new();
            }
        };

        rows.iter()
            .filter_map(|row| {
                Some(TradableSymbolConfig {
                    venue: row.try_get("venue").ok()?,
                    symbol: row.try_get("symbol").ok()?,
                })
            })
            .collect()
    }

    /// 资金费率采集器发现新合约后写入配置，已有交易对保持原有启用状态与排序。
    pub async fn upsert_tradable_symbols(&self, venue: &str, symbols: &[String]) {
        sqlite_branch!(self, db => db.upsert_tradable_symbols(venue, symbols).await);
        let Some(pool) = &self.pool else {
            return;
        };
        if symbols.is_empty() {
            return;
        }

        let result = sqlx::query(
            r#"
            insert into tradable_symbols (venue, symbol, sort_order)
            select $1, symbol, 1000 from unnest($2::text[]) as symbol
            on conflict (venue, symbol) do nothing
            "#,
        )
        .bind(venue)
        .bind(symbols.to_vec())
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, venue, "写入交易对配置失败");
        }
    }

    /// 读取通知配置；未连接数据库时返回默认值。
    pub async fn notification_settings(&self) -> NotificationSettings {
        sqlite_branch!(self, db => db.notification_settings().await);
        let Some(pool) = &self.pool else {
            return NotificationSettings::default();
        };

        let row = match sqlx::query(
            r#"
            select news_enabled, news_sources, funding_enabled, funding_min_annualized_pct,
                   funding_cooldown_secs, telegram_enabled, desktop_enabled
            from notification_settings where id = 1
            "#,
        )
        .fetch_optional(pool)
        .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::error!(%error, "读取通知配置失败");
                return NotificationSettings::default();
            }
        };
        let Some(row) = row else {
            return NotificationSettings::default();
        };

        NotificationSettings {
            news_enabled: row.try_get("news_enabled").unwrap_or(true),
            news_sources: row.try_get("news_sources").unwrap_or_default(),
            funding_enabled: row.try_get("funding_enabled").unwrap_or(true),
            funding_min_annualized_pct: row
                .try_get("funding_min_annualized_pct")
                .unwrap_or_else(|_| Decimal::from(10)),
            // 数据库列是 int4，按 i32 解码后再转 i64。
            funding_cooldown_secs: row
                .try_get::<i32, _>("funding_cooldown_secs")
                .map(i64::from)
                .unwrap_or(6 * 3600),
            telegram_enabled: row.try_get("telegram_enabled").unwrap_or(true),
            desktop_enabled: row.try_get("desktop_enabled").unwrap_or(true),
        }
    }

    /// 保存通知配置；内存模式下由调用方自行保存，这里不报错。
    pub async fn save_notification_settings(&self, settings: &NotificationSettings) {
        sqlite_branch!(self, db => db.save_notification_settings(settings).await);
        let Some(pool) = &self.pool else {
            return;
        };

        let result = sqlx::query(
            r#"
            insert into notification_settings (
                id, news_enabled, news_sources, funding_enabled, funding_min_annualized_pct,
                funding_cooldown_secs, telegram_enabled, desktop_enabled, updated_at
            )
            values (1, $1, $2, $3, $4, $5, $6, $7, now())
            on conflict (id) do update
                set news_enabled = excluded.news_enabled,
                    news_sources = excluded.news_sources,
                    funding_enabled = excluded.funding_enabled,
                    funding_min_annualized_pct = excluded.funding_min_annualized_pct,
                    funding_cooldown_secs = excluded.funding_cooldown_secs,
                    telegram_enabled = excluded.telegram_enabled,
                    desktop_enabled = excluded.desktop_enabled,
                    updated_at = now()
            "#,
        )
        .bind(settings.news_enabled)
        .bind(settings.news_sources.clone())
        .bind(settings.funding_enabled)
        .bind(settings.funding_min_annualized_pct)
        .bind(settings.funding_cooldown_secs)
        .bind(settings.telegram_enabled)
        .bind(settings.desktop_enabled)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, "写入通知配置失败");
        }
    }

    /// 写入策略定义与运行状态快照。
    pub async fn save_strategy(&self, record: &StrategyRecord) {
        sqlite_branch!(self, db => db.save_strategy(record).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.strategies.write().expect("策略锁不可用");
            match rows.iter_mut().find(|existing| existing.id == record.id) {
                Some(existing) => *existing = record.clone(),
                None => rows.push(record.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(record) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, strategy_id = %record.id, "策略序列化失败，未写入数据库");
                return;
            }
        };
        let kind = enum_label(&record.definition.kind);
        let status = enum_label(&record.status);
        let source = enum_label(&record.source);

        let result = sqlx::query(
            r#"
            insert into strategies
                (id, name, kind, status, version, payload, source)
            values ($1, $2, $3, $4, $5, $6, $7)
            on conflict (id) do update
                set name = excluded.name,
                    kind = excluded.kind,
                    status = excluded.status,
                    version = excluded.version,
                    payload = excluded.payload,
                    source = excluded.source,
                    updated_at = now()
            "#,
        )
        .bind(record.id)
        .bind(record.name())
        .bind(kind)
        .bind(status)
        .bind(record.version as i32)
        .bind(payload)
        .bind(source)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, strategy_id = %record.id, "写入策略失败");
        }
    }

    /// 追加一条版本快照；同一 (strategy_id, version) 只保留首次写入。
    pub async fn save_strategy_version(&self, record: &StrategyRecord) {
        sqlite_branch!(self, db => db.save_strategy_version(record).await);
        let snapshot = StrategyVersion::from_record(record);
        let Some(pool) = &self.pool else {
            let mut rows = self.strategy_versions.write().expect("策略版本锁不可用");
            match rows.iter_mut().find(|existing| {
                existing.strategy_id == record.id && existing.version == record.version
            }) {
                Some(existing) => *existing = snapshot,
                None => rows.push(snapshot),
            }
            return;
        };

        let payload = match serde_json::to_value(&snapshot) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, strategy_id = %record.id, "策略版本序列化失败");
                return;
            }
        };
        let result = sqlx::query(
            r#"
            insert into strategy_versions (strategy_id, version, payload)
            values ($1, $2, $3)
            on conflict (strategy_id, version) do update
                set payload = excluded.payload
            "#,
        )
        .bind(record.id)
        .bind(record.version as i32)
        .bind(payload)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, strategy_id = %record.id, "写入策略版本失败");
        }
    }

    /// 写入运行实例；重复写入只更新状态、停止时间与心跳。
    pub async fn save_strategy_run(&self, run: &StrategyRun) {
        sqlite_branch!(self, db => db.save_strategy_run(run).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.strategy_runs.write().expect("运行实例锁不可用");
            match rows.iter_mut().find(|existing| existing.id == run.id) {
                Some(existing) => *existing = run.clone(),
                None => rows.push(run.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(run) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, run_id = %run.id, "运行实例序列化失败");
                return;
            }
        };
        let result = sqlx::query(
            r#"
            insert into strategy_runs
                (id, strategy_id, strategy_version, scope, status, long_venue, short_venue,
                 symbols, payload, started_at, stopped_at, last_heartbeat_at)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            on conflict (id) do update
                set status = excluded.status,
                    payload = excluded.payload,
                    stopped_at = excluded.stopped_at,
                    last_heartbeat_at = excluded.last_heartbeat_at
            "#,
        )
        .bind(run.id)
        .bind(run.strategy_id)
        .bind(run.strategy_version as i32)
        .bind(enum_label(&run.scope))
        .bind(enum_label(&run.status))
        .bind(&run.long_venue)
        .bind(&run.short_venue)
        .bind(&run.symbols)
        .bind(payload)
        .bind(run.started_at)
        .bind(run.stopped_at)
        .bind(run.last_heartbeat_at)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, run_id = %run.id, "写入运行实例失败");
        }
    }

    /// 按开始时间倒序返回策略的运行实例。
    pub async fn strategy_runs(&self, strategy_id: Uuid, limit: i64) -> Vec<StrategyRun> {
        sqlite_branch!(self, db => db.strategy_runs(strategy_id, limit).await);
        let Some(pool) = &self.pool else {
            let rows = self.strategy_runs.read().expect("运行实例锁不可用");
            let mut runs: Vec<StrategyRun> = rows
                .iter()
                .filter(|run| run.strategy_id == strategy_id)
                .cloned()
                .collect();
            runs.sort_by(|left, right| right.started_at.cmp(&left.started_at));
            runs.truncate(limit.max(1) as usize);
            return runs;
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select payload from strategy_runs where strategy_id = $1 order by started_at desc limit $2",
        )
        .bind(strategy_id)
        .bind(limit.max(1))
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, %strategy_id, "读取运行实例失败");
                return Vec::new();
            }
        };
        decode_payload_rows(&rows)
    }

    /// 按 id 读取单个运行实例。
    pub async fn strategy_run(&self, run_id: Uuid) -> Option<StrategyRun> {
        sqlite_branch!(self, db => db.strategy_run(run_id).await);
        let Some(pool) = &self.pool else {
            return self
                .strategy_runs
                .read()
                .expect("运行实例锁不可用")
                .iter()
                .find(|run| run.id == run_id)
                .cloned();
        };

        let row = match sqlx::query("select payload from strategy_runs where id = $1")
            .bind(run_id)
            .fetch_optional(pool)
            .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::error!(%error, %run_id, "读取运行实例失败");
                return None;
            }
        };
        row.and_then(|row| {
            let payload: serde_json::Value = row.try_get("payload").ok()?;
            serde_json::from_value(payload).ok()
        })
    }

    /// 追加一条运行事件。
    pub async fn append_strategy_run_event(&self, event: &StrategyRunEvent) {
        sqlite_branch!(self, db => db.append_strategy_run_event(event).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.strategy_run_events.write().expect("运行事件锁不可用");
            rows.push(event.clone());
            let excess = rows.len().saturating_sub(MAX_MEMORY_RUN_EVENTS);
            if excess > 0 {
                rows.drain(0..excess);
            }
            return;
        };

        let payload = match serde_json::to_value(event) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, run_id = %event.run_id, "运行事件序列化失败");
                return;
            }
        };
        let result = sqlx::query(
            r#"
            insert into strategy_run_events (run_id, strategy_id, level, payload)
            values ($1, $2, $3, $4)
            "#,
        )
        .bind(event.run_id)
        .bind(event.strategy_id)
        .bind(enum_label(&event.level))
        .bind(payload)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, run_id = %event.run_id, "写入运行事件失败");
        }
    }

    /// 按时间正序返回运行实例的事件。
    pub async fn strategy_run_events(&self, run_id: Uuid, limit: i64) -> Vec<StrategyRunEvent> {
        sqlite_branch!(self, db => db.strategy_run_events(run_id, limit).await);
        let Some(pool) = &self.pool else {
            let rows = self.strategy_run_events.read().expect("运行事件锁不可用");
            let mut events: Vec<StrategyRunEvent> = rows
                .iter()
                .filter(|event| event.run_id == run_id)
                .cloned()
                .collect();
            events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
            events.truncate(limit.max(1) as usize);
            events.reverse();
            return events;
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select payload from strategy_run_events where run_id = $1 order by created_at desc limit $2",
        )
        .bind(run_id)
        .bind(limit.max(1))
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, %run_id, "读取运行事件失败");
                return Vec::new();
            }
        };
        let mut events: Vec<StrategyRunEvent> = decode_payload_rows(&rows);
        events.reverse();
        events
    }

    /// 按版本号倒序返回历史快照。
    pub async fn strategy_versions(&self, strategy_id: Uuid) -> Vec<StrategyVersion> {
        sqlite_branch!(self, db => db.strategy_versions(strategy_id).await);
        let Some(pool) = &self.pool else {
            let rows = self.strategy_versions.read().expect("策略版本锁不可用");
            let mut versions: Vec<StrategyVersion> = rows
                .iter()
                .filter(|version| version.strategy_id == strategy_id)
                .cloned()
                .collect();
            versions.sort_by(|a, b| b.version.cmp(&a.version));
            return versions;
        };

        let rows: Vec<sqlx::postgres::PgRow> = match sqlx::query(
            "select payload from strategy_versions where strategy_id = $1 order by version desc",
        )
        .bind(strategy_id)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, %strategy_id, "读取策略版本失败");
                return Vec::new();
            }
        };

        rows.iter()
            .filter_map(|row| {
                let payload: serde_json::Value = row.try_get("payload").ok()?;
                serde_json::from_value(payload).ok()
            })
            .collect()
    }

    /// 读取策略当前沙箱源码。
    pub async fn strategy_code(&self, strategy_id: Uuid) -> Option<StrategyCode> {
        sqlite_branch!(self, db => db.strategy_code(strategy_id).await);
        let Some(pool) = &self.pool else {
            return self
                .strategy_codes
                .read()
                .expect("策略代码锁不可用")
                .get(&strategy_id)
                .cloned();
        };

        match sqlx::query_scalar::<_, serde_json::Value>(
            "select payload from strategy_code where strategy_id = $1",
        )
        .bind(strategy_id)
        .fetch_optional(pool)
        .await
        {
            Ok(payload) => payload.and_then(|payload| serde_json::from_value(payload).ok()),
            Err(error) => {
                tracing::error!(%error, %strategy_id, "读取策略代码失败");
                None
            }
        }
    }

    /// 保存策略源码；每个策略只保留最新一份，代码历史由策略版本承担。
    pub async fn save_strategy_code(&self, code: &StrategyCode) {
        sqlite_branch!(self, db => db.save_strategy_code(code).await);
        let Some(pool) = &self.pool else {
            self.strategy_codes
                .write()
                .expect("策略代码锁不可用")
                .insert(code.strategy_id, code.clone());
            return;
        };

        let payload = match serde_json::to_value(code) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, strategy_id = %code.strategy_id, "策略代码序列化失败");
                return;
            }
        };
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_code (strategy_id, payload)
            values ($1, $2)
            on conflict (strategy_id) do update
                set payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(code.strategy_id)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, strategy_id = %code.strategy_id, "写入策略代码失败");
        }
    }

    /// 删除策略及其版本历史。已有仓位与订单不受影响。
    pub async fn delete_strategy(&self, strategy_id: Uuid) {
        sqlite_branch!(self, db => db.delete_strategy(strategy_id).await);
        let Some(pool) = &self.pool else {
            self.strategies
                .write()
                .expect("策略锁不可用")
                .retain(|record| record.id != strategy_id);
            self.strategy_versions
                .write()
                .expect("策略版本锁不可用")
                .retain(|version| version.strategy_id != strategy_id);
            self.strategy_codes
                .write()
                .expect("策略代码锁不可用")
                .remove(&strategy_id);
            return;
        };

        if let Err(error) = sqlx::query("delete from strategy_code where strategy_id = $1")
            .bind(strategy_id)
            .execute(pool)
            .await
        {
            tracing::error!(%error, %strategy_id, "删除策略代码失败");
        }
        if let Err(error) = sqlx::query("delete from strategy_versions where strategy_id = $1")
            .bind(strategy_id)
            .execute(pool)
            .await
        {
            tracing::error!(%error, %strategy_id, "删除策略版本失败");
        }
        if let Err(error) = sqlx::query("delete from strategies where id = $1")
            .bind(strategy_id)
            .execute(pool)
            .await
        {
            tracing::error!(%error, %strategy_id, "删除策略失败");
        }
    }

    /// 读取策略市场全部数据。数据量小（单一执行器用户），一次拉取便于计算榜单。
    pub async fn market_snapshot(&self) -> MarketSnapshot {
        sqlite_branch!(self, db => db.market_snapshot().await);
        let Some(pool) = &self.pool else {
            return MarketSnapshot {
                authors: self.authors.read().expect("作者锁不可用").clone(),
                publications: self.publications.read().expect("发布锁不可用").clone(),
                performance: self.performance.read().expect("表现记录锁不可用").clone(),
                subscriptions: self.subscriptions.read().expect("订阅锁不可用").clone(),
                settlements: self.settlements.read().expect("结算锁不可用").clone(),
            };
        };

        MarketSnapshot {
            authors: load_payloads(pool, "strategy_authors").await,
            publications: load_payloads(pool, "strategy_publications").await,
            performance: load_payloads(pool, "strategy_performance_records").await,
            subscriptions: load_payloads(pool, "strategy_subscriptions").await,
            settlements: load_payloads(pool, "strategy_settlements").await,
        }
    }

    pub async fn save_author_card(&self, card: &StrategyAuthorCard) {
        sqlite_branch!(self, db => db.save_author_card(card).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.authors.write().expect("作者锁不可用");
            match rows.iter_mut().find(|existing| existing.author_id == card.author_id) {
                Some(existing) => *existing = card.clone(),
                None => rows.push(card.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(card) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "作者卡片序列化失败，未写入数据库");
                return;
            }
        };
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_authors (author_id, payload)
            values ($1, $2)
            on conflict (author_id) do update
                set payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(card.author_id)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, "写入作者卡片失败");
        }
    }

    pub async fn save_publication(&self, publication: &StrategyPublication) {
        sqlite_branch!(self, db => db.save_publication(publication).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.publications.write().expect("发布锁不可用");
            match rows.iter_mut().find(|existing| existing.id == publication.id) {
                Some(existing) => *existing = publication.clone(),
                None => rows.push(publication.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(publication) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "策略发布序列化失败，未写入数据库");
                return;
            }
        };
        let status = enum_label(&publication.status);
        let result = sqlx::query(
            r#"
            insert into strategy_publications
                (id, strategy_id, author_id, status, strategy_version, payload)
            values ($1, $2, $3, $4, $5, $6)
            on conflict (id) do update
                set status = excluded.status,
                    strategy_version = excluded.strategy_version,
                    payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(publication.id)
        .bind(publication.strategy_id)
        .bind(publication.author_id)
        .bind(status)
        .bind(publication.strategy_version as i32)
        .bind(payload)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, "写入策略发布失败");
        }
    }

    pub async fn save_performance(&self, record: &StrategyPerformanceRecord) {
        sqlite_branch!(self, db => db.save_performance(record).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.performance.write().expect("表现记录锁不可用");
            match rows.iter_mut().find(|existing| existing.id == record.id) {
                Some(existing) => *existing = record.clone(),
                None => rows.push(record.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(record) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "表现记录序列化失败，未写入数据库");
                return;
            }
        };
        let source = record.source.label();
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_performance_records (id, publication_id, source, payload)
            values ($1, $2, $3, $4)
            on conflict (id) do update
                set source = excluded.source,
                    payload = excluded.payload
            "#,
        )
        .bind(record.id)
        .bind(record.publication_id)
        .bind(source)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, "写入表现记录失败");
        }
    }

    pub async fn save_subscription(&self, subscription: &StrategySubscription) {
        sqlite_branch!(self, db => db.save_subscription(subscription).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.subscriptions.write().expect("订阅锁不可用");
            match rows.iter_mut().find(|existing| existing.id == subscription.id) {
                Some(existing) => *existing = subscription.clone(),
                None => rows.push(subscription.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(subscription) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "订阅序列化失败，未写入数据库");
                return;
            }
        };
        let status = enum_label(&subscription.status);
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_subscriptions
                (id, publication_id, subscriber_id, status, payload)
            values ($1, $2, $3, $4, $5)
            on conflict (id) do update
                set status = excluded.status,
                    payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(subscription.id)
        .bind(subscription.publication_id)
        .bind(subscription.subscriber_id)
        .bind(status)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, "写入订阅失败");
        }
    }

    pub async fn save_settlement(&self, settlement: &StrategySettlement) {
        sqlite_branch!(self, db => db.save_settlement(settlement).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.settlements.write().expect("结算锁不可用");
            match rows.iter_mut().find(|existing| {
                existing.author_id == settlement.author_id && existing.period == settlement.period
            }) {
                Some(existing) => *existing = settlement.clone(),
                None => rows.push(settlement.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(settlement) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, "结算单序列化失败，未写入数据库");
                return;
            }
        };
        let status = enum_label(&settlement.status);
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_settlements (id, author_id, period, status, payload)
            values ($1, $2, $3, $4, $5)
            on conflict (author_id, period) do update
                set status = excluded.status,
                    payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(settlement.id)
        .bind(settlement.author_id)
        .bind(&settlement.period)
        .bind(status)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, "写入结算单失败");
        }
    }

    /// 读取全部沙箱模块元数据。
    pub async fn sandbox_artifacts(&self) -> Vec<SandboxArtifact> {
        sqlite_branch!(self, db => db.sandbox_artifacts().await);
        let Some(pool) = &self.pool else {
            return self
                .sandbox_artifacts
                .read()
                .expect("沙箱模块锁不可用")
                .clone();
        };
        load_payloads(pool, "sandbox_artifacts").await
    }

    /// 保存模块元数据与字节。相同 sha256 视为同一模块，不重复存储。
    pub async fn save_sandbox_artifact(
        &self,
        artifact: &SandboxArtifact,
        wasm: &[u8],
    ) -> anyhow::Result<()> {
        sqlite_branch!(self, db => db.save_sandbox_artifact(artifact, wasm).await);
        let Some(pool) = &self.pool else {
            {
                let mut rows = self
                    .sandbox_artifacts
                    .write()
                    .expect("沙箱模块锁不可用");
                match rows.iter_mut().find(|existing| existing.sha256 == artifact.sha256) {
                    Some(existing) => *existing = artifact.clone(),
                    None => rows.push(artifact.clone()),
                }
            }
            self.sandbox_modules
                .write()
                .expect("沙箱模块锁不可用")
                .insert(artifact.id, wasm.to_vec());
            return Ok(());
        };

        let payload = serde_json::to_value(artifact)?;
        sqlx::query(
            r#"
            insert into sandbox_artifacts
                (id, name, sha256, size_bytes, abi_version, source, payload, wasm)
            values ($1, $2, $3, $4, $5, $6, $7, $8)
            on conflict (id) do update
                set name = excluded.name,
                    abi_version = excluded.abi_version,
                    payload = excluded.payload,
                    wasm = excluded.wasm
            "#,
        )
        .bind(artifact.id)
        .bind(&artifact.name)
        .bind(&artifact.sha256)
        .bind(artifact.size_bytes as i64)
        .bind(artifact.abi_version.map(|version| version as i32))
        .bind(enum_label(&artifact.source))
        .bind(payload)
        .bind(wasm)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 按 id 读取模块字节；运行器据此执行沙箱。
    pub async fn sandbox_module(&self, id: Uuid) -> Option<Vec<u8>> {
        sqlite_branch!(self, db => db.sandbox_module(id).await);
        let Some(pool) = &self.pool else {
            return self
                .sandbox_modules
                .read()
                .expect("沙箱模块锁不可用")
                .get(&id)
                .cloned();
        };

        sqlx::query_scalar::<_, Vec<u8>>("select wasm from sandbox_artifacts where id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
    }

    /// 读取全部策略模板。未连接数据库时返回内存快照。
    pub async fn strategy_templates(&self) -> Vec<StrategyTemplate> {
        sqlite_branch!(self, db => db.strategy_templates().await);
        let Some(pool) = &self.pool else {
            return self
                .strategy_templates
                .read()
                .expect("模板锁不可用")
                .clone();
        };
        load_payloads(pool, "strategy_templates").await
    }

    /// 写入策略模板，按 `key` 幂等。
    pub async fn save_strategy_template(&self, template: &StrategyTemplate) {
        sqlite_branch!(self, db => db.save_strategy_template(template).await);
        let Some(pool) = &self.pool else {
            let mut rows = self.strategy_templates.write().expect("模板锁不可用");
            match rows.iter_mut().find(|existing| existing.key == template.key) {
                Some(existing) => *existing = template.clone(),
                None => rows.push(template.clone()),
            }
            return;
        };

        let payload = match serde_json::to_value(template) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, key = %template.key, "策略模板序列化失败");
                return;
            }
        };
        if let Err(error) = sqlx::query(
            r#"
            insert into strategy_templates (key, name, payload)
            values ($1, $2, $3)
            on conflict (key) do update
                set name = excluded.name,
                    payload = excluded.payload,
                    updated_at = now()
            "#,
        )
        .bind(&template.key)
        .bind(&template.name)
        .bind(payload)
        .execute(pool)
        .await
        {
            tracing::error!(%error, key = %template.key, "写入策略模板失败");
        }
    }

    fn upsert_funding_memory(&self, rates: &[FundingRate]) -> usize {
        let mut rows = self.funding.write().expect("资金费率锁不可用");
        let mut written = 0;
        for rate in rates {
            match rows.iter_mut().find(|row| row.same_key(rate)) {
                Some(existing) => {
                    if existing.funding_rate != rate.funding_rate
                        || existing.mark_price != rate.mark_price
                    {
                        *existing = rate.clone();
                        written += 1;
                    }
                }
                None => {
                    rows.push(rate.clone());
                    written += 1;
                }
            }
        }

        if rows.len() > MAX_MEMORY_FUNDING_ROWS {
            rows.sort_by_key(|row| row.funding_time);
            let excess = rows.len() - MAX_MEMORY_FUNDING_ROWS;
            rows.drain(0..excess);
        }
        written
    }

    fn funding_rates_memory(
        &self,
        venue: &str,
        symbol: Option<&str>,
        limit: i64,
    ) -> Vec<FundingRate> {
        let mut rows: Vec<FundingRate> = self
            .funding
            .read()
            .expect("资金费率锁不可用")
            .iter()
            .filter(|rate| rate.venue == venue)
            .filter(|rate| symbol.map(|symbol| rate.symbol == symbol).unwrap_or(true))
            .cloned()
            .collect();
        rows.sort_by(|a, b| b.funding_time.cmp(&a.funding_time));
        rows.truncate(limit.max(0) as usize);
        rows
    }
}

/// 读取只含 `payload` 列的表。表名来自内部常量，不接受外部输入。
async fn load_payloads<T: DeserializeOwned>(pool: &PgPool, table: &str) -> Vec<T> {
    let rows: Vec<sqlx::postgres::PgRow> =
        match sqlx::query(&format!("select payload from {table}")).fetch_all(pool).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, table, "读取市场数据失败");
                return Vec::new();
            }
        };
    rows.iter()
        .filter_map(|row| {
            let payload: serde_json::Value = row.try_get("payload").ok()?;
            serde_json::from_value(payload).ok()
        })
        .collect()
}

/// 把枚举序列化成与 API/数据库一致的 snake_case 标签。
fn enum_label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn decode_strategy_row(row: &sqlx::postgres::PgRow) -> Option<StrategyRecord> {
    let payload: serde_json::Value = row.try_get("payload").ok()?;
    serde_json::from_value(payload).ok()
}

/// 从 `select payload ...` 的结果里解码业务对象，单行失败不影响其余行。
fn decode_payload_rows<T: DeserializeOwned>(rows: &[sqlx::postgres::PgRow]) -> Vec<T> {
    rows.iter()
        .filter_map(|row| {
            let payload: serde_json::Value = row.try_get("payload").ok()?;
            serde_json::from_value(payload).ok()
        })
        .collect()
}

fn decode_funding_row(row: &sqlx::postgres::PgRow, venue: &str) -> anyhow::Result<FundingRate> {
    let rate: String = row.try_get("funding_rate")?;
    let mark_price: Option<String> = row.try_get("mark_price")?;
    Ok(FundingRate {
        venue: venue.to_string(),
        symbol: row.try_get("symbol")?,
        funding_time: row.try_get::<DateTime<Utc>, _>("funding_time")?,
        funding_rate: rate.parse::<Decimal>()?,
        mark_price: mark_price.and_then(|price| price.parse().ok()),
    })
}
