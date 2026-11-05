use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::{config::Config, http_client::HttpClient, notifier::Notifier};

/// 每个来源最多保留的条目，超出后丢弃最旧的。
const MAX_ITEMS: usize = 400;

pub const BINANCE: &str = "binance";
pub const UPBIT: &str = "upbit";
pub const OKX: &str = "okx";
pub const BYBIT: &str = "bybit";
pub const BITGET: &str = "bitget";

/// 全部新闻来源，供通知设置的默认值使用。
pub const SOURCES: [&str; 5] = [BINANCE, UPBIT, OKX, BYBIT, BITGET];

/// 一条上币公告，供桌面端「新闻订阅」直接展示。
#[derive(Debug, Clone, Serialize)]
pub struct NewsItem {
    pub id: String,
    pub source: String,
    pub title: String,
    pub url: Option<String>,
    pub published_at: DateTime<Utc>,
}

/// 单个来源的采集状态。
#[derive(Debug, Clone, Serialize)]
pub struct NewsSourceStatus {
    pub source: String,
    pub enabled: bool,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsResponse {
    pub items: Vec<NewsItem>,
    pub sources: Vec<NewsSourceStatus>,
}

struct SourceRuntime {
    id: &'static str,
    enabled: bool,
    base_url: String,
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

/// 上币公告采集器：定时拉取各交易所公告，去重后保存在内存中。
pub struct NewsFeed {
    http: Option<Arc<HttpClient>>,
    sources: RwLock<Vec<SourceRuntime>>,
    items: RwLock<Vec<NewsItem>>,
    seen: Mutex<HashSet<String>>,
    poll: Duration,
    /// 新抓取到的公告按通知设置推送；未挂载时只入库不推送。
    notifier: RwLock<Option<Arc<Notifier>>>,
    /// 首轮采集只建立去重基线，避免重启后把历史公告全部推送一遍。
    primed: AtomicBool,
}

impl NewsFeed {
    pub fn start(config: &Config, http: Option<Arc<HttpClient>>) -> Arc<Self> {
        let sources = vec![
            SourceRuntime {
                id: BINANCE,
                enabled: config.news_enabled,
                base_url: config.news_binance_url.clone(),
                last_success_at: None,
                last_error: None,
            },
            SourceRuntime {
                id: UPBIT,
                enabled: config.news_enabled,
                base_url: config.news_upbit_url.clone(),
                last_success_at: None,
                last_error: None,
            },
            SourceRuntime {
                id: OKX,
                enabled: config.news_enabled,
                base_url: config.news_okx_url.clone(),
                last_success_at: None,
                last_error: None,
            },
            SourceRuntime {
                id: BYBIT,
                enabled: config.news_enabled,
                base_url: config.news_bybit_url.clone(),
                last_success_at: None,
                last_error: None,
            },
            SourceRuntime {
                id: BITGET,
                enabled: config.news_enabled,
                base_url: config.news_bitget_url.clone(),
                last_success_at: None,
                last_error: None,
            },
        ];
        let feed = Arc::new(Self {
            http,
            sources: RwLock::new(sources),
            items: RwLock::new(Vec::new()),
            seen: Mutex::new(HashSet::new()),
            poll: Duration::from_secs(config.news_poll_secs.max(30)),
            notifier: RwLock::new(None),
            primed: AtomicBool::new(false),
        });

        if feed.http.is_none() {
            tracing::warn!("缺少共享 HTTP 客户端，上币新闻采集不可用");
            return feed;
        }

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(worker.poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                worker.sync_once().await;
            }
        });
        feed
    }

