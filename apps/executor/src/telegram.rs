use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{bail, Context};
use arb_core::{OrderOrigin, Side, StrategyExecutionMode};
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
    listing::{
        self, CloseReason, ListingAnnouncement, ListingRule, ListingTrade, OpenListingRequest,
    },
    skill,
    state::AppState,
};

/// 待确认订单的有效期。
const PENDING_TTL_SECS: i64 = 120;
/// 单个绑定用户最多保留的待确认订单数。
const MAX_PENDING: usize = 5;
/// 上币公告卡片在电报里的有效时间；超过后按钮失效，需要重新扫到公告。
const LISTING_TTL_SECS: i64 = 30 * 60;
/// 用户只输入金额时的默认杠杆。
const DEFAULT_LEVERAGE: Decimal = Decimal::TEN;

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

/// 开仓向导当前在等用户做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenStage {
    /// 等用户发送交易对（`/open` 不带参数时）。
    Symbol,
    /// 等用户点按钮选多头交易所。
    Long,
    /// 等用户点按钮选空头交易所。
    Short,
    /// 等用户发送每条腿金额与杠杆。
    Amount,
}

/// 单个用户进行中的开仓向导；金额与杠杆由用户手输，交易所由按钮选择。
#[derive(Debug, Clone)]
struct OpenDraft {
    symbol: Option<String>,
    long_venue: Option<String>,
    short_venue: Option<String>,
    stage: OpenStage,
    expires_at: DateTime<Utc>,
}

/// 机器人回复：正文 + 可选内联键盘（确认按钮、交易所选择等）。
struct BotReply {
    text: String,
    markup: Option<Value>,
}

impl BotReply {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markup: None,
        }
    }

    fn with_markup(text: impl Into<String>, markup: Value) -> Self {
        Self {
            text: text.into(),
            markup: Some(markup),
        }
    }
}

/// 开仓向导里的按钮动作：选多头、选空头、取消。
#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenStep {
    Long(String),
    Short(String),
    Cancel,
}

/// 电报里的一条上币公告卡片，编号在按钮回调中回传。
#[derive(Debug, Clone)]
struct ListingCard {
    announcement: ListingAnnouncement,
    expires_at: DateTime<Utc>,
}

/// 用户已选交易所、等待确认的上币单腿订单。
#[derive(Debug, Clone)]
struct ListingDraft {
    /// 卡片编号，用于校验回调与草稿是否匹配。
    id: String,
    announcement: ListingAnnouncement,
    venue: String,
    expires_at: DateTime<Utc>,
}

/// 上币卡片上的按钮动作：选交易所、确认开仓、取消。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListingStep {
    Venue { id: String, venue: String },
    Confirm(String),
    Cancel(String),
}

