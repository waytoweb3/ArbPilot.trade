use std::{collections::HashMap, sync::RwLock};

use arb_core::{
    BacktestReport, Order, SandboxArtifact, StrategyAuthorCard, StrategyCode, StrategyDefinition,
    StrategyExecutionMode, StrategyPerformanceRecord, StrategyPublication, StrategyRecord,
    StrategySettlement, StrategySource, StrategyStatus, StrategySubscription, StrategyValidation,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use uuid::Uuid;

use crate::exchange::FundingRate;

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
    pub execution_mode: StrategyExecutionMode,
    pub definition: StrategyDefinition,
    pub validation: StrategyValidation,
    pub backtest: Option<BacktestReport>,
    pub saved_at: DateTime<Utc>,
}

impl StrategyVersion {
    fn from_record(record: &StrategyRecord) -> Self {
        Self {
            strategy_id: record.id,
            version: record.version,
            status: record.status,
            source: record.source,
            execution_mode: record.execution_mode,
            definition: record.definition.clone(),
            validation: record.validation.clone(),
            backtest: record.last_backtest.clone(),
            saved_at: Utc::now(),
        }
    }
}

/// 内存模式下最多保留的资金费率条数，避免长时间运行无限增长。
const MAX_MEMORY_FUNDING_ROWS: usize = 50_000;
/// 内存模式下最多保留的权益快照条数。
const MAX_MEMORY_EQUITY_ROWS: usize = 20_000;

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
    /// 内存降级时的资金费率缓存，重启即丢。
    funding: RwLock<Vec<FundingRate>>,
    /// 内存降级时的账户权益快照，重启即丢。
    equity: RwLock<Vec<EquityPoint>>,
    /// 内存降级时的策略快照，重启即丢。
    strategies: RwLock<Vec<StrategyRecord>>,
    /// 内存降级时的策略版本历史，重启即丢。
    strategy_versions: RwLock<Vec<StrategyVersion>>,
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
}

impl Store {
    pub async fn connect_if_configured(database_url: Option<&str>) -> Self {
        let Some(url) = database_url else {
            tracing::info!("未配置 DATABASE_URL，执行器以内存模式运行");
            return Self::memory();
        };

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
                Self::with_pool(Some(pool))
            }
            Err(error) => {
                tracing::error!(%error, "数据库迁移失败，执行器以内存模式运行");
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
            funding: RwLock::new(Vec::new()),
            equity: RwLock::new(Vec::new()),
            strategies: RwLock::new(Vec::new()),
            strategy_versions: RwLock::new(Vec::new()),
            strategy_codes: RwLock::new(HashMap::new()),
            authors: RwLock::new(Vec::new()),
            publications: RwLock::new(Vec::new()),
            performance: RwLock::new(Vec::new()),
            subscriptions: RwLock::new(Vec::new()),
            settlements: RwLock::new(Vec::new()),
            sandbox_artifacts: RwLock::new(Vec::new()),
            sandbox_modules: RwLock::new(HashMap::new()),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.pool.is_some()
    }

    pub async fn save_order(&self, order: &Order) {
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

    /// 写入资金费率，按 (venue, symbol, funding_time) 幂等。
    ///
    /// 返回实际写入或更新的条数；未连接数据库时写入内存缓存（重启即丢）。
    pub async fn upsert_funding_rates(&self, rates: &[FundingRate]) -> anyhow::Result<usize> {
        if rates.is_empty() {
            return Ok(0);
        }

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

    /// 写入策略定义与运行状态快照。
    pub async fn save_strategy(&self, record: &StrategyRecord) {
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
        let execution_mode = enum_label(&record.execution_mode);

        let result = sqlx::query(
            r#"
            insert into strategies
                (id, name, kind, status, version, payload, source, execution_mode)
            values ($1, $2, $3, $4, $5, $6, $7, $8)
            on conflict (id) do update
                set name = excluded.name,
                    kind = excluded.kind,
                    status = excluded.status,
                    version = excluded.version,
                    payload = excluded.payload,
                    source = excluded.source,
                    execution_mode = excluded.execution_mode,
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
        .bind(execution_mode)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::error!(%error, strategy_id = %record.id, "写入策略失败");
        }
    }

    /// 追加一条版本快照；同一 (strategy_id, version) 只保留首次写入。
    pub async fn save_strategy_version(&self, record: &StrategyRecord) {
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

    /// 按版本号倒序返回历史快照。
    pub async fn strategy_versions(&self, strategy_id: Uuid) -> Vec<StrategyVersion> {
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
