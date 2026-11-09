//! 消息通知分发：新闻推送与资金费率套利提醒。
//!
//! 通知条件全部来自 `notification_settings` 表：开关、来源、年化阈值与冷却时间。
//! 命中后按渠道设置分别推送到 Telegram 与桌面端（WebSocket 事件）。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use arb_core::{Opportunity, OpportunityKind};
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use crate::{
    events::{ExecutorEvent, NoticeKind},
    news::NewsItem,
    store::{NotificationSettings, Store},
    telegram::{TelegramBot, TelegramMessage},
};

/// 每轮新闻最多推送的条目数，避免首次采集时刷屏。
const MAX_NEWS_NOTICES: usize = 5;
/// 每轮资金费率最多推送的机会数。
const MAX_FUNDING_NOTICES: usize = 5;

/// 通知分发器：读取配置、判断条件，并按渠道发送。
pub struct Notifier {
    store: Arc<Store>,
    events: broadcast::Sender<ExecutorEvent>,
    telegram: Arc<TelegramBot>,
    /// 交易对 → 上次提醒时间，用于按冷却时间抑制重复提醒。
    cooldowns: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl Notifier {
    pub fn new(
        store: Arc<Store>,
        events: broadcast::Sender<ExecutorEvent>,
        telegram: Arc<TelegramBot>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            events,
            telegram,
            cooldowns: Mutex::new(HashMap::new()),
        })
    }

    /// 新闻推送：按来源过滤后逐条发送。
    pub async fn notify_news(&self, items: &[NewsItem]) {
        if items.is_empty() {
            return;
        }
        let settings = self.store.notification_settings().await;
        if !settings.news_enabled {
            return;
        }
        for item in items.iter().take(MAX_NEWS_NOTICES) {
            if !source_allowed(&settings, &item.source) {
                continue;
            }
            let mut message = format!("上币公告 · {}\n{}", item.source, item.title);
            let mut message_en = format!("Listing · {}\n{}", item.source, item.title);
            if let Some(url) = &item.url {
                message.push('\n');
                message.push_str(url);
                message_en.push('\n');
                message_en.push_str(url);
            }
            self.deliver(
                &settings,
                NoticeKind::News,
                item.title.clone(),
                message,
                message_en,
            )
            .await;
        }
    }

    /// 资金费率提醒：按年化阈值与冷却时间过滤后发送。
    pub async fn notify_funding(&self, opportunities: &[Opportunity]) {
        if opportunities.is_empty() {
            return;
        }
        let settings = self.store.notification_settings().await;
        if !settings.funding_enabled {
            return;
        }
        let cooldown = chrono::Duration::seconds(settings.funding_cooldown_secs.max(0));
        let now = Utc::now();
        let mut sent = 0usize;
        for opportunity in opportunities {
            if sent >= MAX_FUNDING_NOTICES {
                break;
            }
            // 各所结算间隔不同（Hyperliquid 1h、多数 CEX 8h），统一按机会里的年化值判断。
            let Some(annualized) = opportunity.funding_annualized_pct else {
                continue;
            };
            if annualized < settings.funding_min_annualized_pct {
                continue;
            }
            // 同一交易对可能同时有现货-永续与永续-永续两类机会，冷却按类型区分。
            let cooldown_key = format!("{}:{:?}", opportunity.symbol, opportunity.kind);
            {
                let mut cooldowns = self.cooldowns.lock().expect("通知冷却锁不可用");
                if let Some(last) = cooldowns.get(&cooldown_key) {
                    if now.signed_duration_since(*last) < cooldown {
                        continue;
                    }
                }
                cooldowns.insert(cooldown_key, now);
            }

            let (headline, headline_en, title) = match opportunity.kind {
                OpportunityKind::PerpPerpFunding => (
                    "永续-永续资金费率差",
                    "Perp-perp funding spread",
                    format!("{} 跨所资金费率差", opportunity.symbol),
                ),
                _ => (
                    "资金费率套利机会",
                    "Funding-rate arbitrage",
                    format!("{} 资金费率套利", opportunity.symbol),
                ),
            };
            let message = format!(
                "{headline}\n{}：{} 多 / {} 空\n年化 {}%，单期净收益 {} bps\n可成交额度 {} USD",
                opportunity.symbol,
                opportunity.long_venue,
                opportunity.short_venue,
                annualized.round_dp(2),
                opportunity.net_edge_bps,
                opportunity.max_notional_usd,
            );
            let message_en = format!(
                "{headline_en}\n{}: long {} / short {}\nAnnualised {}%, net per period {} bps\nTradable notional {} USD",
                opportunity.symbol,
                opportunity.long_venue,
                opportunity.short_venue,
                annualized.round_dp(2),
                opportunity.net_edge_bps,
                opportunity.max_notional_usd,
            );
            self.deliver(
                &settings,
                NoticeKind::Funding,
                title,
                message,
                message_en,
            )
            .await;
            sent += 1;
        }
    }

    /// 按渠道开关推送：桌面端事件 + Telegram 消息。
    async fn deliver(
        &self,
        settings: &NotificationSettings,
        kind: NoticeKind,
        title: String,
        message: String,
        message_en: String,
    ) {
        if settings.desktop_enabled {
            let _ = self.events.send(ExecutorEvent::Notice {
                kind,
                title,
                message: message.clone(),
                at: Utc::now(),
            });
        }
        if settings.telegram_enabled && self.telegram.enabled() {
            // 每个绑定账号收到自己语言的版本。
            self.telegram
                .broadcast(&TelegramMessage::new(message, message_en))
                .await;
        }
    }
}

/// 新闻来源过滤：空列表表示全部来源。
fn source_allowed(settings: &NotificationSettings, source: &str) -> bool {
    settings.news_sources.is_empty() || settings.news_sources.iter().any(|item| item == source)
}