/// 按钮回调：待确认订单的确认/取消、开仓向导，以及上币卡片。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Callback {
    Confirm(String),
    Cancel(String),
    Open(OpenStep),
    Listing(ListingStep),
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
    /// 每个用户进行中的开仓向导（交易对、两条腿的交易所）。
    drafts: Mutex<HashMap<i64, OpenDraft>>,
    /// 已推送的上币公告卡片：编号 → 公告信息。
    listing_cards: Mutex<HashMap<String, ListingCard>>,
    /// 每个用户已选交易所、等待确认的上币单腿订单。
    listing_drafts: Mutex<HashMap<i64, ListingDraft>>,
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
            drafts: Mutex::new(HashMap::new()),
            listing_cards: Mutex::new(HashMap::new()),
            listing_drafts: Mutex::new(HashMap::new()),
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
                self.register_commands().await;
            }
            Err(error) => {
                *self.last_error.write().expect("电报错误锁不可用") = Some(error.to_string());
            }
        }
    }

    /// 注册输入框旁的命令菜单；中文作为默认，英文单独注册一份。
    async fn register_commands(&self) {
        let default = self
            .api_post(
                "setMyCommands",
                &serde_json::json!({ "commands": command_menu(Lang::Zh) }),
            )
            .await;
        let english = self
            .api_post(
                "setMyCommands",
                &serde_json::json!({
                    "commands": command_menu(Lang::En),
                    "language_code": "en",
                }),
            )
            .await;
        if let Err(error) = default {
            tracing::warn!(%error, "注册 Telegram 命令菜单失败");
        }
        if let Err(error) = english {
            tracing::warn!(%error, "注册 Telegram 英文命令菜单失败");
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

        // 开仓向导进行中时，用户下一条普通文本就是交易对或金额/杠杆。
        if !text.starts_with('/') {
            if let Some(reply) = self.open_input(state, user_id, lang, text) {
                self.send_with_markup(chat_id, &reply.text, reply.markup.as_ref())
                    .await;
                return;
            }
        }

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
                        "/pause" => self.toggle_pause(state, user_id, lang, true),
                        "/resume" => self.toggle_pause(state, user_id, lang, false),
                        "/open" => {
                            let prepared = self.prepare_open(state, user_id, lang, &args);
                            markup = prepared.markup;
                            prepared.text
                        }
                        "/listing" => {
                            let prepared = self.listing_command(state, user_id, lang, &args).await;
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
        self.send_with_markup(chat_id, &reply, markup.as_ref())
            .await;
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

        let reply = if self.binding(user_id).is_none() {
            BotReply::text(pick(
                lang,
                "尚未绑定，无法操作。",
                "This account is not linked; cannot act.",
            ))
        } else {
            match parse_callback(data) {
                Some(Callback::Confirm(token)) => {
                    if self.can_trade(user_id) {
                        BotReply::text(self.confirm(state, user_id, lang, &token).await)
                    } else {
                        BotReply::text(pick(
                            lang,
                            "该账号没有交易权限。",
                            "This account has no trading permission.",
                        ))
                    }
                }
                Some(Callback::Cancel(token)) => BotReply::text(self.cancel(user_id, lang, &token)),
                Some(Callback::Open(step)) => {
                    if self.can_trade(user_id) {
                        self.open_step(state, user_id, lang, step)
                    } else {
                        BotReply::text(pick(
                            lang,
                            "该账号没有交易权限。",
                            "This account has no trading permission.",
                        ))
                    }
                }
                Some(Callback::Listing(step)) => match step {
                    ListingStep::Venue { id, venue } => {
                        if self.can_trade(user_id) {
                            self.listing_preview(state, user_id, lang, &id, &venue)
                                .await
                        } else {
                            BotReply::text(pick(
                                lang,
                                "该账号没有交易权限。",
                                "This account has no trading permission.",
                            ))
                        }
                    }
                    ListingStep::Confirm(id) => {
                        if self.can_trade(user_id) {
                            self.confirm_listing(state, user_id, lang, &id).await
                        } else {
                            BotReply::text(pick(
                                lang,
                                "该账号没有交易权限。",
                                "This account has no trading permission.",
                            ))
                        }
                    }
                    ListingStep::Cancel(id) => self.cancel_listing(user_id, lang, &id),
                },
                None => BotReply::text(pick(lang, "无法识别的操作。", "Unrecognized action.")),
            }
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
                // 用结果覆盖原消息并清空/替换按钮，避免重复点击。
                let edited = self
                    .edit_message(chat_id, message_id, &reply.text, reply.markup.as_ref())
                    .await;
                if let Err(error) = edited {
                    // 重复点击同一菜单项时正文可能完全一致，Telegram 会报「未修改」，无需重发。
                    if !error.to_string().contains("message is not modified") {
                        tracing::warn!(%error, "更新 Telegram 待确认消息失败，改为发送新消息");
                        self.send_with_markup(chat_id, &reply.text, reply.markup.as_ref())
                            .await;
                    }
                }
            }
            (Some(chat_id), None) => {
                self.send_with_markup(chat_id, &reply.text, reply.markup.as_ref())
                    .await;
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
                Lang::En => {
                    format!("Language switched, but writing the bindings file failed: {error}")
                }
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

    /// `/open` 入口：交易对与交易所都可以逐步选择，金额和杠杆由用户输入。
    fn prepare_open(&self, state: &AppState, user_id: i64, lang: Lang, args: &[&str]) -> BotReply {
        if !self.can_trade(user_id) {
            return BotReply::text(pick(
                lang,
                "该账号没有交易权限。",
                "This account has no trading permission.",
            ));
        }
        let venues = tradable_venues(state);
        if venues.is_empty() {
            return BotReply::text(
                pick(
                    lang,
                    "还没有可交易的交易所账户，请先在桌面端「账户」导入 API Key。",
                    "No tradable exchange account yet. Import API keys from Accounts in the desktop app first.",
                ),
            );
        }
        if args.len() > 3 {
            return BotReply::text(wizard_only_text(lang));
        }

        let mut draft = OpenDraft {
            symbol: None,
            long_venue: None,
            short_venue: None,
            stage: OpenStage::Symbol,
            expires_at: draft_expiry(),
        };
        if let Some(raw) = args.first() {
            let Some(symbol) = parse_symbol(raw) else {
                return BotReply::text(symbol_error_text(lang));
            };
            draft.symbol = Some(symbol);
            draft.stage = OpenStage::Long;
        }
        if let Some(raw) = args.get(1) {
            let venue = raw.to_ascii_lowercase();
            if !venues.contains(&venue) {
                return BotReply::text(unknown_venue_text(lang, &venue));
            }
            draft.long_venue = Some(venue);
            draft.stage = OpenStage::Short;
        }
        if let Some(raw) = args.get(2) {
            let venue = raw.to_ascii_lowercase();
            if !venues.contains(&venue) {
                return BotReply::text(unknown_venue_text(lang, &venue));
            }
            if draft.long_venue.as_ref() == Some(&venue) {
                return BotReply::text(same_venue_text(lang));
            }
            draft.short_venue = Some(venue);
            draft.stage = OpenStage::Amount;
        }
        self.store_draft(user_id, draft.clone());
        self.open_prompt(state, lang, &draft)
    }

    /// 校验行情后写入待确认订单，并回复确认/取消按钮。
    fn create_pending(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        symbol: &str,
        long_venue: &str,
        short_venue: &str,
        notional_usd: Decimal,
        leverage: Decimal,
    ) -> BotReply {
        for venue in [long_venue, short_venue] {
            if state.market.quote(venue, symbol).is_none() {
                return BotReply::text(match lang {
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
                return BotReply::text(
                    pick(
                        lang,
                        "待确认订单过多，请等待旧订单过期或点击旧订单的取消按钮。",
                        "Too many pending orders. Wait for older ones to expire or tap their Cancel buttons.",
                    ),
                );
            }
            pending.insert(
                token.clone(),
                PendingTrade {
                    owner: user_id,
                    symbol: symbol.to_string(),
                    long_venue: long_venue.to_string(),
                    short_venue: short_venue.to_string(),
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
        BotReply {
            text,
            markup: Some(markup),
        }
    }

    /// 向导进度提示：按当前阶段告诉用户下一步该做什么。
    fn open_prompt(&self, state: &AppState, lang: Lang, draft: &OpenDraft) -> BotReply {
        let venues = tradable_venues(state);
        let symbol = draft.symbol.clone().unwrap_or_default();
        match draft.stage {
            OpenStage::Symbol => BotReply::with_markup(
                pick(
                    lang,
                    "开仓第 1 步：发送交易对，例如 BTCUSDT（仅支持 USDT 本位永续）。",
                    "Entry step 1: send a symbol such as BTCUSDT (USDT-margined perpetuals only).",
                ),
                cancel_markup(lang),
            ),
            OpenStage::Long => {
                let text = match lang {
                    Lang::Zh => format!("开仓 {symbol}\n第 2 步：选择多头交易所。"),
                    Lang::En => format!("Entry {symbol}\nStep 2: choose the long venue."),
                };
                BotReply::with_markup(text, venue_markup(lang, "long", &venues, true))
            }
            OpenStage::Short => {
                let long = draft.long_venue.clone().unwrap_or_default();
                let options: Vec<String> = venues
                    .iter()
                    .filter(|venue| venue.as_str() != long)
                    .cloned()
                    .collect();
                let text = match lang {
                    Lang::Zh => format!(
                        "开仓 {symbol}\n多头 {long}\n第 3 步：选择空头交易所（不能与多头相同）。"
                    ),
                    Lang::En => format!(
                        "Entry {symbol}\nLong {long}\nStep 3: choose the short venue (must differ from the long leg)."
                    ),
                };
                BotReply::with_markup(text, venue_markup(lang, "short", &options, true))
            }
            OpenStage::Amount => {
                let long = draft.long_venue.clone().unwrap_or_default();
                let short = draft.short_venue.clone().unwrap_or_default();
                let text = match lang {
                    Lang::Zh => format!(
                        "开仓 {symbol}\n多头 {long} / 空头 {short}\n\n第 4 步：发送每条腿的 USD 金额。\n如需自定义杠杆，发送「金额 杠杆」，例如 500 10；只发金额时杠杆默认 {}x。",
                        DEFAULT_LEVERAGE
                    ),
                    Lang::En => format!(
                        "Entry {symbol}\nLong {long} / short {short}\n\nStep 4: send the USD amount per leg.\nTo set leverage, send \"<amount> <leverage>\", e.g. 500 10; amount alone uses the default {}x.",
                        DEFAULT_LEVERAGE
                    ),
                };
                BotReply::with_markup(text, cancel_markup(lang))
            }
        }
    }

    /// 向导里的交易所按钮回调。
    fn open_step(&self, state: &AppState, user_id: i64, lang: Lang, step: OpenStep) -> BotReply {
        match step {
            OpenStep::Cancel => {
                self.clear_draft(user_id);
                BotReply::text(pick(lang, "已取消开仓。", "Entry cancelled."))
            }
            OpenStep::Long(venue) => {
                let Some(draft) = self.current_draft(user_id) else {
                    return BotReply::text(expired_open_text(lang));
                };
                if draft.stage != OpenStage::Long {
                    return BotReply::text(expired_open_text(lang));
                }
                if !tradable_venues(state).contains(&venue) {
                    return BotReply::text(unknown_venue_text(lang, &venue));
                }
                let mut draft = draft;
                draft.long_venue = Some(venue);
                draft.stage = OpenStage::Short;
                draft.expires_at = draft_expiry();
                self.store_draft(user_id, draft.clone());
                self.open_prompt(state, lang, &draft)
            }
            OpenStep::Short(venue) => {
                let Some(draft) = self.current_draft(user_id) else {
                    return BotReply::text(expired_open_text(lang));
                };
                if draft.stage != OpenStage::Short {
                    return BotReply::text(expired_open_text(lang));
                }
                if !tradable_venues(state).contains(&venue) {
                    return BotReply::text(unknown_venue_text(lang, &venue));
                }
                if draft.long_venue.as_ref() == Some(&venue) {
                    return BotReply::text(same_venue_text(lang));
                }
                let mut draft = draft;
                draft.short_venue = Some(venue);
                draft.stage = OpenStage::Amount;
                draft.expires_at = draft_expiry();
                self.store_draft(user_id, draft.clone());
                self.open_prompt(state, lang, &draft)
            }
        }
    }

    /// 向导进行中收到的普通文本：当前阶段是输入交易对还是金额/杠杆。
    fn open_input(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        text: &str,
    ) -> Option<BotReply> {
        if !self.can_trade(user_id) {
            return None;
        }
        let draft = self.current_draft(user_id)?;
        match draft.stage {
            OpenStage::Symbol => match parse_symbol(text) {
                Some(symbol) => {
                    let mut draft = draft;
                    draft.symbol = Some(symbol);
                    draft.stage = OpenStage::Long;
                    draft.expires_at = draft_expiry();
                    self.store_draft(user_id, draft.clone());
                    Some(self.open_prompt(state, lang, &draft))
                }
                None => Some(BotReply::text(symbol_error_text(lang))),
            },
            OpenStage::Amount => {
                let Some((notional_usd, leverage)) = parse_amount_input(text) else {
                    return Some(BotReply::text(amount_error_text(lang)));
                };
                let (Some(symbol), Some(long_venue), Some(short_venue)) = (
                    draft.symbol.clone(),
                    draft.long_venue.clone(),
                    draft.short_venue.clone(),
                ) else {
                    self.clear_draft(user_id);
                    return Some(BotReply::text(expired_open_text(lang)));
                };
                self.clear_draft(user_id);
                Some(self.create_pending(
                    state,
                    user_id,
                    lang,
                    &symbol,
                    &long_venue,
                    &short_venue,
                    notional_usd,
                    leverage,
                ))
            }
            OpenStage::Long | OpenStage::Short => Some(BotReply::text(pick(
                lang,
                "请点击上方按钮选择交易所，或重新发送 /open 开始新的开仓。",
                "Tap a venue button above, or send /open to restart the entry.",
            ))),
        }
    }

    fn store_draft(&self, user_id: i64, draft: OpenDraft) {
        self.drafts
            .lock()
            .expect("开仓向导锁不可用")
            .insert(user_id, draft);
    }

    fn current_draft(&self, user_id: i64) -> Option<OpenDraft> {
        let mut drafts = self.drafts.lock().expect("开仓向导锁不可用");
        if matches!(drafts.get(&user_id), Some(draft) if draft.expires_at <= Utc::now()) {
            drafts.remove(&user_id);
            return None;
        }
        drafts.get(&user_id).cloned()
    }

    fn clear_draft(&self, user_id: i64) {
        self.drafts
            .lock()
            .expect("开仓向导锁不可用")
            .remove(&user_id);
    }

    /// 暂停或恢复开仓；只有具备交易权限的账号才能切换。
    fn toggle_pause(&self, state: &AppState, user_id: i64, lang: Lang, paused: bool) -> String {
        if !self.can_trade(user_id) {
            return pick(
                lang,
                "该账号没有交易权限。",
                "This account has no trading permission.",
            )
            .to_string();
        }
        if state.is_paused() == paused {
            return pick(
                lang,
                if paused {
                    "开仓已处于暂停状态。"
                } else {
                    "开仓正在运行。"
                },
                if paused {
                    "Entries are already paused."
                } else {
                    "Entries are already active."
                },
            )
            .to_string();
        }
        state.set_paused(paused, None);
        pick(
            lang,
            if paused {
                "已暂停开仓。"
            } else {
                "已恢复开仓。"
            },
            if paused {
                "Entries paused."
            } else {
                "Entries resumed."
            },
        )
        .to_string()
    }

    async fn confirm(&self, state: &AppState, user_id: i64, lang: Lang, token: &str) -> String {
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
                leverage: Some(trade.leverage),
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

    /// 用户点了交易所按钮：给出带金额与止盈止损的确认卡片。
    async fn listing_preview(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        id: &str,
        venue: &str,
    ) -> BotReply {
        let id = id.to_ascii_uppercase();
        let card = {
            let mut cards = self.listing_cards.lock().expect("上币卡片锁不可用");
            cards.retain(|_, card| card.expires_at > Utc::now());
            cards.get(&id).cloned()
        };
        let Some(card) = card else {
            return BotReply::text(listing_expired_text(lang));
        };
        let venue = venue.trim().to_ascii_lowercase();
        let settings = listing::manual_settings(state).await;
        if !listing_venues(state, &settings).contains(&venue) {
            return BotReply::text(unknown_venue_text(lang, &venue));
        }
        self.listing_drafts
            .lock()
            .expect("上币草稿锁不可用")
            .insert(
                user_id,
                ListingDraft {
                    id: id.clone(),
                    announcement: card.announcement.clone(),
                    venue: venue.clone(),
                    expires_at: listing_expiry(),
                },
            );
        BotReply::with_markup(
            listing_confirm_text(lang, &card.announcement, &venue, &settings),
            listing_confirm_markup(lang, &id),
        )
    }

    /// 用户点了「确认开仓」：按设置里的金额、杠杆与止盈止损单腿做多。
    async fn confirm_listing(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        id: &str,
    ) -> BotReply {
        let id = id.to_ascii_uppercase();
        let draft = self
            .listing_drafts
            .lock()
            .expect("上币草稿锁不可用")
            .remove(&user_id);
        let Some(draft) = draft.filter(|draft| draft.id == id && draft.expires_at > Utc::now())
        else {
            return BotReply::text(listing_expired_text(lang));
        };

        let settings = listing::manual_settings(state).await;
        let request = OpenListingRequest {
            token: draft.announcement.token,
            headline: draft.announcement.headline,
            venue: draft.venue,
            symbol: draft.announcement.symbol,
            side: Side::Buy,
            notional_usd: settings.notional_usd,
            leverage: settings.leverage,
            take_profit_pct: settings.take_profit_pct,
            stop_loss_pct: settings.stop_loss_pct,
            // 电报里手动确认的下单没有运行中的规则触发，仓位来源按手动处理。
            strategy_id: None,
            rule_id: None,
            rule_name: None,
            // 电报里手动确认的下单是真实委托，不受规则下单模式影响。
            paper: false,
            skill: false,
            max_hold_secs: 0,
            notify: true,
        };
        match listing::open_position(state, request).await {
            Ok(trade) => BotReply::text(listing_opened_text(lang, &trade)),
            Err(error) => BotReply::text(match lang {
                Lang::Zh => format!("下单失败：{}", describe_error(lang, &error)),
                Lang::En => format!("Order failed: {}", describe_error(lang, &error)),
            }),
        }
    }

    fn cancel_listing(&self, user_id: i64, lang: Lang, id: &str) -> BotReply {
        let id = id.to_ascii_uppercase();
        let removed = self
            .listing_drafts
            .lock()
            .expect("上币草稿锁不可用")
            .remove(&user_id);
        match removed {
            Some(draft) if draft.id == id => {
                BotReply::text(pick(lang, "已取消该上币订单。", "Listing order cancelled."))
            }
            _ => BotReply::text(pick(
                lang,
                "没有待确认的上币订单。",
                "No pending listing order.",
            )),
        }
    }

    /// `/listing`：查看与修改上币单腿交易；下单与平仓仍走统一风控网关。
    async fn listing_command(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        args: &[&str],
    ) -> BotReply {
        let Some(sub) = args.first().map(|value| value.to_ascii_lowercase()) else {
            return BotReply::text(listing_status_text(lang, state).await);
        };
        let value = args.get(1).copied();
        match sub.as_str() {
            "amount" | "leverage" | "tp" | "sl" => {
                if !self.can_trade(user_id) {
                    return BotReply::text(listing_permission_text(lang));
                }
                let Some(number) = value.and_then(|value| value.parse::<Decimal>().ok()) else {
                    return BotReply::text(listing_number_error_text(lang));
                };
                let requires_positive = matches!(sub.as_str(), "amount" | "leverage");
                if number < Decimal::ZERO || (requires_positive && number == Decimal::ZERO) {
                    return BotReply::text(listing_number_error_text(lang));
                }
                let strategies = listing::running_strategies(state).await;
                if strategies.is_empty() {
                    return BotReply::text(listing_no_running_text(lang));
                }
                // 参数写到运行中策略的每一条规则上；技能接管的策略写技能设置，
                // 否则状态仍会读到旧的上币监听设置，出现「显示模拟、实际真实」的偏差。
                for (strategy_id, _, mut settings) in strategies {
                    if skill::update_listing_number(state, strategy_id, sub.as_str(), number).await
                    {
                        continue;
                    }
                    for rule in &mut settings.rules {
                        match sub.as_str() {
                            "amount" => rule.notional_usd = number,
                            "leverage" => rule.leverage = number,
                            "tp" => rule.take_profit_pct = number,
                            _ => rule.stop_loss_pct = number,
                        }
                    }
                    state.listing.update_settings(strategy_id, settings).await;
                }
                BotReply::text(listing_status_text(lang, state).await)
            }
            "open" => {
                if !self.can_trade(user_id) {
                    return BotReply::text(listing_permission_text(lang));
                }
                let Some(token) = args
                    .get(1)
                    .map(|value| value.trim().to_ascii_uppercase())
                    .filter(|token| token.len() >= 2 && token.len() <= 12)
                    .filter(|token| token.chars().all(|ch| ch.is_ascii_alphanumeric()))
                else {
                    return BotReply::text(listing_usage_text(lang));
                };
                let Some(venue) = args.get(2).map(|value| value.to_ascii_lowercase()) else {
                    return BotReply::text(listing_usage_text(lang));
                };
                let symbol = args
                    .get(3)
                    .map(|value| value.trim().to_ascii_uppercase())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| format!("{token}USDT"));

                let id = listing_card_id();
                let announcement = ListingAnnouncement {
                    token,
                    symbol,
                    headline: pick(
                        lang,
                        "手动发起的上币单腿交易",
                        "Manually started listing long",
                    )
                    .to_string(),
                };
                {
                    let mut cards = self.listing_cards.lock().expect("上币卡片锁不可用");
                    cards.retain(|_, card| card.expires_at > Utc::now());
                    cards.insert(
                        id.clone(),
                        ListingCard {
                            announcement,
                            expires_at: listing_expiry(),
                        },
                    );
                }
                self.listing_preview(state, user_id, lang, &id, &venue)
                    .await
            }
            "close" => {
                if !self.can_trade(user_id) {
                    return BotReply::text(listing_permission_text(lang));
                }
                let Some(prefix) = args.get(1) else {
                    return BotReply::text(listing_usage_text(lang));
                };
                let Some(trade) = state.listing.find_open_by_prefix(prefix) else {
                    return BotReply::text(pick(
                        lang,
                        "没有找到匹配的持仓。",
                        "No matching open trade.",
                    ));
                };
                match listing::close_position(state, trade.id, CloseReason::Manual).await {
                    Ok(closed) => BotReply::text(listing_closed_text(lang, &closed)),
                    Err(error) => BotReply::text(match lang {
                        Lang::Zh => format!("平仓失败：{}", describe_error(lang, &error)),
                        Lang::En => format!("Close failed: {}", describe_error(lang, &error)),
                    }),
                }
            }
            _ => BotReply::text(listing_status_text(lang, state).await),
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

    /// 用处理结果覆盖原消息；`markup` 为空时清空内联按钮，避免重复点击。
    async fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        markup: Option<&Value>,
    ) -> anyhow::Result<()> {
        let text: String = text.chars().take(3900).collect();
        let markup = markup
            .map(|value| value.to_string())
            .unwrap_or_else(|| r#"{"inline_keyboard":[]}"#.to_string());
        self.api(
            "editMessageText",
            &[
                ("chat_id", chat_id.to_string()),
                ("message_id", message_id.to_string()),
                ("text", text),
                ("reply_markup", markup),
            ],
        )
        .await?;
        Ok(())
    }

    async fn api(&self, method: &str, params: &[(&str, String)]) -> anyhow::Result<Value> {
        let query = params
            .iter()
            .map(|(key, value)| format!("{key}={}", encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.request(method, &query, None).await
    }

    /// 需要 JSON 请求体的接口（如 `setMyCommands`）。
    async fn api_post(&self, method: &str, payload: &Value) -> anyhow::Result<Value> {
        self.request(method, "", Some(payload.clone())).await
    }

    async fn request(
        &self,
        method: &str,
        query: &str,
        payload: Option<Value>,
    ) -> anyhow::Result<Value> {
        let Some(http) = self.http.clone() else {
            bail!("HTTP 客户端不可用");
        };
        let Some(token) = self.token.clone() else {
            bail!("未配置 Bot Token");
        };
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

        let value = match payload {
            Some(payload) => tokio::task::spawn_blocking(move || http.post_json(&url, &payload))
                .await
                .context("Telegram 请求任务失败")??,
            None => tokio::task::spawn_blocking(move || http.get_json(&url))
                .await
                .context("Telegram 请求任务失败")??,
        };
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
            "/open 开仓向导：按钮选交易所，输入金额与杠杆（默认 10x）",
            "开仓会回复确认按钮，点击「确认开仓」后才真正下单",
            "/listing 上币单腿交易：金额、杠杆、止盈止损与手动平仓",
            "/listing amount 100 / leverage 10 调整下单参数",
            "/listing open <代币> <交易所> [交易对] 手动发起",
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
            "/pause Pause entries",
            "/resume Resume entries",
            "/open Entry wizard: pick venues with buttons, then type the amount and leverage (default 10x)",
            "Entries reply with a confirm button; tap it to place the order",
            "/listing Listing single-leg trades: notional, leverage, TP/SL and manual close",
            "/listing amount 100 / leverage 10 adjusts the order size and leverage",
            "/listing open <token> <venue> [symbol] starts one manually",
            "/lang zh|en Switch the bot language",
            "/unbind Unlink this account",
            "",
            "API keys stay on the machine running the executor; the bot never receives them.",
        ]
        .join("\n"),
    }
}

/// 可下单的交易所：已导入账户且凭证齐全，按名称排序去重。
fn tradable_venues(state: &AppState) -> Vec<String> {
    let mut venues: Vec<String> = state
        .accounts
        .list()
        .into_iter()
        .filter(|account| account.can_trade)
        .map(|account| account.venue)
        .collect();
    venues.sort();
    venues.dedup();
    venues
}

/// 上币单腿交易可选交易所：按规则里的执行优先级排列，只保留当前可下单的交易所。
fn listing_venues(state: &AppState, rule: &ListingRule) -> Vec<String> {
    let tradable = tradable_venues(state);
    let priority = &rule.execution_venues;
    if priority.is_empty() {
        return tradable;
    }
    priority
        .iter()
        .filter(|venue| tradable.contains(venue))
        .cloned()
        .collect()
}

/// 交易对统一为大写，仅接受 USDT 本位永续（形如 `BTCUSDT`）。
fn parse_symbol(value: &str) -> Option<String> {
    let symbol = value.trim().to_ascii_uppercase();
    if symbol.len() <= 4 || !symbol.ends_with("USDT") {
        return None;
    }
    if !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some(symbol)
}

/// 解析金额输入：`500` 或 `500 10`；只给金额时使用默认杠杆。
fn parse_amount_input(text: &str) -> Option<(Decimal, Decimal)> {
    let mut parts = text.split_whitespace();
    let notional_usd = parts.next()?.parse::<Decimal>().ok()?;
    if notional_usd <= Decimal::ZERO {
        return None;
    }
    let leverage = match parts.next() {
        Some(value) => {
            let leverage = value.parse::<Decimal>().ok()?;
            if leverage <= Decimal::ZERO {
                return None;
            }
            leverage
        }
        None => DEFAULT_LEVERAGE,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((notional_usd, leverage))
}

fn draft_expiry() -> DateTime<Utc> {
    Utc::now() + chrono::Duration::seconds(PENDING_TTL_SECS)
}

/// 交易所按钮：每行两个，`step` 为 `long` 或 `short`。
fn venue_markup(lang: Lang, step: &str, venues: &[String], cancel: bool) -> Value {
    let mut rows: Vec<Vec<Value>> = venues
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|venue| {
                    serde_json::json!({
                        "text": venue,
                        "callback_data": format!("open:{step}:{venue}"),
                    })
                })
                .collect()
        })
        .collect();
    if cancel {
        rows.push(cancel_row(lang));
    }
    serde_json::json!({ "inline_keyboard": rows })
}

fn cancel_markup(lang: Lang) -> Value {
    serde_json::json!({ "inline_keyboard": [cancel_row(lang)] })
}

fn cancel_row(lang: Lang) -> Vec<Value> {
    vec![serde_json::json!({
        "text": pick(lang, "取消", "Cancel"),
        "callback_data": "open:cancel",
    })]
}

fn symbol_error_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "交易对格式不正确，请发送形如 BTCUSDT 的交易对。",
        "Invalid symbol. Send a symbol such as BTCUSDT.",
    )
}

fn amount_error_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "金额或杠杆不正确。请发送「金额」或「金额 杠杆」，例如 500 10。",
        "Invalid amount or leverage. Send \"<amount>\" or \"<amount> <leverage>\", e.g. 500 10.",
    )
}

fn wizard_only_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "金额与杠杆在向导最后一步输入。请发送 /open <交易对> [多头] [空头] 开始。",
        "Amount and leverage are entered in the last wizard step. Send /open <symbol> [long] [short] to start.",
    )
}

fn same_venue_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "双腿交易所不能相同。",
        "The two legs cannot use the same venue.",
    )
}

fn expired_open_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "开仓流程已过期，请重新发送 /open 开始。",
        "The entry wizard expired. Send /open to start again.",
    )
}