    async fn sync_once(&self) {
        // 首轮只建立去重基线：重启后缓存为空，全部公告都算「新增」，此时推送会刷屏。
        let first_round = !self.primed.swap(true, Ordering::Relaxed);
        let targets: Vec<(&'static str, String)> = self
            .sources
            .read()
            .expect("新闻来源锁不可用")
            .iter()
            .filter(|source| source.enabled)
            .map(|source| (source.id, source.base_url.clone()))
            .collect();

        for (id, base_url) in targets {
            let Some(http) = self.http.clone() else {
                return;
            };
            let result =
                tokio::task::spawn_blocking(move || http.get_json(&base_url)).await;
            match result {
                Ok(Ok(payload)) => match self.parse(id, payload) {
                    Ok(parsed) => {
                        let fetched = parsed.len();
                        let added = self.merge(parsed);
                        self.mark(id, true, None);
                        tracing::info!(source = id, count = fetched, "上币新闻已更新");
                        let notifier = self
                            .notifier
                            .read()
                            .expect("新闻通知锁不可用")
                            .clone();
                        if let Some(notifier) = notifier {
                            if !first_round && !added.is_empty() {
                                // 推送走后台任务，避免 Telegram 请求拖慢采集节奏。
                                tokio::spawn(async move { notifier.notify_news(&added).await });
                            }
                        }
                    }
                    Err(error) => self.mark(id, false, Some(error.to_string())),
                },
                Ok(Err(error)) => self.mark(id, false, Some(error.to_string())),
                Err(error) => self.mark(id, false, Some(error.to_string())),
            }
        }
    }

    fn parse(&self, source: &str, payload: Value) -> anyhow::Result<Vec<NewsItem>> {
        match source {
            BINANCE => parse_binance(&payload),
            UPBIT => parse_upbit(&payload),
            OKX => parse_okx(&payload),
            BYBIT => parse_bybit(&payload),
            BITGET => parse_bitget(&payload),
            other => anyhow::bail!("未知的新闻来源: {other}"),
        }
    }

    /// 绑定通知分发器后，新增公告会按通知设置推送。
    pub fn attach_notifier(&self, notifier: Arc<Notifier>) {
        *self.notifier.write().expect("新闻通知锁不可用") = Some(notifier);
    }

    /// 去重后按发布时间倒序写入缓存，返回本次真正新增的条目。
    fn merge(&self, parsed: Vec<NewsItem>) -> Vec<NewsItem> {
        if parsed.is_empty() {
            return Vec::new();
        }
        let mut seen = self.seen.lock().expect("新闻去重锁不可用");
        let mut items = self.items.write().expect("新闻缓存锁不可用");
        let mut added = Vec::new();
        for item in parsed {
            if seen.insert(item.id.clone()) {
                added.push(item.clone());
                items.push(item);
            }
        }
        items.sort_by(|left, right| right.published_at.cmp(&left.published_at));
        items.truncate(MAX_ITEMS);
        added
    }

    fn mark(&self, source: &str, ok: bool, error: Option<String>) {
        let mut sources = self.sources.write().expect("新闻来源锁不可用");
        if let Some(entry) = sources.iter_mut().find(|item| item.id == source) {
            if ok {
                entry.last_success_at = Some(Utc::now());
                entry.last_error = None;
            } else {
                entry.last_error = error;
            }
        }
    }

    /// 按发布时间倒序返回缓存条目与各来源状态。
    pub fn snapshot(&self, limit: usize) -> NewsResponse {
        let items = self.items.read().expect("新闻缓存锁不可用");
        let mut counts = std::collections::HashMap::new();
        for item in items.iter() {
            *counts.entry(item.source.clone()).or_insert(0usize) += 1;
        }
        let sources = self
            .sources
            .read()
            .expect("新闻来源锁不可用")
            .iter()
            .map(|source| NewsSourceStatus {
                source: source.id.to_string(),
                enabled: source.enabled,
                last_success_at: source.last_success_at,
                last_error: source.last_error.clone(),
                count: counts.get(source.id).copied().unwrap_or(0),
            })
            .collect();

        NewsResponse {
            items: items.iter().take(limit).cloned().collect(),
            sources,
        }
    }
}

fn parse_binance(payload: &Value) -> anyhow::Result<Vec<NewsItem>> {
    let catalogs = payload
        .pointer("/data/catalogs")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Binance 公告响应缺少 data.catalogs"))?;

    let mut items = Vec::new();
    for catalog in catalogs {
        let Some(articles) = catalog.get("articles").and_then(Value::as_array) else {
            continue;
        };
        for article in articles {
            let Some(id) = article.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let Some(title) = article.get("title").and_then(Value::as_str) else {
                continue;
            };
            let Some(at) = article.get("releaseDate").and_then(Value::as_i64) else {
                continue;
            };
            let Some(published_at) = DateTime::from_timestamp_millis(at) else {
                continue;
            };
            let url = article
                .get("code")
                .and_then(Value::as_str)
                .map(|code| format!("https://www.binance.com/en/support/announcement/{code}"));
            items.push(NewsItem {
                id: format!("{BINANCE}:{id}"),
                source: BINANCE.to_string(),
                title: title.to_string(),
                url,
                published_at,
            });
        }
    }
    Ok(items)
}

fn parse_upbit(payload: &Value) -> anyhow::Result<Vec<NewsItem>> {
    let notices = payload
        .pointer("/data/notices")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Upbit 公告响应缺少 data.notices"))?;

    let mut items = Vec::new();
    for notice in notices {
        let Some(id) = notice.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let Some(title) = notice.get("title").and_then(Value::as_str) else {
            continue;
        };
        let Some(at) = notice
            .get("listed_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        else {
            continue;
        };
        items.push(NewsItem {
            id: format!("{UPBIT}:{id}"),
            source: UPBIT.to_string(),
            title: title.to_string(),
            url: Some(format!("https://upbit.com/service_center/notice?id={id}")),
            published_at: at.with_timezone(&Utc),
        });
    }
    Ok(items)
}

/// 毫秒时间戳，兼容数字与字符串两种写法。
fn parse_millis(value: &Value) -> Option<DateTime<Utc>> {
    let raw = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.parse().ok()?,
        _ => return None,
    };
    DateTime::from_timestamp_millis(raw)
}

