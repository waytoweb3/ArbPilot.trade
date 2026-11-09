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

/// 机器人回复与推送支持的语言。Telegram 用户可在会话里用 `/lang` 切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    /// 首次绑定时按 Telegram 的 language_code 判断；非中文一律回落到英文。
    fn from_code(code: Option<&str>) -> Self {
        match code {
            Some(code) if code.to_ascii_lowercase().starts_with("zh") => Lang::Zh,
            Some(code) if code.eq_ignore_ascii_case("cn") => Lang::Zh,
            Some(_) => Lang::En,
            None => Lang::Zh,
        }
    }

    /// 解析 `/lang` 的参数。
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "zh" | "cn" | "zh-cn" | "zh-hans" | "zh-hant" | "chinese" | "中文" => Some(Lang::Zh),
            "en" | "en-us" | "en-gb" | "english" => Some(Lang::En),
            _ => None,
        }
    }
}

/// 按语言取一段固定文案。
fn pick(lang: Lang, zh: &'static str, en: &'static str) -> &'static str {
    match lang {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 同一条推送的中英文版本；广播时按每个绑定账号的语言选择。
#[derive(Debug, Clone)]
pub struct TelegramMessage {
    pub zh: String,
    pub en: String,
}

impl TelegramMessage {
    pub fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            zh: zh.into(),
            en: en.into(),
        }
    }

    fn for_lang(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => &self.zh,
            Lang::En => &self.en,
        }
    }
}

/// Telegram 绑定关系，保存在执行器本机（若执行器跑在用户服务器上，就存在那台服务器）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramBinding {
    pub telegram_user_id: i64,
    pub chat_id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 该账号的机器人语言；旧绑定文件没有这个字段时按消息里的 language_code 判断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<Lang>,
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

/// `/open` 的回复：校验失败时只有文案，成功时附带确认/取消按钮。
struct PreparedOpen {
    text: String,
    markup: Option<Value>,
}