fn unknown_venue_text(lang: Lang, venue: &str) -> String {
    match lang {
        Lang::Zh => format!("没有该交易所的可交易账户：{venue}，请用按钮选择。"),
        Lang::En => format!("No tradable account for venue {venue}; pick one of the buttons."),
    }
}

fn listing_card_id() -> String {
    Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase()
}

fn listing_expiry() -> DateTime<Utc> {
    Utc::now() + chrono::Duration::seconds(LISTING_TTL_SECS)
}

/// 止盈止损为 0 表示不启用，展示成「关闭」而不是 0%。
fn pct_label(lang: Lang, value: Decimal) -> String {
    if value > Decimal::ZERO {
        format!("{}%", value.round_dp(2))
    } else {
        pick(lang, "关闭", "off").to_string()
    }
}

fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn listing_confirm_text(
    lang: Lang,
    announcement: &ListingAnnouncement,
    venue: &str,
    settings: &ListingRule,
) -> String {
    match lang {
        Lang::Zh => format!(
            "确认上币单腿开仓\n{} · {}\n市价买入 {} USD，杠杆 {}x\n止盈 {} · 止损 {}\n\n点击「确认开仓」后才会下单；成交后由后台按标记价监控止盈止损。",
            announcement.symbol,
            venue,
            settings.notional_usd.round_dp(2),
            settings.leverage.round_dp(2),
            pct_label(lang, settings.take_profit_pct),
            pct_label(lang, settings.stop_loss_pct),
        ),
        Lang::En => format!(
            "Confirm the listing long\n{} · {}\nMarket buy {} USD, leverage {}x\nTP {} · SL {}\n\nThe order is placed only after you tap Confirm; once filled, the executor watches mark price for TP/SL.",
            announcement.symbol,
            venue,
            settings.notional_usd.round_dp(2),
            settings.leverage.round_dp(2),
            pct_label(lang, settings.take_profit_pct),
            pct_label(lang, settings.stop_loss_pct),
        ),
    }
}

