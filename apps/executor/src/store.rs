use arb_core::Order;
use sqlx::{postgres::PgPoolOptions, PgPool};

/// 业务状态存储。
///
/// - 未配置 DATABASE_URL 或连接失败时降级为内存模式，执行器仍可运行（便于本地开发）。
/// - 只会写入订单、风控告警等业务数据，不保存任何交易所密钥或签名材料。
pub struct Store {
    pool: Option<PgPool>,
}

impl Store {
    pub async fn connect_if_configured(database_url: Option<&str>) -> Self {
        let Some(url) = database_url else {
            tracing::info!("未配置 DATABASE_URL，执行器以内存模式运行");
            return Self { pool: None };
        };

        let pool = match PgPoolOptions::new().max_connections(8).connect(url).await {
            Ok(pool) => pool,
            Err(error) => {
                tracing::warn!(%error, "连接 PostgreSQL 失败，执行器以内存模式运行");
                return Self { pool: None };
            }
        };

        match sqlx::migrate!("./migrations").run(&pool).await {
            Ok(_) => {
                tracing::info!("PostgreSQL 已连接并完成迁移");
                Self { pool: Some(pool) }
            }
            Err(error) => {
                tracing::error!(%error, "数据库迁移失败，执行器以内存模式运行");
                Self { pool: None }
            }
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
}