fn parse_okx(payload: &Value) -> anyhow::Result<Vec<NewsItem>> {
    let details = payload
        .pointer("/data/0/details")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("OKX 公告响应缺少 data[0].details"))?;

    let mut items = Vec::new();
    for detail in details {
        let Some(title) = detail.get("title").and_then(Value::as_str) else {
            continue;
        };
        let Some(url) = detail.get("url").and_then(Value::as_str) else {
            continue;
        };
        let Some(published_at) = detail.get("pTime").and_then(parse_millis) else {
            continue;
        };
        items.push(NewsItem {
            id: format!(
                "{OKX}:{}",
                url.trim_end_matches('/').rsplit('/').next().unwrap_or(url)
            ),
            source: OKX.to_string(),
            title: title.to_string(),
            url: Some(url.to_string()),
            published_at,
        });
    }
    Ok(items)
}

fn parse_bybit(payload: &Value) -> anyhow::Result<Vec<NewsItem>> {
    let list = payload
        .pointer("/result/list")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Bybit 公告响应缺少 result.list"))?;

    let mut items = Vec::new();
    for entry in list {
        let Some(title) = entry.get("title").and_then(Value::as_str) else {
            continue;
        };
        let Some(url) = entry.get("url").and_then(Value::as_str) else {
            continue;
        };
        let Some(published_at) = entry
            .get("dateTimestamp")
            .or_else(|| entry.get("publishTime"))
            .and_then(parse_millis)
        else {
            continue;
        };
        items.push(NewsItem {
            id: format!(
                "{BYBIT}:{}",
                url.trim_end_matches('/').rsplit('/').next().unwrap_or(url)
            ),
            source: BYBIT.to_string(),
            title: title.to_string(),
            url: Some(url.to_string()),
            published_at,
        });
    }
    Ok(items)
}