fn listing_confirm_markup(lang: Lang, id: &str) -> Value {
    serde_json::json!({
        "inline_keyboard": [[
            {
                "text": pick(lang, "确认开仓", "Confirm entry"),
                "callback_data": format!("listing:confirm:{id}"),
            },
            {
                "text": pick(lang, "取消", "Cancel"),
                "callback_data": format!("listing:cancel:{id}"),
            },
        ]],
    })
}

async fn listing_status_text(lang: Lang, state: &AppState) -> String {
    let rules = listing::running_strategies(state).await;
    let trades = state.listing.open_trades();
    let mut lines = Vec::new();
    if rules.is_empty() {
        lines.push(listing_no_running_text(lang).to_string());
    }
    for (_, name, settings) in &rules {
        for (index, rule) in settings.rules.iter().enumerate() {
            // 每条规则独立选择真实或模拟下单，状态里逐条标注模拟模式。
            let mode_suffix = if rule.execution_mode == StrategyExecutionMode::Paper {
                pick(lang, "（模拟）", " (paper)")
            } else {
                ""
            };
            let rule_name = if rule.name.is_empty() {
                format!("{} {}", pick(lang, "规则", "Rule"), index + 1)
            } else {
                rule.name.clone()
            };
            // 单独关掉的规则仍列出来，但标注为已关闭，避免误以为还在跟随。
            let rule_name = if rule.enabled {
                rule_name
            } else {
                format!("{rule_name}（{}）", pick(lang, "已关闭", "off"))
            };
            let watch = if rule.watch_venues.is_empty() {
                pick(lang, "全部来源", "all sources").to_string()
            } else {
                rule.watch_venues.join("/")
            };
            lines.push(match lang {
                Lang::Zh => format!(
                    "{name} · {rule_name}（{watch}）{mode_suffix}：{notional} USD · 杠杆 {leverage}x · 止盈 {tp} · 止损 {sl}",
                    notional = rule.notional_usd.round_dp(2),
                    leverage = rule.leverage.round_dp(2),
                    tp = pct_label(lang, rule.take_profit_pct),
                    sl = pct_label(lang, rule.stop_loss_pct),
                ),
                Lang::En => format!(
                    "{name} · {rule_name} ({watch}){mode_suffix}: {notional} USD · leverage {leverage}x · TP {tp} · SL {sl}",
                    notional = rule.notional_usd.round_dp(2),
                    leverage = rule.leverage.round_dp(2),
                    tp = pct_label(lang, rule.take_profit_pct),
                    sl = pct_label(lang, rule.stop_loss_pct),
                ),
            });
        }
    }
    lines.push(match lang {
        Lang::Zh => format!("持仓 {} 笔", trades.len()),
        Lang::En => format!("Open trades: {}", trades.len()),
    });
    for trade in trades.iter().take(10) {
        lines.push(match lang {
            Lang::Zh => format!(
                "#{} {} {} 入场 {}（止盈 {} / 止损 {}）",
                short_id(trade.id),
                trade.symbol,
                trade.venue,
                trade.entry_price.round_dp(6),
                pct_label(lang, trade.take_profit_pct),
                pct_label(lang, trade.stop_loss_pct),
            ),
            Lang::En => format!(
                "#{} {} {} entry {} (TP {} / SL {})",
                short_id(trade.id),
                trade.symbol,
                trade.venue,
                trade.entry_price.round_dp(6),
                pct_label(lang, trade.take_profit_pct),
                pct_label(lang, trade.stop_loss_pct),
            ),
        });
    }
    lines.push(String::new());
    lines.extend(
        match lang {
            Lang::Zh => [
                "/listing amount 100 设置单笔金额",
                "/listing leverage 1 设置杠杆（默认取风控杠杆上限）",
                "/listing tp 20 / sl 10 设置止盈止损，0 关闭",
                "/listing open <代币> <交易所> [交易对] 手动发起",
                "/listing close <编号前缀> 手动平仓",
            ],
            Lang::En => [
                "/listing amount 100 sets the order size",
                "/listing leverage 1 sets leverage (defaults to the risk leverage cap)",
                "/listing tp 20 / sl 10 sets TP/SL, 0 disables",
                "/listing open <token> <venue> [symbol] starts one manually",
                "/listing close <id prefix> closes manually",
            ],
        }
        .iter()
        .map(|line| line.to_string()),
    );
    lines.join("\n")
}

