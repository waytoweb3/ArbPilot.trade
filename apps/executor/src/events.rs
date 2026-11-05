use arb_core::{Order, OrderStatus};
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
    ExecutorStatus {
        paused: bool,
        uptime_secs: u64,
        server_time: DateTime<Utc>,
    },
    Alert {
        level: AlertLevel,
        message: String,
        at: DateTime<Utc>,
    },
}

impl ExecutorEvent {
    pub fn order_transition(order: &Order, message: impl Into<String>) -> Self {
        Self::Alert {
            level: if matches!(order.status, OrderStatus::Failed) {
                AlertLevel::Critical
            } else {
                AlertLevel::Info
            },
            message: message.into(),
            at: Utc::now(),
        }
    }
}