impl PreparedOpen {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markup: None,
        }
    }
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
                        *bot.last_error.write().expect("电报错误锁不可用") =
                            Some(error.to_string());
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
                    Ok(ExecutorEvent::Alert {
                        message,
                        message_en,
                        ..
                    }) => {
                        let en = message_en.unwrap_or_else(|| message.clone());
                        bot.broadcast(&TelegramMessage::new(message, en)).await;
                    }
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
            bot_username: self.bot_username.read().expect("电报身份锁不可用").clone(),
            last_error: self.last_error.read().expect("电报错误锁不可用").clone(),
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
            if let Some(callback) = update.get("callback_query") {
                self.handle_callback(state, callback).await;
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
        let language_code = message
            .pointer("/from/language_code")
            .and_then(Value::as_str);
        let text = message
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            return;
        }
        // 绑定文件可能被另一个执行器实例改写（例如桌面端重启后残留的旧进程），
        // 处理指令前重新读取，避免继续用启动时的旧快照误判“尚未绑定”。
        self.sync_bindings();

        let mut parts = text.split_whitespace();
        let command = parts
            .next()
            .unwrap_or_default()
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let args: Vec<&str> = parts.collect();
        let lang = self.lang_for(user_id, language_code);

        let mut markup = None;
        let reply = match command.as_str() {
            "/start" | "/help" => help_text(lang, self.enabled()),
            "/bind" => self.bind(chat_id, user_id, username, language_code, &args),
            _ => {
                if self.binding(user_id).is_none() {
                    pick(
                        lang,
                        "尚未绑定。请在桌面端「设置 → Telegram」生成配对码，然后发送 /bind <配对码>。",
                        "Not linked yet. Create a pairing code in the desktop app under Settings → Telegram, then send /bind <code>.",
                    )
                    .to_string()
                } else {
                    match command.as_str() {
                        "/status" => status_text(lang, state),
                        "/positions" => positions_text(lang, state),
                        "/orders" => orders_text(lang, state),
                        "/news" => news_text(lang, state),
                        "/pause" => {
                            state.set_paused(
                                true,
                                Some(("Telegram 指令", "Telegram command")),
                            );
                            pick(lang, "已暂停开仓。", "New entries paused.").to_string()
                        }
                        "/resume" => {
                            state.set_paused(false, None);
                            pick(lang, "已恢复开仓。", "New entries resumed.").to_string()
                        }
                        "/open" => {
                            let prepared = self.prepare_open(state, user_id, lang, &args);
                            markup = prepared.markup;
                            prepared.text
                        }
                        "/lang" | "/language" => self.set_lang(user_id, lang, &args),
                        "/unbind" => self.unbind(user_id, lang),
                        other => format!(
                            "{}\n\n{}",
                            match lang {
                                Lang::Zh => format!("未识别的指令 {other}"),
                                Lang::En => format!("Unknown command {other}"),
                            },
                            help_text(lang, self.enabled())
                        ),
                    }
                }
            }
        };
        self.send_with_markup(chat_id, &reply, markup.as_ref()).await;
    }

    /// 处理按钮点击：按钮回调携带待确认订单编号，用户无需手输。
    async fn handle_callback(&self, state: &AppState, callback: &Value) {
        let Some(callback_id) = callback.get("id").and_then(Value::as_str) else {
            return;
        };
        let user_id = callback
            .pointer("/from/id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let language_code = callback
            .pointer("/from/language_code")
            .and_then(Value::as_str);
        let chat_id = callback.pointer("/message/chat/id").and_then(Value::as_i64);
        let message_id = callback
            .pointer("/message/message_id")
            .and_then(Value::as_i64);
        let data = callback.get("data").and_then(Value::as_str).unwrap_or("");

        self.sync_bindings();
        let lang = self.lang_for(user_id, language_code);

        let reply = match parse_callback(data) {
            Some((action, token)) => {
                if self.binding(user_id).is_none() {
                    pick(
                        lang,
                        "尚未绑定，无法操作待确认订单。",
                        "This account is not linked; cannot act on the pending order.",
                    )
                    .to_string()
                } else if !self.can_trade(user_id) {
                    pick(
                        lang,
                        "该账号没有交易权限。",
                        "This account has no trading permission.",
                    )
                    .to_string()
                } else if action == "confirm" {
                    self.confirm(state, user_id, lang, token).await
                } else {
                    self.cancel(user_id, lang, token)
                }
            }
            None => pick(lang, "无法识别的操作。", "Unrecognized action.").to_string(),
        };

        // 先回应回调，避免按钮一直处于加载状态。
        let acknowledged = self
            .api(
                "answerCallbackQuery",
                &[("callback_query_id", callback_id.to_string())],
            )
            .await;
        if let Err(error) = acknowledged {
            tracing::warn!(%error, "回应 Telegram 按钮回调失败");
        }

        match (chat_id, message_id) {
            (Some(chat_id), Some(message_id)) => {
                // 用结果覆盖原消息并清空按钮，避免重复点击。
                let edited = self
                    .edit_message(chat_id, message_id, &reply)
                    .await;
                if let Err(error) = edited {
                    tracing::warn!(%error, "更新 Telegram 待确认消息失败，改为发送新消息");
                    self.send(chat_id, &reply).await;
                }
            }
            _ => {}
        }
    }

    /// 已绑定的账号以绑定里记录的语言为准；旧绑定按消息的 language_code 判断并回写。
    fn lang_for(&self, user_id: i64, language_code: Option<&str>) -> Lang {
        let current = self.binding(user_id);
        if let Some(lang) = current.as_ref().and_then(|binding| binding.lang) {
            return lang;
        }
        let lang = Lang::from_code(language_code);
        if current.is_some() {
            if let Err(error) = self.store_lang(user_id, lang) {
                tracing::warn!(%error, "写入 Telegram 绑定语言失败");
            }
        }
        lang
    }

    fn bind(
        &self,
        chat_id: i64,
        user_id: i64,
        username: Option<String>,
        language_code: Option<&str>,
        args: &[&str],
    ) -> String {
        let lang = self.lang_for(user_id, language_code);
        if self.binding(user_id).is_some() {
            return pick(
                lang,
                "该账号已绑定，如需换绑请先发送 /unbind。",
                "This account is already linked. Send /unbind first to switch.",
            )
            .to_string();
        }
        let Some(code) = args.first() else {
            return pick(lang, "用法：/bind <配对码>", "Usage: /bind <code>").to_string();
        };

        let matched = {
            let pairing = self.pairing.lock().expect("配对码锁不可用");
            matches!(
                pairing.as_ref(),
                Some(active) if active.code.eq_ignore_ascii_case(code) && active.expires_at > Utc::now()
            )
        };
        if !matched {
            return pick(
                lang,
                "配对码不正确或已过期，请在桌面端重新生成。",
                "The pairing code is invalid or expired. Create a new one in the desktop app.",
            )
            .to_string();
        }

        *self.pairing.lock().expect("配对码锁不可用") = None;
        {
            let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
            bindings.push(TelegramBinding {
                telegram_user_id: user_id,
                chat_id,
                username,
                permissions: vec!["read".to_string(), "trade".to_string()],
                lang: Some(lang),
                paired_at: Utc::now(),
            });
        }
        match self.persist() {
            Ok(()) => pick(
                lang,
                "绑定成功。发送 /help 查看可用指令，开仓需要二次确认。",
                "Linked successfully. Send /help for commands; entries need a second confirmation.",
            )
            .to_string(),
            Err(error) => match lang {
                Lang::Zh => format!("绑定已记录，但写入绑定文件失败：{error}"),
                Lang::En => format!("Linked, but writing the bindings file failed: {error}"),
            },
        }
    }

    fn unbind(&self, user_id: i64, lang: Lang) -> String {
        let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
        let before = bindings.len();
        bindings.retain(|binding| binding.telegram_user_id != user_id);
        if before == bindings.len() {
            return pick(lang, "该账号尚未绑定。", "This account is not linked.").to_string();
        }
        drop(bindings);
        match self.persist() {
            Ok(()) => pick(
                lang,
                "已解除绑定，后续指令将不再响应。",
                "Unlinked. The bot will ignore further commands.",
            )
            .to_string(),
            Err(error) => match lang {
                Lang::Zh => format!("已解除绑定，但写入绑定文件失败：{error}"),
                Lang::En => format!("Unlinked, but writing the bindings file failed: {error}"),
            },
        }
    }

    /// 切换当前账号的机器人语言；`/lang` 不带参数时返回当前语言与用法。
    fn set_lang(&self, user_id: i64, current: Lang, args: &[&str]) -> String {
        let Some(value) = args.first() else {
            return match current {
                Lang::Zh => "当前语言：中文。用法：/lang zh|en".to_string(),
                Lang::En => "Current language: English. Usage: /lang zh|en".to_string(),
            };
        };
        let Some(lang) = Lang::parse(value) else {
            return pick(
                current,
                "不支持的语言。用法：/lang zh|en",
                "Unsupported language. Usage: /lang zh|en",
            )
            .to_string();
        };
        match self.store_lang(user_id, lang) {
            Ok(()) => pick(lang, "已切换为中文。", "Language switched to English.").to_string(),
            Err(error) => match lang {
                Lang::Zh => format!("语言已切换，但写入绑定文件失败：{error}"),
                Lang::En => format!("Language switched, but writing the bindings file failed: {error}"),
            },
        }
    }

    fn store_lang(&self, user_id: i64, lang: Lang) -> anyhow::Result<()> {
        {
            let mut bindings = self.bindings.write().expect("电报绑定锁不可用");
            let Some(binding) = bindings
                .iter_mut()
                .find(|binding| binding.telegram_user_id == user_id)
            else {
                return Ok(());
            };
            if binding.lang == Some(lang) {
                return Ok(());
            }
            binding.lang = Some(lang);
        }
        self.persist()
    }

    fn prepare_open(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        args: &[&str],
    ) -> PreparedOpen {
        if !self.can_trade(user_id) {
            return PreparedOpen::text(pick(
                lang,
                "该账号没有交易权限。",
                "This account has no trading permission.",
            ));
        }
        if args.len() < 4 {
            return PreparedOpen::text(pick(
                lang,
                "用法：/open <交易对> <多头交易所> <空头交易所> <每条腿USD> [杠杆]\n例如：/open BTCUSDT binance okx 500 1",
                "Usage: /open <symbol> <long venue> <short venue> <USD per leg> [leverage]\nExample: /open BTCUSDT binance okx 500 1",
            ));
        }
        let symbol = args[0].to_ascii_uppercase();
        let long_venue = args[1].to_ascii_lowercase();
        let short_venue = args[2].to_ascii_lowercase();
        if long_venue == short_venue {
            return PreparedOpen::text(pick(
                lang,
                "双腿交易所不能相同。",
                "The two legs cannot use the same venue.",
            ));
        }
        let Ok(notional_usd) = args[3].parse::<Decimal>() else {
            return PreparedOpen::text(pick(
                lang,
                "金额不是合法数字。",
                "The amount is not a valid number.",
            ));
        };
        if notional_usd <= Decimal::ZERO {
            return PreparedOpen::text(pick(
                lang,
                "金额必须大于 0。",
                "The amount must be greater than 0.",
            ));
        }
        let leverage = match args.get(4) {
            Some(value) => match value.parse::<Decimal>() {
                Ok(value) if value > Decimal::ZERO => value,
                _ => {
                    return PreparedOpen::text(pick(
                        lang,
                        "杠杆不是合法数字。",
                        "The leverage is not a valid number.",
                    ))
                }
            },
            None => Decimal::ONE,
        };

        for venue in [&long_venue, &short_venue] {
            if state.market.quote(venue, &symbol).is_none() {
                return PreparedOpen::text(match lang {
                    Lang::Zh => {
                        format!("{symbol} 在 {venue} 没有可用行情，无法下单。")
                    }
                    Lang::En => {
                        format!("No market data for {symbol} on {venue}; cannot place the order.")
                    }
                });
            }
        }

        let token = Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase();
        let expires_at = Utc::now() + chrono::Duration::seconds(PENDING_TTL_SECS);
        {
            let mut pending = self.pending.lock().expect("待确认订单锁不可用");
            pending.retain(|_, trade| trade.expires_at > Utc::now() && trade.owner != user_id);
            if pending
                .values()
                .filter(|trade| trade.owner == user_id)
                .count()
                >= MAX_PENDING
            {
                return PreparedOpen::text(pick(
                    lang,
                    "待确认订单过多，请等待旧订单过期或点击旧订单的取消按钮。",
                    "Too many pending orders. Wait for older ones to expire or tap their Cancel buttons.",
                ));
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

        let text = match lang {
            Lang::Zh => format!(
                "待确认 {token}\n{symbol}：{long_venue} 多头 / {short_venue} 空头\n每条腿 {notional_usd} USD，杠杆 {leverage}x\n\n请点击下方按钮在 {PENDING_TTL_SECS} 秒内确认或取消。下单仍会经过执行器风控。"
            ),
            Lang::En => format!(
                "Pending {token}\n{symbol}: long {long_venue} / short {short_venue}\n{notional_usd} USD per leg, leverage {leverage}x\n\nTap a button below within {PENDING_TTL_SECS}s to confirm or cancel. Risk checks still apply."
            ),
        };
        let markup = serde_json::json!({
            "inline_keyboard": [[
                {
                    "text": pick(lang, "确认开仓", "Confirm entry"),
                    "callback_data": format!("confirm:{token}"),
                },
                {
                    "text": pick(lang, "取消", "Cancel"),
                    "callback_data": format!("cancel:{token}"),
                },
            ]],
        });
        PreparedOpen {
            text,
            markup: Some(markup),
        }
    }

    async fn confirm(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        token: &str,
    ) -> String {
        let token = token.to_ascii_uppercase();
        let trade = {
            let mut pending = self.pending.lock().expect("待确认订单锁不可用");
            match pending.remove(&token) {
                Some(trade) => trade,
                None => {
                    return pick(
                        lang,
                        "没有找到该待确认订单，可能已过期。",
                        "Pending order not found; it may have expired.",
                    )
                    .to_string()
                }
            }
        };

        if trade.owner != user_id {
            return pick(
                lang,
                "该订单不属于当前账号。",
                "This order belongs to another account.",
            )
            .to_string();
        }
        if trade.expires_at <= Utc::now() {
            return pick(
                lang,
                "该订单已过期，请重新发起 /open。",
                "The order expired. Send /open again.",
            )
            .to_string();
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
                let mut lines = match lang {
                    Lang::Zh => vec![
                        format!("订单 {:?}", order.status),
                        format!("编号 {}", order.id),
                        format!(
                            "{}：{} 多 / {} 空，每条腿 {} USD",
                            order.intent.legs[0].symbol,
                            order.intent.legs[0].venue,
                            order.intent.legs[1].venue,
                            trade.notional_usd
                        ),
                    ],
                    Lang::En => vec![
                        format!("Order {:?}", order.status),
                        format!("ID {}", order.id),
                        format!(
                            "{}: long {} / short {}, {} USD per leg",
                            order.intent.legs[0].symbol,
                            order.intent.legs[0].venue,
                            order.intent.legs[1].venue,
                            trade.notional_usd
                        ),
                    ],
                };
                if let Some(note) = &order.note {
                    lines.push(match lang {
                        Lang::Zh => format!("备注：{note}"),
                        Lang::En => format!("Note: {note}"),
                    });
                }
                lines.join("\n")
            }
            Err(error) => match lang {
                Lang::Zh => format!("下单失败：{}", describe_error(lang, &error)),
                Lang::En => format!("Order failed: {}", describe_error(lang, &error)),
            },
        }
    }

    fn cancel(&self, user_id: i64, lang: Lang, token: &str) -> String {
        let token = token.to_ascii_uppercase();
        let mut pending = self.pending.lock().expect("待确认订单锁不可用");
        match pending.get(&token) {
            Some(trade) if trade.owner == user_id => {
                pending.remove(&token);
                pick(lang, "已取消该待确认订单。", "Pending order cancelled.").to_string()
            }
            _ => pick(lang, "没有找到该待确认订单。", "Pending order not found.").to_string(),
        }
    }

    /// 从绑定文件刷新内存快照；文件缺失或损坏时保留当前内存值。
    fn sync_bindings(&self) {
        if let Some(bindings) = read_bindings(&self.bindings_file) {
            *self.bindings.write().expect("电报绑定锁不可用") = bindings;
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

    /// 向所有已绑定账号推送一条消息，供告警与通知分发使用；每个账号收到自己语言的版本。
    pub async fn broadcast(&self, message: &TelegramMessage) {
        let targets: Vec<(i64, Lang)> = self
            .bindings
            .read()
            .expect("电报绑定锁不可用")
            .iter()
            .map(|binding| (binding.chat_id, binding.lang.unwrap_or(Lang::Zh)))
            .collect();
        for (chat_id, lang) in targets {
            self.send(chat_id, message.for_lang(lang)).await;
        }
    }

    async fn send(&self, chat_id: i64, text: &str) {
        self.send_with_markup(chat_id, text, None).await;
    }

    /// 发送消息；`markup` 非空时附带内联键盘（如待确认订单的确认/取消按钮）。
    async fn send_with_markup(&self, chat_id: i64, text: &str, markup: Option<&Value>) {
        // Telegram 单条消息上限 4096 字符，超出时截断避免整条发送失败。
        let text: String = text.chars().take(3900).collect();
        let mut params = vec![
            ("chat_id", chat_id.to_string()),
            ("text", text),
            ("disable_web_page_preview", "true".to_string()),
        ];
        if let Some(markup) = markup {
            params.push(("reply_markup", markup.to_string()));
        }
        let result = self.api("sendMessage", &params).await;
        if let Err(error) = result {
            tracing::warn!(%error, chat_id, "发送 Telegram 消息失败");
        }
    }

    /// 用处理结果覆盖原消息并清空内联按钮，避免重复点击。
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> anyhow::Result<()> {
        let text: String = text.chars().take(3900).collect();
        self.api(
            "editMessageText",
            &[
                ("chat_id", chat_id.to_string()),
                ("message_id", message_id.to_string()),
                ("text", text),
                ("reply_markup", r#"{"inline_keyboard":[]}"#.to_string()),
            ],
        )
        .await?;
        Ok(())
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
            format!(
                "{}/bot{token}/{method}",
                self.api_base.trim_end_matches('/')
            )
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

fn help_text(lang: Lang, enabled: bool) -> String {
    if !enabled {
        return pick(
            lang,
            "执行器尚未配置 Telegram Bot Token，请在服务器 .env 中设置 ARBPILOT_TELEGRAM_BOT_TOKEN 后重启。",
            "The executor has no Telegram bot token. Set ARBPILOT_TELEGRAM_BOT_TOKEN in the server .env and restart.",
        )
        .to_string();
    }
    match lang {
        Lang::Zh => [
            "ArbPilot 指令：",
            "/status 执行器状态",
            "/positions 当前仓位",
            "/orders 最近订单",
            "/news 最新上币公告",
            "/pause 暂停开仓",
            "/resume 恢复开仓",
            "/open <交易对> <多头> <空头> <每条腿USD> [杠杆] 发起开仓",
            "开仓会回复确认按钮，点击「确认开仓」后才真正下单",
            "/lang zh|en 切换机器人语言",
            "/unbind 解除绑定",
            "",
            "API Key 只保存在执行器所在机器，机器人不会接收密钥。",
        ]
        .join("\n"),
        Lang::En => [
            "ArbPilot commands:",
            "/status Executor status",
            "/positions Current positions",
            "/orders Recent orders",
            "/news Latest listing announcements",
            "/pause Pause new entries",
            "/resume Resume entries",
            "/open <symbol> <long venue> <short venue> <USD per leg> [leverage] Prepare an entry",
            "Entries reply with a confirm button; tap it to place the order",
            "/lang zh|en Switch the bot language",
            "/unbind Unlink this account",
            "",
            "API keys stay on the machine running the executor; the bot never receives them.",
        ]
        .join("\n"),
    }
}

fn status_text(lang: Lang, state: &AppState) -> String {
    let uptime = state.uptime_secs();
    let hours = uptime / 3600;
    let minutes = (uptime % 3600) / 60;
    match lang {
        Lang::Zh => format!(
            "执行器在线\n开仓：{}\n运行：{hours}h{minutes}m\n账户权益：{} USD",
            if state.is_paused() {
                "已暂停"
            } else {
                "正常"
            },
            state.accounts.total_equity_usd().round_dp(2)
        ),
        Lang::En => format!(
            "Executor online\nEntries: {}\nUptime: {hours}h{minutes}m\nAccount equity: {} USD",
            if state.is_paused() {
                "paused"
            } else {
                "active"
            },
            state.accounts.total_equity_usd().round_dp(2)
        ),
    }
}

fn positions_text(lang: Lang, state: &AppState) -> String {
    let positions = state.positions.read().expect("仓位锁不可用");
    if positions.is_empty() {
        return pick(lang, "暂无仓位。", "No open positions.").to_string();
    }
    let mut lines = vec![match lang {
        Lang::Zh => format!("当前仓位 {} 条：", positions.len()),
        Lang::En => format!("Open positions: {}", positions.len()),
    }];
    for position in positions.iter().take(10) {
        lines.push(format!(
            "{} {} {} {} @ {}",
            position.symbol,
            position.venue,
            pick(
                lang,
                if matches!(position.side, arb_core::Side::Buy) {
                    "多"
                } else {
                    "空"
                },
                if matches!(position.side, arb_core::Side::Buy) {
                    "Long"
                } else {
                    "Short"
                },
            ),
            position.quantity,
            position.entry_price
        ));
    }
    lines.join("\n")
}

fn orders_text(lang: Lang, state: &AppState) -> String {
    let orders = state.orders.read().expect("订单锁不可用");
    if orders.is_empty() {
        return pick(lang, "暂无订单。", "No orders yet.").to_string();
    }
    let mut lines = vec![pick(lang, "最近订单：", "Recent orders:").to_string()];
    for order in orders.iter().take(5) {
        lines.push(format!(
            "{:?} {} {}",
            order.status, order.intent.legs[0].symbol, order.id
        ));
    }
    lines.join("\n")
}

fn news_text(lang: Lang, state: &AppState) -> String {
    let snapshot = state.news.snapshot(5);
    if snapshot.items.is_empty() {
        return pick(lang, "暂无上币公告。", "No listing announcements.").to_string();
    }
    let mut lines = vec![pick(lang, "最新上币公告：", "Latest listing announcements:").to_string()];
    for item in snapshot.items {
        let title: String = item.title.chars().take(80).collect();
        lines.push(format!("[{}] {}", item.source, title));
    }
    lines.join("\n")
}

fn describe_error(lang: Lang, error: &ApiError) -> String {
    match error {
        ApiError::Unauthorized => pick(lang, "未授权", "Unauthorized").to_string(),
        ApiError::UnauthorizedWith(message)
        | ApiError::BadRequest(message)
        | ApiError::NotFound(message)
        | ApiError::Conflict(message)
        | ApiError::PaymentRequired(message)
        | ApiError::Forbidden(message)
        | ApiError::Internal(message) => message.clone(),
    }
}

/// 解析内联按钮的 `callback_data`，格式为 `confirm:<编号>` 或 `cancel:<编号>`。
fn parse_callback(data: &str) -> Option<(&str, &str)> {
    let (action, token) = data.split_once(':')?;
    if token.is_empty() || !matches!(action, "confirm" | "cancel") {
        return None;
    }
    Some((action, token))
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
    read_bindings(path).unwrap_or_default()
}

/// 读取并解析绑定文件；文件缺失或内容损坏时返回 `None`，由调用方决定是否保留内存中的旧值。
fn read_bindings(path: &Path) -> Option<Vec<TelegramBinding>> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "读取 Telegram 绑定文件失败");
            return None;
        }
    };
    serde_json::from_slice::<Vec<TelegramBinding>>(&raw)
        .map_err(|error| {
            tracing::warn!(%error, path = %path.display(), "解析 Telegram 绑定文件失败");
        })
        .ok()
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
        let text = help_text(Lang::Zh, true);
        assert!(text.contains("/open"));
        assert!(text.contains("确认按钮"));
        assert!(!text.contains("/confirm"));
        assert!(!text.contains("/cancel"));
        assert!(text.contains("/lang"));
        assert!(!text.contains("尚未配置"));
    }

    #[test]
    fn parses_callback_data() {
        assert_eq!(parse_callback("confirm:ABC123"), Some(("confirm", "ABC123")));
        assert_eq!(parse_callback("cancel:ABC123"), Some(("cancel", "ABC123")));
        assert_eq!(parse_callback("confirm:"), None);
        assert_eq!(parse_callback("delete:ABC123"), None);
        assert_eq!(parse_callback("ABC123"), None);
    }

    #[test]
    fn help_is_available_in_english() {
        let text = help_text(Lang::En, true);
        assert!(text.contains("/open"));
        assert!(text.contains("/lang"));
        assert!(!text.contains("指令"));
        assert!(help_text(Lang::En, false).contains("ARBPILOT_TELEGRAM_BOT_TOKEN"));
    }

    #[test]
    fn detects_language_codes() {
        assert_eq!(Lang::from_code(Some("zh-hans")), Lang::Zh);
        assert_eq!(Lang::from_code(Some("zh-CN")), Lang::Zh);
        assert_eq!(Lang::from_code(Some("en")), Lang::En);
        assert_eq!(Lang::from_code(Some("ru")), Lang::En);
        assert_eq!(Lang::from_code(None), Lang::Zh);
    }

    #[test]
    fn parses_lang_arguments() {
        assert_eq!(Lang::parse("zh"), Some(Lang::Zh));
        assert_eq!(Lang::parse("EN"), Some(Lang::En));
        assert_eq!(Lang::parse("fr"), None);
    }
}