fn listing_opened_text(lang: Lang, trade: &ListingTrade) -> String {
    match lang {
        Lang::Zh => format!(
            "已开仓 #{}\n{} {} @ {}\n金额 {} USD · 杠杆 {}x\n止盈 {} · 止损 {}",
            short_id(trade.id),
            trade.symbol,
            trade.venue,
            trade.entry_price.round_dp(6),
            trade.notional_usd.round_dp(2),
            trade.leverage.round_dp(2),
            pct_label(lang, trade.take_profit_pct),
            pct_label(lang, trade.stop_loss_pct),
        ),
        Lang::En => format!(
            "Opened #{}\n{} {} @ {}\n{} USD · leverage {}x\nTP {} · SL {}",
            short_id(trade.id),
            trade.symbol,
            trade.venue,
            trade.entry_price.round_dp(6),
            trade.notional_usd.round_dp(2),
            trade.leverage.round_dp(2),
            pct_label(lang, trade.take_profit_pct),
            pct_label(lang, trade.stop_loss_pct),
        ),
    }
}

fn listing_closed_text(lang: Lang, trade: &ListingTrade) -> String {
    let price = trade.close_price.unwrap_or(trade.entry_price);
    match lang {
        Lang::Zh => format!(
            "已平仓 #{}\n{} {} @ {}（入场 {}）",
            short_id(trade.id),
            trade.symbol,
            trade.venue,
            price.round_dp(6),
            trade.entry_price.round_dp(6),
        ),
        Lang::En => format!(
            "Closed #{}\n{} {} @ {} (entry {})",
            short_id(trade.id),
            trade.symbol,
            trade.venue,
            price.round_dp(6),
            trade.entry_price.round_dp(6),
        ),
    }
}

