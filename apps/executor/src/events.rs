use arb_core::{Order, OrderStatus, Position};
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
}

/// 消息通知类型：新闻推送与资金费率套利提醒。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    News,
    Funding,
}

/// 通过 WebSocket 推送给桌面端的事件，同时用于审计落库。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutorEvent {
    /// 按通知设置触发的消息提醒，桌面端据此弹应用内通知。
    Notice {
        kind: NoticeKind,
        title: String,
        message: String,
        at: DateTime<Utc>,
    },
    OpportunityUpdated {
        count: usize,
        observed_at: DateTime<Utc>,
    },
    OrderUpdated {
        order: Order,
    },
    /// 仓位价格随 WebSocket 行情更新后的完整快照，桌面端据此即时刷新。
    PositionsUpdated {
        positions: Vec<Position>,
        at: DateTime<Utc>,
    },
    ExecutorStatus {
        paused: bool,
        uptime_secs: u64,
        server_time: DateTime<Utc>,
    },
    Alert {
        level: AlertLevel,
        message: String,
        /// 英文版本，供英文界面的桌面端与 Telegram 使用；旧客户端会忽略该字段。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_en: Option<String>,
        at: DateTime<Utc>,
    },
    /// 行情侧发现极端异常（如资金费率瞬间跳变），请求执行器停摆开仓。
    RiskHalt {
        reason: String,
        reason_en: String,
        at: DateTime<Utc>,
    },
}

impl ExecutorEvent {
    pub fn order_transition(
        order: &Order,
        message: impl Into<String>,
        message_en: impl Into<String>,
    ) -> Self {
        Self::Alert {
            level: if matches!(order.status, OrderStatus::Failed) {
                AlertLevel::Critical
            } else {
                AlertLevel::Info
            },
            message: message.into(),
            message_en: Some(message_en.into()),
            at: Utc::now(),
        }
    }
}
