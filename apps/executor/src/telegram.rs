use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{bail, Context};
use arb_core::OrderOrigin;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    config::Config,
    error::ApiError,
    events::ExecutorEvent,
    gateway::{self, PairOrderRequest},
    http_client::HttpClient,
    state::AppState,
};

/// 待确认订单的有效期。
const PENDING_TTL_SECS: i64 = 120;
/// 单个绑定用户最多保留的待确认订单数。
const MAX_PENDING: usize = 5;

/// Telegram 绑定关系，保存在执行器本机（若执行器跑在用户服务器上，就存在那台服务器）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramBinding {
    pub telegram_user_id: i64,
    pub chat_id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub paired_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelegramStatus {
    /// 是否配置了 Bot Token。
    pub enabled: bool,
    /// 是否正在轮询 Telegram。
    pub polling: bool,
    pub bot_username: Option<String>,
    pub last_error: Option<String>,
    pub bindings: Vec<TelegramBinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

struct ActivePairing {
    code: String,
    expires_at: DateTime<Utc>,
}

struct PendingTrade {
    owner: i64,
    symbol: String,
    long_venue: String,
    short_venue: String,
    notional_usd: Decimal,
    leverage: Decimal,
    expires_at: DateTime<Utc>,
}

/// Telegram 机器人：配对绑定、查询、暂停/恢复与经确认的双腿开仓。
pub struct TelegramBot {
    http: Option<Arc<HttpClient>>,
    api_base: String,
    token: Option<String>,
    bindings_file: PathBuf,
    bindings: RwLock<Vec<TelegramBinding>>,
    pairing: Mutex<Option<ActivePairing>>,
    pending: Mutex<HashMap<String, PendingTrade>>,
    offset: Mutex<i64>,
    bot_username: RwLock<Option<String>>,
    last_error: RwLock<Option<String>>,
    poll: Duration,
    pairing_ttl: Duration,
}

impl TelegramBot {
    pub fn start(config: &Config, http: Option<Arc<HttpClient>>) -> Arc<Self> {
        let bindings = load_bindings(&config.telegram_bindings_file);
        Arc::new(Self {
            http,
            api_base: config.telegram_api_base.clone(),
            token: config
                .telegram_bot_token
                .clone()
                .filter(|_| config.telegram_enabled),
            bindings_file: config.telegram_bindings_file.clone(),
            bindings: RwLock::new(bindings),
            pairing: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            offset: Mutex::new(0),
            bot_username: RwLock::new(None),
            last_error: RwLock::new(None),
            poll: Duration::from_secs(config.telegram_poll_secs.max(2)),
            pairing_ttl: Duration::from_secs(config.telegram_pairing_secs.max(60)),
        })
    }

    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    /// 绑定到执行器状态后开始轮询，并把执行器告警推送给已绑定的账号。
    pub fn attach(self: &Arc<Self>, state: AppState) {
        if !self.enabled() {
            tracing::info!("未配置 ARBPILOT_TELEGRAM_BOT_TOKEN，电报入口未启用");
            return;
        }

        // 先订阅事件再移动 state，避免轮询任务持有时无法再访问事件总线。
        let mut events = state.events.subscribe();
        let bot = Arc::clone(self);
        let poll_state = state.clone();
        tokio::spawn(async move {
            bot.refresh_identity().await;
            loop {
                match bot.poll_once(&poll_state).await {
                    Ok(()) => tokio::time::sleep(bot.poll).await,
                    Err(error) => {
                        *bot.last_error.write().expect("电报错误锁不可用") = Some(error.to_string());
                        tracing::warn!(%error, "Telegram 轮询失败");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        let bot = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ExecutorEvent::Alert { message, .. }) => bot.broadcast(&message).await,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    pub fn status(&self) -> TelegramStatus {
        TelegramStatus {
            enabled: self.enabled(),
            polling: self.enabled() && self.last_error.read().expect("电报错误锁不可用").is_none(),
            bot_username: self
                .bot_username
                .read()
                .expect("电报身份锁不可用")
                .clone(),
            last_error: self
                .last_error
                .read()
                .expect("电报错误锁不可用")
                .clone(),
            bindings: self.bindings.read().expect("电报绑定锁不可用").clone(),
        }
    }

    /// 桌面端生成一次性配对码，用户在 Telegram 里发送 `/bind <code>` 完成绑定。
    pub fn create_pairing(&self) -> Result<PairingCode, ApiError> {
        if !self.enabled() {
            return Err(ApiError::Conflict(
                "未配置 ARBPILOT_TELEGRAM_BOT_TOKEN，电报入口未启用".to_string(),
            ));
        }
        let code = Uuid::new_v4().simple().to_string()[..8].to_ascii_uppercase();
        let expires_at = Utc::now() + chrono::Duration::seconds(self.pairing_ttl.as_secs() as i64);
        *self.pairing.lock().expect("配对码锁不可用") = Some(ActivePairing {
            code: code.clone(),
            expires_at,
        });
        Ok(PairingCode { code, expires_at })
    }

    pub fn revoke(&self, user_id: i64) -> Result<(), ApiError> {
        let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
        let before = bindings.len();
        bindings.retain(|binding| binding.telegram_user_id != user_id);
        if before == bindings.len() {
            return Err(ApiError::NotFound("该 Telegram 账号未绑定".to_string()));
        }
        drop(bindings);
        self.persist().map_err(ApiError::from)
    }

    async fn refresh_identity(&self) {
        match self.api("getMe", &[]).await {
            Ok(value) => {
                let username = value
                    .pointer("/result/username")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if username.is_some() {
                    *self.bot_username.write().expect("电报身份锁不可用") = username;
                }
            }
            Err(error) => {
                *self.last_error.write().expect("电报错误锁不可用") = Some(error.to_string());
            }
        }
    }

    async fn poll_once(self: &Arc<Self>, state: &AppState) -> anyhow::Result<()> {
        let offset = *self.offset.lock().expect("轮询游标锁不可用");
        let value = self
            .api(
                "getUpdates",
                &[("offset", offset.to_string()), ("timeout", "0".to_string())],
            )
            .await?;
        *self.last_error.write().expect("电报错误锁不可用") = None;

        for update in value["result"].as_array().cloned().unwrap_or_default() {
            if let Some(update_id) = update.get("update_id").and_then(Value::as_i64) {
                *self.offset.lock().expect("轮询游标锁不可用") = update_id + 1;
            }
            if let Some(message) = update.get("message") {
                self.handle_message(state, message).await;
            }
        }
        Ok(())
    }

    async fn handle_message(&self, state: &AppState, message: &Value) {
        let Some(chat_id) = message.pointer("/chat/id").and_then(Value::as_i64) else {
            return;
        };
        let user_id = message
            .pointer("/from/id")
            .and_then(Value::as_i64)
            .unwrap_or(chat_id);
        let username = message
            .pointer("/from/username")
            .and_then(Value::as_str)
            .map(str::to_string);
        let text = message.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() {
            return;
        }

        let mut parts = text.split_whitespace();
        let command = parts
            .next()
            .unwrap_or_default()
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let args: Vec<&str> = parts.collect();

        let reply = match command.as_str() {
            "/start" | "/help" => help_text(self.enabled()),
            "/bind" => self.bind(chat_id, user_id, username, &args),
            _ => {
                if self.binding(user_id).is_none() {
                    "尚未绑定。请在桌面端「设置 → Telegram」生成配对码，然后发送 /bind <配对码>。".to_string()
                } else {
                    match command.as_str() {
                        "/status" => status_text(state),
                        "/positions" => positions_text(state),
                        "/orders" => orders_text(state),
                        "/news" => news_text(state),
                        "/pause" => {
                            state.set_paused(true, Some("Telegram 指令"));
                            "已暂停开仓。".to_string()
                        }
                        "/resume" => {
                            state.set_paused(false, None);
                            "已恢复开仓。".to_string()
                        }
                        "/open" => self.prepare_open(state, user_id, &args),
                        "/confirm" => self.confirm(state, user_id, &args).await,
                        "/cancel" => self.cancel(user_id, &args),
                        "/unbind" => self.unbind(user_id),
                        other => format!("未识别的指令 {other}\n\n{}", help_text(self.enabled())),
                    }
                }
            }
        };
        self.send(chat_id, &reply).await;
    }

    fn bind(
        &self,
        chat_id: i64,
        user_id: i64,
        username: Option<String>,
        args: &[&str],
    ) -> String {
        if self.binding(user_id).is_some() {
            return "该账号已绑定，如需换绑请先发送 /unbind。".to_string();
        }
        let Some(code) = args.first() else {
            return "用法：/bind <配对码>".to_string();
        };

        let matched = {
            let pairing = self.pairing.lock().expect("配对码锁不可用");
            matches!(
                pairing.as_ref(),
                Some(active) if active.code.eq_ignore_ascii_case(code) && active.expires_at > Utc::now()
            )
        };
        if !matched {
            return "配对码不正确或已过期，请在桌面端重新生成。".to_string();
        }

        *self.pairing.lock().expect("配对码锁不可用") = None;
        {
            let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
            bindings.push(TelegramBinding {
                telegram_user_id: user_id,
                chat_id,
                username,
                permissions: vec!["read".to_string(), "trade".to_string()],
                paired_at: Utc::now(),
            });
        }
        match self.persist() {
            Ok(()) => "绑定成功。发送 /help 查看可用指令，开仓需要二次确认。".to_string(),
            Err(error) => format!("绑定已记录，但写入绑定文件失败：{error}"),
        }
    }

    fn unbind(&self, user_id: i64) -> String {
        let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
        let before = bindings.len();
        bindings.retain(|binding| binding.telegram_user_id != user_id);
        if before == bindings.len() {
            return "该账号尚未绑定。".to_string();
        }
        drop(bindings);
        match self.persist() {
            Ok(()) => "已解除绑定，后续指令将不再响应。".to_string(),
            Err(error) => format!("已解除绑定，但写入绑定文件失败：{error}"),
        }
    }

    fn prepare_open(&self, state: &AppState, user_id: i64, args: &[&str]) -> String {
        if !self.can_trade(user_id) {
            return "该账号没有交易权限。".to_string();
        }
        if args.len() < 4 {
            return "用法：/open <交易对> <多头交易所> <空头交易所> <每条腿USD> [杠杆]\n例如：/open BTCUSDT binance okx 500 1"
                .to_string();
        }
        let symbol = args[0].to_ascii_uppercase();
        let long_venue = args[1].to_ascii_lowercase();
        let short_venue = args[2].to_ascii_lowercase();
        if long_venue == short_venue {
            return "双腿交易所不能相同。".to_string();
        }
        let Ok(notional_usd) = args[3].parse::<Decimal>() else {
            return "金额不是合法数字。".to_string();
        };
        if notional_usd <= Decimal::ZERO {
            return "金额必须大于 0。".to_string();
        }
        let leverage = match args.get(4) {
            Some(value) => match value.parse::<Decimal>() {
                Ok(value) if value > Decimal::ZERO => value,
                _ => return "杠杆不是合法数字。".to_string(),
            },
            None => Decimal::ONE,
        };

        for venue in [&long_venue, &short_venue] {
            if state.market.quote(venue, &symbol).is_none() {
                return format!("{symbol} 在 {venue} 没有可用行情，无法下单。");
            }
        }

        let token = Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase();
        let expires_at = Utc::now() + chrono::Duration::seconds(PENDING_TTL_SECS);
        {
            let mut pending = self.pending.lock().expect("待确认订单锁不可用");
            pending.retain(|_, trade| trade.expires_at > Utc::now() && trade.owner != user_id);
            if pending.values().filter(|trade| trade.owner == user_id).count() >= MAX_PENDING {
                return "待确认订单过多，请先 /cancel 旧订单。".to_string();
            }
            pending.insert(
                token.clone(),
                PendingTrade {
                    owner: user_id,
                    symbol: symbol.clone(),
                    long_venue: long_venue.clone(),
                    short_venue: short_venue.clone(),
                    notional_usd,
                    leverage,
                    expires_at,
                },
            );
        }

        format!(
            "待确认 {token}\n{symbol}：{long_venue} 多头 / {short_venue} 空头\n每条腿 {notional_usd} USD，杠杆 {leverage}x\n\n发送 /confirm {token} 在 {PENDING_TTL_SECS} 秒内确认，/cancel {token} 取消。下单仍会经过执行器风控。"
        )
    }

    async fn confirm(&self, state: &AppState, user_id: i64, args: &[&str]) -> String {
        let Some(token) = args.first() else {
            return "用法：/confirm <编号>".to_string();
        };
        let token = token.to_ascii_uppercase();
        let trade = {
            let mut pending = self.pending.lock().expect("待确认订单锁不可用");
            match pending.remove(&token) {
                Some(trade) => trade,
                None => return "没有找到该待确认订单，可能已过期。".to_string(),
            }
        };

        if trade.owner != user_id {
            return "该订单不属于当前账号。".to_string();
        }
        if trade.expires_at <= Utc::now() {
            return "该订单已过期，请重新发起 /open。".to_string();
        }

        let outcome = gateway::submit_pair(
            state,
            PairOrderRequest {
                symbol: trade.symbol.clone(),
                long_venue: trade.long_venue.clone(),
                short_venue: trade.short_venue.clone(),
                notional_usd: trade.notional_usd,
                leverage: trade.leverage,
                max_slippage_bps: None,
                strategy_id: None,
                origin: OrderOrigin::Telegram,
                idempotency_key: format!("telegram-{}", Uuid::new_v4()),
                closing: false,
                max_unhedged_secs: None,
            },
        )
        .await;

        match outcome {
            Ok(outcome) => {
                let order = outcome.order;
                let mut lines = vec![
                    format!("订单 {:?}", order.status),
                    format!("编号 {}", order.id),
                    format!(
                        "{}：{} 多 / {} 空，每条腿 {} USD",
                        order.intent.legs[0].symbol,
                        order.intent.legs[0].venue,
                        order.intent.legs[1].venue,
                        trade.notional_usd
                    ),
                ];
                if let Some(note) = &order.note {
                    lines.push(format!("备注：{note}"));
                }
                lines.join("\n")
            }
            Err(error) => format!("下单失败：{}", describe_error(&error)),
        }
    }

    fn cancel(&self, user_id: i64, args: &[&str]) -> String {
        let Some(token) = args.first() else {
            return "用法：/cancel <编号>".to_string();
        };
        let token = token.to_ascii_uppercase();
        let mut pending = self.pending.lock().expect("待确认订单锁不可用");
        match pending.get(&token) {
            Some(trade) if trade.owner == user_id => {
                pending.remove(&token);
                "已取消该待确认订单。".to_string()
            }
            _ => "没有找到该待确认订单。".to_string(),
        }
    }

    fn binding(&self, user_id: i64) -> Option<TelegramBinding> {
        self.bindings
            .read()
            .expect("电报绑定锁不可用")
            .iter()
            .find(|binding| binding.telegram_user_id == user_id)
            .cloned()
    }

    fn can_trade(&self, user_id: i64) -> bool {
        self.binding(user_id)
            .map(|binding| binding.permissions.iter().any(|item| item == "trade"))
            .unwrap_or(false)
    }

    async fn broadcast(&self, text: &str) {
        let targets: Vec<i64> = self
            .bindings
            .read()
            .expect("电报绑定锁不可用")
            .iter()
            .map(|binding| binding.chat_id)
            .collect();
        for chat_id in targets {
            self.send(chat_id, text).await;
        }
    }

    async fn send(&self, chat_id: i64, text: &str) {
        // Telegram 单条消息上限 4096 字符，超出时截断避免整条发送失败。
        let text: String = text.chars().take(3900).collect();
        let result = self
            .api(
                "sendMessage",
                &[
                    ("chat_id", chat_id.to_string()),
                    ("text", text),
                    ("disable_web_page_preview", "true".to_string()),
                ],
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(%error, chat_id, "发送 Telegram 消息失败");
        }
    }

    async fn api(&self, method: &str, params: &[(&str, String)]) -> anyhow::Result<Value> {
        let Some(http) = self.http.clone() else {
            bail!("HTTP 客户端不可用");
        };
        let Some(token) = self.token.clone() else {
            bail!("未配置 Bot Token");
        };
        let query = params
            .iter()
            .map(|(key, value)| format!("{key}={}", encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        let url = if query.is_empty() {
            format!("{}/bot{token}/{method}", self.api_base.trim_end_matches('/'))
        } else {
            format!(
                "{}/bot{token}/{method}?{query}",
                self.api_base.trim_end_matches('/')
            )
        };

        let value = tokio::task::spawn_blocking(move || http.get_json(&url))
            .await
            .context("Telegram 请求任务失败")??;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!(
                "Telegram API 返回错误: {}",
                value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("未知错误")
            );
        }
        Ok(value)
    }

    fn persist(&self) -> anyhow::Result<()> {
        let bindings = self.bindings.read().expect("电报绑定锁不可用").clone();
        write_bindings(&self.bindings_file, &bindings)
    }
}

fn help_text(enabled: bool) -> String {
    if !enabled {
        return "执行器尚未配置 Telegram Bot Token，请在服务器 .env 中设置 ARBPILOT_TELEGRAM_BOT_TOKEN 后重启。".to_string();
    }
    [
        "ArbPilot 指令：",
        "/status 执行器状态",
        "/positions 当前仓位",
        "/orders 最近订单",
        "/news 最新上币公告",
        "/pause 暂停开仓",
        "/resume 恢复开仓",
        "/open <交易对> <多头> <空头> <每条腿USD> [杠杆] 发起开仓",
        "/confirm <编号> 确认开仓",
        "/cancel <编号> 取消待确认订单",
        "/unbind 解除绑定",
        "",
        "API Key 只保存在执行器所在机器，机器人不会接收密钥。",
    ]
    .join("\n")
}

fn status_text(state: &AppState) -> String {
    let uptime = state.uptime_secs();
    let hours = uptime / 3600;
    let minutes = (uptime % 3600) / 60;
    format!(
        "执行器在线\n开仓：{}\n运行：{hours}h{minutes}m\n账户权益：{} USD",
        if state.is_paused() { "已暂停" } else { "正常" },
        state.accounts.total_equity_usd()
    )
}

fn positions_text(state: &AppState) -> String {
    let positions = state.positions.read().expect("仓位锁不可用");
    if positions.is_empty() {
        return "暂无仓位。".to_string();
    }
    let mut lines = vec![format!("当前仓位 {} 条：", positions.len())];
    for position in positions.iter().take(10) {
        lines.push(format!(
            "{} {} {} {} @ {}",
            position.symbol,
            position.venue,
            if matches!(position.side, arb_core::Side::Buy) {
                "多"
            } else {
                "空"
            },
            position.quantity,
            position.entry_price
        ));
    }
    lines.join("\n")
}

fn orders_text(state: &AppState) -> String {
    let orders = state.orders.read().expect("订单锁不可用");
    if orders.is_empty() {
        return "暂无订单。".to_string();
    }
    let mut lines = vec!["最近订单：".to_string()];
    for order in orders.iter().take(5) {
        lines.push(format!(
            "{:?} {} {}",
            order.status, order.intent.legs[0].symbol, order.id
        ));
    }
    lines.join("\n")
}

fn news_text(state: &AppState) -> String {
    let snapshot = state.news.snapshot(5);
    if snapshot.items.is_empty() {
        return "暂无上币公告。".to_string();
    }
    let mut lines = vec!["最新上币公告：".to_string()];
    for item in snapshot.items {
        let title: String = item.title.chars().take(80).collect();
        lines.push(format!("[{}] {}", item.source, title));
    }
    lines.join("\n")
}

fn describe_error(error: &ApiError) -> String {
    match error {
        ApiError::Unauthorized => "未授权".to_string(),
        ApiError::BadRequest(message)
        | ApiError::NotFound(message)
        | ApiError::Conflict(message)
        | ApiError::Internal(message) => message.clone(),
    }
}

/// Telegram 的 query 参数需要完整百分号编码，文本里的换行与空格都不能直接出现。
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn load_bindings(path: &Path) -> Vec<TelegramBinding> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "读取 Telegram 绑定文件失败");
            return Vec::new();
        }
    };
    serde_json::from_slice::<Vec<TelegramBinding>>(&raw).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "解析 Telegram 绑定文件失败");
        Vec::new()
    })
}

fn write_bindings(path: &Path, bindings: &[TelegramBinding]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建绑定目录失败: {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(bindings).context("序列化 Telegram 绑定失败")?;
    std::fs::write(path, json).with_context(|| format!("写入绑定文件失败: {}", path.display()))?;
    restrict_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("hello world"), "hello%20world");
        assert_eq!(encode("a&b=c\nd"), "a%26b%3Dc%0Ad");
        assert_eq!(encode("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn help_lists_trade_commands() {
        let text = help_text(true);
        assert!(text.contains("/open"));
        assert!(text.contains("/confirm"));
        assert!(!text.contains("尚未配置"));
    }
}