fn listing_expired_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "上币卡片已过期，请重新扫描公告或用 /listing open 手动发起。",
        "The listing card expired. Wait for a new announcement or use /listing open.",
    )
}

fn listing_permission_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "该账号没有交易权限。",
        "This account has no trading permission.",
    )
}

fn listing_usage_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "用法：/listing open <代币> <交易所> [交易对]，例如 /listing open XYZ bitget XYZUSDT。",
        "Usage: /listing open <token> <venue> [symbol], e.g. /listing open XYZ bitget XYZUSDT.",
    )
}

fn listing_number_error_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "数值不正确：金额与杠杆需大于 0，止盈止损可为 0 表示关闭。",
        "Invalid value: amount and leverage must be positive; TP/SL accept 0 to disable.",
    )
}

/// 没有运行中的上币公告跟随策略时的提示。
fn listing_no_running_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "当前没有运行中的上币公告跟随策略，请先在桌面端策略页启动。",
        "No listing-follow strategy is running; start one on the desktop strategy page first.",
    )
}

/// Telegram 输入框旁的命令菜单内容。
fn command_menu(lang: Lang) -> Value {
    let items: &[(&str, &str)] = match lang {
        Lang::Zh => &[
            ("status", "执行器状态"),
            ("positions", "当前仓位"),
            ("orders", "最近订单"),
            ("news", "最新上币公告"),
            ("pause", "暂停开仓"),
            ("resume", "恢复开仓"),
            ("open", "发起双腿开仓（需二次确认）"),
            ("listing", "上币单腿交易设置与持仓"),
            ("lang", "切换机器人语言"),
            ("unbind", "解除绑定"),
        ],
        Lang::En => &[
            ("status", "Executor status"),
            ("positions", "Current positions"),
            ("orders", "Recent orders"),
            ("news", "Latest listing announcements"),
            ("pause", "Pause entries"),
            ("resume", "Resume entries"),
            ("open", "Prepare a hedged entry (needs confirmation)"),
            ("listing", "Listing single-leg settings and trades"),
            ("lang", "Switch the bot language"),
            ("unbind", "Unlink this account"),
        ],
    };
    Value::Array(
        items
            .iter()
            .map(|(command, description)| {
                serde_json::json!({ "command": command, "description": description })
            })
            .collect(),
    )
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
        ApiError::SkillValidation(report) => match lang {
            Lang::Zh => format!("技能参数未通过校验（{} 项错误）", report.errors),
            Lang::En => format!(
                "Skill parameters failed validation ({} errors)",
                report.errors
            ),
        },
    }
}