fn parse_bitget(payload: &Value) -> anyhow::Result<Vec<NewsItem>> {
    let articles = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Bitget 公告响应缺少 data"))?;

    let mut items = Vec::new();
    for article in articles {
        let Some(id) = article.get("annId").and_then(Value::as_str) else {
            continue;
        };
        let Some(title) = article.get("annTitle").and_then(Value::as_str) else {
            continue;
        };
        let Some(published_at) = article.get("cTime").and_then(parse_millis) else {
            continue;
        };
        let url = article
            .get("annUrl")
            .and_then(Value::as_str)
            .map(str::to_string);
        items.push(NewsItem {
            id: format!("{BITGET}:{id}"),
            source: BITGET.to_string(),
            title: title.to_string(),
            url,
            published_at,
        });
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_binance_articles() {
        let payload = serde_json::json!({
            "code": "000000",
            "data": {
                "catalogs": [{
                    "catalogId": 48,
                    "articles": [{
                        "id": 288627,
                        "code": "a259777",
                        "title": "Binance Will List Predict (PREDICT)",
                        "releaseDate": 1_793_274_532_329i64
                    }]
                }]
            }
        });
        let items = parse_binance(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "binance:288627");
        assert_eq!(items[0].source, "binance");
        assert!(items[0].url.as_deref().unwrap().ends_with("a259777"));
    }

    #[test]
    fn parses_upbit_notices() {
        let payload = serde_json::json!({
            "success": true,
            "data": {
                "notices": [{
                    "id": 6714,
                    "title": "닐리온(NIL) KRW 마켓 디지털 자산 추가",
                    "listed_at": "2026-10-27T16:30:00+09:00"
                }]
            }
        });
        let items = parse_upbit(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "upbit:6714");
        assert_eq!(
            items[0].published_at,
            Utc.with_ymd_and_hms(2026, 10, 27, 7, 30, 0).unwrap()
        );
    }

    #[test]
    fn parses_okx_announcements() {
        let payload = serde_json::json!({
            "code": "0",
            "data": [{
                "details": [{
                    "annType": "announcements-new-listings",
                    "title": "OKX to list TRUMP for spot trading",
                    "url": "https://www.okx.com/help/okx-to-list-trump",
                    "pTime": "1737255602487"
                }]
            }]
        });
        let items = parse_okx(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "okx:okx-to-list-trump");
        assert_eq!(
            items[0].published_at,
            DateTime::from_timestamp_millis(1_737_255_602_487).unwrap()
        );
    }

    #[test]
    fn parses_bybit_announcements() {
        let payload = serde_json::json!({
            "retCode": 0,
            "result": {
                "list": [{
                    "title": "Bybit to List KOR Protocol (KOR) on Spot",
                    "url": "https://announcements.bybit.com/en-US/article/bybit-to-list-kor--art1951638e067c/",
                    "dateTimestamp": 1_793_779_622_000i64,
                    "publishTime": 1_793_780_409_000i64
                }]
            }
        });
        let items = parse_bybit(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "bybit:bybit-to-list-kor--art1951638e067c");
        assert_eq!(items[0].source, "bybit");
    }

    #[test]
    fn parses_bitget_announcements() {
        let payload = serde_json::json!({
            "code": "00000",
            "data": [{
                "annId": "12560603899630",
                "annTitle": "Bitget Lists FISVUSDT Stock Perp",
                "annUrl": "https://www.bitget.com/en/support/articles/12560603899630",
                "cTime": "1793803260000"
            }]
        });
        let items = parse_bitget(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "bitget:12560603899630");
        assert_eq!(items[0].url.as_deref(), Some("https://www.bitget.com/en/support/articles/12560603899630"));
    }

    #[test]
    fn keeps_sorted_and_deduped_items() {
        let feed = NewsFeed {
            http: None,
            sources: RwLock::new(Vec::new()),
            items: RwLock::new(Vec::new()),
            seen: Mutex::new(HashSet::new()),
            poll: Duration::from_secs(60),
            notifier: RwLock::new(None),
            primed: AtomicBool::new(false),
        };
        let older = NewsItem {
            id: "upbit:1".into(),
            source: UPBIT.into(),
            title: "old".into(),
            url: None,
            published_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
        };
        let newer = NewsItem {
            id: "binance:2".into(),
            source: BINANCE.into(),
            title: "new".into(),
            url: None,
            published_at: Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap(),
        };
        feed.merge(vec![older.clone(), newer]);
        feed.merge(vec![older]);
        let snapshot = feed.snapshot(10);
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].id, "binance:2");
    }
}
