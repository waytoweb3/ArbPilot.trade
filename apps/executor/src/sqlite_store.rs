use std::{collections::HashMap, path::Path};

use arb_core::{
    Order, SandboxArtifact, StrategyAuthorCard, StrategyCode, StrategyPerformanceRecord,
    StrategyPublication, StrategyRecord, StrategySettlement, StrategySubscription,
};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Row, SqlitePool,
};
use uuid::Uuid;

use crate::{
    conditional::ConditionalOrder,
    exchange::FundingRate,
    store::{
        EquityPoint, ExchangeVenueConfig, FundingSummary, MarketSnapshot, NotificationSettings,
        StrategyRun, StrategyRunEvent, StrategyVersion, TradableSymbolConfig,
    },
    templates::StrategyTemplate,
};

/// 桌面端本地数据库的表结构。
///
/// 本地执行器只服务单个用户，业务对象以 JSON 文档保存，资金费率与权益曲线
/// 单独建表用于时间范围查询。所有数据都留在本机，不包含任何交易所密钥。
const SCHEMA: &str = r#"
create table if not exists documents (
    collection text not null,
    id text not null,
    payload text not null,
    created_at integer not null,
    updated_at integer not null,
    primary key (collection, id)
);

create table if not exists funding_rates (
    venue text not null,
    symbol text not null,
    funding_time integer not null,
    funding_rate text not null,
    mark_price text,
    ingested_at integer not null,
    primary key (venue, symbol, funding_time)
);

create index if not exists funding_rates_venue_time_idx
    on funding_rates (venue, funding_time desc);

create table if not exists account_equity_points (
    id integer primary key autoincrement,
    account_id text not null,
    venue text not null,
    equity_usd text not null,
    observed_at integer not null
);

create index if not exists account_equity_points_observed_idx
    on account_equity_points (observed_at asc);

create table if not exists exchange_venues (
    venue text not null,
    segment text not null,
    display_name text not null default '',
    enabled integer not null default 1,
    sort_order integer not null default 0,
    primary key (venue, segment)
);

create table if not exists tradable_symbols (
    venue text not null,
    symbol text not null,
    enabled integer not null default 1,
    sort_order integer not null default 0,
    primary key (venue, symbol)
);

create table if not exists sandbox_modules (
    id text primary key,
    wasm blob not null
);
"#;

/// 与 PostgreSQL 迁移 0010 保持一致的初始市场配置。
const SEED_VENUES: &[(&str, &str, &str, i64)] = &[
    ("binance-spot", "spot", "Binance Spot", 10),
    ("okx-spot", "spot", "OKX Spot", 20),
    ("binance", "perp", "Binance Perp", 10),
    ("okx", "perp", "OKX Swap", 20),
    ("bybit", "perp", "Bybit Perp", 30),
    ("bitget", "perp", "Bitget Perp", 40),
    ("gate", "perp", "Gate Perp", 50),
    ("aster", "perp", "Aster Perp", 60),
    ("hyperliquid", "perp", "Hyperliquid Perp", 70),
    ("lighter", "perp", "Lighter Perp", 80),
    ("htx", "perp", "HTX Perp", 90),
    ("mexc", "perp", "MEXC Perp", 100),
    ("bingx", "perp", "BingX Perp", 110),
];

const SEED_SYMBOLS: &[&str] = &[
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "BNBUSDT", "DOGEUSDT", "ADAUSDT", "LINKUSDT",
    "AVAXUSDT", "TONUSDT",
];

const SEED_UNIVERSE: &[&str] = &[
    "binance",
    "okx",
    "bybit",
    "bitget",
    "gate",
    "aster",
    "hyperliquid",
    "lighter",
    "htx",
    "mexc",
    "bingx",
];