/// 解析内联按钮的 `callback_data`：
/// `confirm:<编号>` / `cancel:<编号>` 对应待确认订单，
/// `open:<步骤>` 是开仓向导的交易所选择。
fn parse_callback(data: &str) -> Option<Callback> {
    let (action, value) = data.split_once(':')?;
    if value.is_empty() {
        return None;
    }
    match action {
        "confirm" => Some(Callback::Confirm(value.to_string())),
        "cancel" => Some(Callback::Cancel(value.to_string())),
        "open" => match value {
            "cancel" => Some(Callback::Open(OpenStep::Cancel)),
            _ => {
                let (step, venue) = value.split_once(':')?;
                if venue.is_empty() {
                    return None;
                }
                match step {
                    "long" => Some(Callback::Open(OpenStep::Long(venue.to_string()))),
                    "short" => Some(Callback::Open(OpenStep::Short(venue.to_string()))),
                    _ => None,
                }
            }
        },
        "listing" => {
            let (step, rest) = value.split_once(':')?;
            match step {
                "venue" => {
                    let (id, venue) = rest.split_once(':')?;
                    if id.is_empty() || venue.is_empty() {
                        return None;
                    }
                    Some(Callback::Listing(ListingStep::Venue {
                        id: id.to_string(),
                        venue: venue.to_string(),
                    }))
                }
                "confirm" => Some(Callback::Listing(ListingStep::Confirm(rest.to_string()))),
                "cancel" => Some(Callback::Listing(ListingStep::Cancel(rest.to_string()))),
                _ => None,
            }
        }
        _ => None,
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
        assert!(!text.contains("/menu"));
        assert!(text.contains("/pause"));
        assert!(text.contains("/resume"));
        assert!(text.contains("/open"));
        assert!(text.contains("确认按钮"));
        assert!(!text.contains("/confirm"));
        assert!(!text.contains("/cancel"));
        assert!(text.contains("/lang"));
        assert!(!text.contains("尚未配置"));
    }

    #[test]
    fn command_menu_lists_trade_commands() {
        for lang in [Lang::Zh, Lang::En] {
            let menu = command_menu(lang);
            let commands = menu.as_array().expect("命令菜单应为数组");
            assert!(!commands.iter().any(|item| item["command"] == "menu"));
            assert!(commands.iter().any(|item| item["command"] == "open"));
            assert!(commands.iter().any(|item| item["command"] == "pause"));
            assert!(commands.iter().any(|item| item["command"] == "resume"));
            assert!(commands
                .iter()
                .all(|item| item["description"].as_str().is_some()));
        }
    }

    #[test]
    fn parses_callback_data() {
        assert!(matches!(
            parse_callback("confirm:ABC123"),
            Some(Callback::Confirm(token)) if token == "ABC123"
        ));
        assert!(matches!(
            parse_callback("cancel:ABC123"),
            Some(Callback::Cancel(token)) if token == "ABC123"
        ));
        assert_eq!(parse_callback("confirm:"), None);
        assert_eq!(parse_callback("delete:ABC123"), None);
        assert_eq!(parse_callback("panel:status"), None);
        assert_eq!(parse_callback("menu"), None);
        assert_eq!(parse_callback("ABC123"), None);
    }

    #[test]
    fn parses_open_callbacks() {
        assert_eq!(
            parse_callback("open:long:binance"),
            Some(Callback::Open(OpenStep::Long("binance".to_string())))
        );
        assert_eq!(
            parse_callback("open:short:okx"),
            Some(Callback::Open(OpenStep::Short("okx".to_string())))
        );
        assert_eq!(
            parse_callback("open:cancel"),
            Some(Callback::Open(OpenStep::Cancel))
        );
        assert_eq!(parse_callback("open:long:"), None);
        assert_eq!(parse_callback("open:side:binance"), None);
    }

    #[test]
    fn parses_listing_callbacks() {
        assert_eq!(
            parse_callback("listing:venue:ABC123:bitget"),
            Some(Callback::Listing(ListingStep::Venue {
                id: "ABC123".to_string(),
                venue: "bitget".to_string(),
            }))
        );
        assert_eq!(
            parse_callback("listing:confirm:ABC123"),
            Some(Callback::Listing(ListingStep::Confirm(
                "ABC123".to_string()
            )))
        );
        assert_eq!(
            parse_callback("listing:cancel:ABC123"),
            Some(Callback::Listing(ListingStep::Cancel("ABC123".to_string())))
        );
        assert_eq!(parse_callback("listing:venue:ABC123"), None);
        assert_eq!(parse_callback("listing:venue::bitget"), None);
        assert_eq!(parse_callback("listing:unknown:ABC123"), None);
    }

    #[test]
    fn parses_symbol_and_amount_inputs() {
        assert_eq!(parse_symbol("btcusdt"), Some("BTCUSDT".to_string()));
        assert_eq!(parse_symbol(" BTCUSDT "), Some("BTCUSDT".to_string()));
        assert_eq!(parse_symbol("BTC-USDT"), None);
        assert_eq!(parse_symbol("BTC"), None);

        assert_eq!(
            parse_amount_input("500"),
            Some((Decimal::from(500), DEFAULT_LEVERAGE))
        );
        assert_eq!(
            parse_amount_input("500 5"),
            Some((Decimal::from(500), Decimal::from(5)))
        );
        assert_eq!(parse_amount_input("500 0"), None);
        assert_eq!(parse_amount_input("abc"), None);
        assert_eq!(parse_amount_input("500 5 1"), None);
    }

    #[test]
    fn venue_markup_links_open_steps() {
        let venues = vec![
            "binance".to_string(),
            "okx".to_string(),
            "bybit".to_string(),
        ];
        let markup = venue_markup(Lang::Zh, "long", &venues, true);
        let rows = markup["inline_keyboard"]
            .as_array()
            .expect("内联键盘应为数组");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0]["callback_data"], "open:long:binance");
        assert_eq!(rows[1][0]["text"], "bybit");
        assert_eq!(rows[2][0]["callback_data"], "open:cancel");
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