/// 本地 SQLite 存储。
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    pub async fn connect(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        seed_market_config(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn save_order(&self, order: &Order) {
        if let Err(error) = self.put_doc("orders", &order.id.to_string(), order).await {
            tracing::error!(%error, order_id = %order.id, "写入订单失败");
        }
    }

    pub async fn append_order_event(
        &self,
        order_id: Uuid,
        event_type: &str,
        detail: serde_json::Value,
    ) {
        let payload = serde_json::json!({
            "order_id": order_id,
            "event_type": event_type,
            "detail": detail,
        });
        if let Err(error) = self
            .put_doc("order_events", &Uuid::new_v4().to_string(), &payload)
            .await
        {
            tracing::error!(%error, %order_id, event_type, "写入订单事件失败");
        }
    }

    pub async fn append_audit_log(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: serde_json::Value,
    ) {
        let payload = serde_json::json!({
            "actor": actor,
            "action": action,
            "target": target,
            "detail": detail,
        });
        if let Err(error) = self
            .put_doc("audit_log", &Uuid::new_v4().to_string(), &payload)
            .await
        {
            tracing::error!(%error, actor, action, "写入审计日志失败");
        }
    }

    pub async fn upsert_funding_rates(&self, rates: &[FundingRate]) -> anyhow::Result<usize> {
        let mut written = 0;
        for rate in rates {
            let result = sqlx::query(
                r#"
                insert into funding_rates
                    (venue, symbol, funding_time, funding_rate, mark_price, ingested_at)
                values (?, ?, ?, ?, ?, ?)
                on conflict (venue, symbol, funding_time) do update
                    set funding_rate = excluded.funding_rate,
                        mark_price = excluded.mark_price,
                        ingested_at = excluded.ingested_at
                "#,
            )
            .bind(&rate.venue)
            .bind(&rate.symbol)
            .bind(rate.funding_time.timestamp_millis())
            .bind(rate.funding_rate.to_string())
            .bind(rate.mark_price.map(|price| price.to_string()))
            .bind(Utc::now().timestamp_millis())
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => written += 1,
                Err(error) => {
                    tracing::error!(%error, symbol = %rate.symbol, "写入资金费率失败");
                    anyhow::bail!("写入资金费率失败: {error}");
                }
            }
        }
        Ok(written)
    }

    pub async fn funding_rates(
        &self,
        venue: &str,
        symbol: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let rows = sqlx::query(
            r#"
            select symbol, funding_time, funding_rate, mark_price
            from funding_rates
            where venue = ? and (? is null or symbol = ?)
            order by funding_time desc
            limit ?
            "#,
        )
        .bind(venue)
        .bind(symbol)
        .bind(symbol)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| decode_funding_row(row, venue))
            .collect()
    }

    pub async fn funding_latest(
        &self,
        venue: &str,
        symbols: &[String],
    ) -> anyhow::Result<Vec<FundingRate>> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select symbol, funding_time, funding_rate, mark_price from funding_rates where venue = ?",
        )
        .bind(venue)
        .fetch_all(&self.pool)
        .await?;

        let mut latest: HashMap<String, FundingRate> = HashMap::new();
        for row in &rows {
            let rate = decode_funding_row(row, venue)?;
            if !symbols.iter().any(|symbol| symbol == &rate.symbol) {
                continue;
            }
            let replace = latest
                .get(&rate.symbol)
                .map(|current| rate.funding_time > current.funding_time)
                .unwrap_or(true);
            if replace {
                latest.insert(rate.symbol.clone(), rate);
            }
        }
        Ok(latest.into_values().collect())
    }

    pub async fn funding_settlements(
        &self,
        venue: &str,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<(String, DateTime<Utc>)>> {
        let rows = sqlx::query(
            r#"
            select symbol, funding_time
            from funding_rates
            where venue = ? and funding_time >= ?
            order by symbol, funding_time
            "#,
        )
        .bind(venue)
        .bind(since.timestamp_millis())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                Ok((
                    row.try_get("symbol")?,
                    from_millis(row.try_get("funding_time")?),
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?)
    }

    pub async fn funding_summary(
        &self,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<FundingSummary>> {
        let rows = sqlx::query(
            "select venue, symbol, funding_rate from funding_rates where funding_time >= ?",
        )
        .bind(since.timestamp_millis())
        .fetch_all(&self.pool)
        .await?;

        let mut grouped: HashMap<(String, String), (Decimal, i64)> = HashMap::new();
        for row in rows {
            let venue: String = row.try_get("venue")?;
            let symbol: String = row.try_get("symbol")?;
            let rate: String = row.try_get("funding_rate")?;
            let entry = grouped.entry((venue, symbol)).or_default();
            entry.0 += rate.parse::<Decimal>()?;
            entry.1 += 1;
        }
        Ok(grouped
            .into_iter()
            .map(|((venue, symbol), (rate, count))| FundingSummary {
                venue,
                symbol,
                rate,
                count,
            })
            .collect())
    }

    pub async fn funding_count(&self, venue: &str) -> anyhow::Result<usize> {
        let count: i64 = sqlx::query_scalar("select count(*) from funding_rates where venue = ?")
            .bind(venue)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    pub async fn prune_funding_rates(&self, before: DateTime<Utc>) -> anyhow::Result<usize> {
        let result = sqlx::query("delete from funding_rates where funding_time < ?")
            .bind(before.timestamp_millis())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() as usize)
    }

    pub async fn save_equity_point(&self, point: &EquityPoint) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            insert into account_equity_points (account_id, venue, equity_usd, observed_at)
            values (?, ?, ?, ?)
            "#,
        )
        .bind(point.account_id.to_string())
        .bind(&point.venue)
        .bind(point.equity_usd.to_string())
        .bind(point.observed_at.timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn equity_history(
        &self,
        since: DateTime<Utc>,
        account_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<EquityPoint>> {
        let rows = sqlx::query(
            r#"
            select account_id, venue, equity_usd, observed_at
            from account_equity_points
            where observed_at >= ? and (? is null or account_id = ?)
            order by observed_at asc
            "#,
        )
        .bind(since.timestamp_millis())
        .bind(account_id.map(|id| id.to_string()))
        .bind(account_id.map(|id| id.to_string()))
        .fetch_all(&self.pool)
        .await?;

        let mut points = Vec::with_capacity(rows.len());
        for row in rows {
            let account_id: String = row.try_get("account_id")?;
            let equity: String = row.try_get("equity_usd")?;
            points.push(EquityPoint {
                account_id: Uuid::parse_str(&account_id)?,
                venue: row.try_get("venue")?,
                equity_usd: equity.parse::<Decimal>()?,
                observed_at: from_millis(row.try_get::<i64, _>("observed_at")?),
            });
        }
        Ok(points)
    }

    pub async fn clear_equity_points(&self, account_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("delete from account_equity_points where account_id = ?")
            .bind(account_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn load_strategies(&self) -> Vec<StrategyRecord> {
        self.docs("strategies").await
    }

    pub async fn exchange_venues(&self) -> Vec<ExchangeVenueConfig> {
        let rows = sqlx::query(
            r#"
            select venue, segment, display_name
            from exchange_venues
            where enabled = 1
            order by sort_order asc, venue asc
            "#,
        )
        .fetch_all(&self.pool)
        .await;

        let Ok(rows) = rows else {
            tracing::error!("读取交易所配置失败");
            return Vec::new();
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

    pub async fn tradable_symbols(&self) -> Vec<TradableSymbolConfig> {
        let rows = sqlx::query(
            r#"
            select venue, symbol
            from tradable_symbols
            where enabled = 1
            order by venue asc, sort_order asc, symbol asc
            "#,
        )
        .fetch_all(&self.pool)
        .await;

        let Ok(rows) = rows else {
            tracing::error!("读取交易对配置失败");
            return Vec::new();
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

    pub async fn upsert_tradable_symbols(&self, venue: &str, symbols: &[String]) {
        for symbol in symbols {
            let result = sqlx::query(
                "insert or ignore into tradable_symbols (venue, symbol, sort_order) values (?, ?, 1000)",
            )
            .bind(venue)
            .bind(symbol)
            .execute(&self.pool)
            .await;
            if let Err(error) = result {
                tracing::error!(%error, venue, "写入交易对配置失败");
                return;
            }
        }
    }

    pub async fn notification_settings(&self) -> NotificationSettings {
        self.get_doc("settings", "notifications")
            .await
            .unwrap_or_default()
    }

    pub async fn save_notification_settings(&self, settings: &NotificationSettings) {
        if let Err(error) = self.put_doc("settings", "notifications", settings).await {
            tracing::error!(%error, "写入通知配置失败");
        }
    }

    pub async fn save_strategy(&self, record: &StrategyRecord) {
        if let Err(error) = self
            .put_doc("strategies", &record.id.to_string(), record)
            .await
        {
            tracing::error!(%error, strategy_id = %record.id, "写入策略失败");
        }
    }

    pub async fn save_strategy_version(&self, record: &StrategyRecord) {
        let snapshot = StrategyVersion::from_record(record);
        let id = format!("{}:{}", record.id, record.version);
        if let Err(error) = self.put_doc("strategy_versions", &id, &snapshot).await {
            tracing::error!(%error, strategy_id = %record.id, "写入策略版本失败");
        }
    }

    pub async fn save_strategy_run(&self, run: &StrategyRun) {
        if let Err(error) = self
            .put_doc("strategy_runs", &run.id.to_string(), run)
            .await
        {
            tracing::error!(%error, run_id = %run.id, "写入运行实例失败");
        }
    }

    pub async fn strategy_runs(&self, strategy_id: Uuid, limit: i64) -> Vec<StrategyRun> {
        let mut runs: Vec<StrategyRun> = self
            .docs("strategy_runs")
            .await
            .into_iter()
            .filter(|run: &StrategyRun| run.strategy_id == strategy_id)
            .collect();
        runs.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        runs.truncate(limit.max(1) as usize);
        runs
    }

    pub async fn strategy_run(&self, run_id: Uuid) -> Option<StrategyRun> {
        self.get_doc("strategy_runs", &run_id.to_string()).await
    }

    pub async fn append_strategy_run_event(&self, event: &StrategyRunEvent) {
        if let Err(error) = self
            .put_doc("strategy_run_events", &Uuid::new_v4().to_string(), event)
            .await
        {
            tracing::error!(%error, run_id = %event.run_id, "写入运行事件失败");
        }
    }

    pub async fn strategy_run_events(&self, run_id: Uuid, limit: i64) -> Vec<StrategyRunEvent> {
        let mut events: Vec<StrategyRunEvent> = self
            .docs("strategy_run_events")
            .await
            .into_iter()
            .filter(|event: &StrategyRunEvent| event.run_id == run_id)
            .collect();
        events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        events.truncate(limit.max(1) as usize);
        events.reverse();
        events
    }

    pub async fn strategy_versions(&self, strategy_id: Uuid) -> Vec<StrategyVersion> {
        let mut versions: Vec<StrategyVersion> = self
            .docs("strategy_versions")
            .await
            .into_iter()
            .filter(|version: &StrategyVersion| version.strategy_id == strategy_id)
            .collect();
        versions.sort_by(|left, right| right.version.cmp(&left.version));
        versions
    }

    pub async fn strategy_code(&self, strategy_id: Uuid) -> Option<StrategyCode> {
        self.get_doc("strategy_code", &strategy_id.to_string())
            .await
    }

    pub async fn save_strategy_code(&self, code: &StrategyCode) {
        if let Err(error) = self
            .put_doc("strategy_code", &code.strategy_id.to_string(), code)
            .await
        {
            tracing::error!(%error, strategy_id = %code.strategy_id, "写入策略代码失败");
        }
    }

    pub async fn delete_strategy(&self, strategy_id: Uuid) {
        let _ = self
            .delete_doc("strategies", &strategy_id.to_string())
            .await;
        let _ = self
            .delete_doc("strategy_code", &strategy_id.to_string())
            .await;
        self.delete_docs_for_strategy("strategy_versions", strategy_id)
            .await;
        self.delete_docs_for_strategy("strategy_runs", strategy_id)
            .await;
    }

    pub async fn market_snapshot(&self) -> MarketSnapshot {
        MarketSnapshot {
            authors: self.docs("strategy_authors").await,
            publications: self.docs("strategy_publications").await,
            performance: self.docs("strategy_performance_records").await,
            subscriptions: self.docs("strategy_subscriptions").await,
            settlements: self.docs("strategy_settlements").await,
        }
    }

    pub async fn save_author_card(&self, card: &StrategyAuthorCard) {
        if let Err(error) = self
            .put_doc("strategy_authors", &card.author_id.to_string(), card)
            .await
        {
            tracing::error!(%error, "写入作者卡片失败");
        }
    }

    pub async fn save_publication(&self, publication: &StrategyPublication) {
        if let Err(error) = self
            .put_doc(
                "strategy_publications",
                &publication.id.to_string(),
                publication,
            )
            .await
        {
            tracing::error!(%error, "写入策略发布失败");
        }
    }

    pub async fn save_performance(&self, record: &StrategyPerformanceRecord) {
        if let Err(error) = self
            .put_doc(
                "strategy_performance_records",
                &record.id.to_string(),
                record,
            )
            .await
        {
            tracing::error!(%error, "写入表现记录失败");
        }
    }

    pub async fn save_subscription(&self, subscription: &StrategySubscription) {
        if let Err(error) = self
            .put_doc(
                "strategy_subscriptions",
                &subscription.id.to_string(),
                subscription,
            )
            .await
        {
            tracing::error!(%error, "写入订阅失败");
        }
    }

    pub async fn save_settlement(&self, settlement: &StrategySettlement) {
        let id = format!("{}:{}", settlement.author_id, settlement.period);
        if let Err(error) = self.put_doc("strategy_settlements", &id, settlement).await {
            tracing::error!(%error, "写入结算单失败");
        }
    }

    pub async fn sandbox_artifacts(&self) -> Vec<SandboxArtifact> {
        self.docs("sandbox_artifacts").await
    }

    pub async fn save_sandbox_artifact(
        &self,
        artifact: &SandboxArtifact,
        wasm: &[u8],
    ) -> anyhow::Result<()> {
        self.put_doc("sandbox_artifacts", &artifact.id.to_string(), artifact)
            .await?;
        sqlx::query(
            r#"
            insert into sandbox_modules (id, wasm) values (?, ?)
            on conflict (id) do update set wasm = excluded.wasm
            "#,
        )
        .bind(artifact.id.to_string())
        .bind(wasm)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn sandbox_module(&self, id: Uuid) -> Option<Vec<u8>> {
        sqlx::query_scalar::<_, Vec<u8>>("select wasm from sandbox_modules where id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
    }

    pub async fn strategy_templates(&self) -> Vec<StrategyTemplate> {
        self.docs("strategy_templates").await
    }

    pub async fn save_strategy_template(&self, template: &StrategyTemplate) {
        if let Err(error) = self
            .put_doc("strategy_templates", &template.key, template)
            .await
        {
            tracing::error!(%error, key = %template.key, "写入策略模板失败");
        }
    }

    pub async fn conditional_orders(&self) -> Vec<ConditionalOrder> {
        self.docs("conditional_orders").await
    }

    pub async fn save_conditional_order(&self, order: &ConditionalOrder) {
        let id = order.id;
        if let Err(error) = self
            .put_doc("conditional_orders", &id.to_string(), order)
            .await
        {
            tracing::error!(%error, %id, "写入条件单失败");
        }
    }

    async fn put_doc<T: Serialize>(
        &self,
        collection: &str,
        id: &str,
        value: &T,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(value)?;
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            insert into documents (collection, id, payload, created_at, updated_at)
            values (?, ?, ?, ?, ?)
            on conflict (collection, id) do update
                set payload = excluded.payload,
                    updated_at = excluded.updated_at
            "#,
        )
        .bind(collection)
        .bind(id)
        .bind(payload)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_doc<T: DeserializeOwned>(&self, collection: &str, id: &str) -> Option<T> {
        let payload = sqlx::query_scalar::<_, String>(
            "select payload from documents where collection = ? and id = ?",
        )
        .bind(collection)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;
        serde_json::from_str(&payload).ok()
    }

    async fn docs<T: DeserializeOwned>(&self, collection: &str) -> Vec<T> {
        let rows = sqlx::query(
            "select payload from documents where collection = ? order by created_at asc",
        )
        .bind(collection)
        .fetch_all(&self.pool)
        .await;

        let Ok(rows) = rows else {
            tracing::error!(collection, "读取本地数据失败");
            return Vec::new();
        };
        rows.iter()
            .filter_map(|row| {
                let payload: String = row.try_get("payload").ok()?;
                serde_json::from_str(&payload).ok()
            })
            .collect()
    }

    async fn delete_doc(&self, collection: &str, id: &str) -> anyhow::Result<()> {
        sqlx::query("delete from documents where collection = ? and id = ?")
            .bind(collection)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 删除 payload 中 `strategy_id` 匹配的文档；本地数据量很小，直接过滤即可。
    async fn delete_docs_for_strategy(&self, collection: &str, strategy_id: Uuid) {
        let rows = match sqlx::query("select id, payload from documents where collection = ?")
            .bind(collection)
            .fetch_all(&self.pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, collection, "读取本地数据失败");
                return;
            }
        };

        for row in rows {
            let payload: String = match row.try_get("payload") {
                Ok(payload) => payload,
                Err(_) => continue,
            };
            let matches = serde_json::from_str::<serde_json::Value>(&payload)
                .ok()
                .and_then(|value| {
                    value
                        .get("strategy_id")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                })
                .map(|id| id == strategy_id.to_string())
                .unwrap_or(false);
            if matches {
                let id: String = match row.try_get("id") {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let _ = self.delete_doc(collection, &id).await;
            }
        }
    }
}

async fn seed_market_config(pool: &SqlitePool) -> anyhow::Result<()> {
    for (venue, segment, display_name, sort_order) in SEED_VENUES {
        sqlx::query(
            r#"
            insert or ignore into exchange_venues
                (venue, segment, display_name, enabled, sort_order)
            values (?, ?, ?, 1, ?)
            "#,
        )
        .bind(venue)
        .bind(segment)
        .bind(display_name)
        .bind(sort_order)
        .execute(pool)
        .await?;
    }

    for (venue_index, venue) in SEED_UNIVERSE.iter().enumerate() {
        for (symbol_index, symbol) in SEED_SYMBOLS.iter().enumerate() {
            sqlx::query(
                r#"
                insert or ignore into tradable_symbols (venue, symbol, enabled, sort_order)
                values (?, ?, 1, ?)
                "#,
            )
            .bind(venue)
            .bind(symbol)
            .bind(((venue_index + 1) * 1000 + symbol_index) as i64)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

fn decode_funding_row(row: &sqlx::sqlite::SqliteRow, venue: &str) -> anyhow::Result<FundingRate> {
    let rate: String = row.try_get("funding_rate")?;
    let mark_price: Option<String> = row.try_get("mark_price")?;
    Ok(FundingRate {
        venue: venue.to_string(),
        symbol: row.try_get("symbol")?,
        funding_time: from_millis(row.try_get::<i64, _>("funding_time")?),
        funding_rate: rate.parse::<Decimal>()?,
        mark_price: mark_price.and_then(|price| price.parse().ok()),
    })
}

fn from_millis(value: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(value)
        .single()
        .unwrap_or_else(Utc::now)
}
