use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{bail, Context};
use arb_core::{OrderOrigin, OrderStatus, Side};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    config::Config,
    error::ApiError,
    events::ExecutorEvent,
    gateway::{self, LegOrderRequest, OrderBatchRequest},
    http_client::HttpClient,
    listing::{
        self, CloseReason, ListingAnnouncement, ListingRule, ListingTrade, OpenListingRequest,
    },
    state::AppState,
};

/// 待确认订单的有效期。
const PENDING_TTL_SECS: i64 = 120;
/// 一键平仓确认卡片的有效期；过期后按钮失效，避免误点旧消息平掉新仓位。
const CLOSE_ALL_TTL_SECS: i64 = 120;
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

/// 待确认订单里的一条腿：交易所与方向。
#[derive(Debug, Clone)]
struct PendingLeg {
    venue: String,
    side: Side,
}

struct PendingTrade {
    owner: i64,
    symbol: String,
    /// 单腿或双腿；开仓时按顺序提交。
    legs: Vec<PendingLeg>,
    notional_usd: Decimal,
    leverage: Decimal,
    expires_at: DateTime<Utc>,
}

/// 开仓方式：双腿对冲，或者只做其中一条腿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    Hedge,
    LongOnly,
    ShortOnly,
}

/// 开仓向导当前在等用户做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenStage {
    /// 等用户发送交易对（`/open` 不带参数时）。
    Symbol,
    /// 等用户点按钮选开仓方式：双腿对冲或单腿。
    Mode,
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
    mode: Option<OpenMode>,
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

/// 开仓向导里的按钮动作：选开仓方式、选多头、选空头、取消。
#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenStep {
    Mode(OpenMode),
    Long(String),
    Short(String),
    Cancel,
}

/// 电报里的一条上币公告卡片，编号在按钮回调中回传。
#[derive(Debug, Clone)]
struct ListingCard {
    announcement: ListingAnnouncement,
    /// 开仓方向：手动发起时可指定做多或做空。
    side: Side,
    expires_at: DateTime<Utc>,
}

/// 用户已选交易所、等待确认的上币单腿订单。
#[derive(Debug, Clone)]
struct ListingDraft {
    /// 卡片编号，用于校验回调与草稿是否匹配。
    id: String,
    announcement: ListingAnnouncement,
    /// 开仓方向，确认下单时沿用到订单。
    side: Side,
    venue: String,
    expires_at: DateTime<Utc>,
}

/// 上币手动下单的引导阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListingStage {
    /// 等用户发送代币代码。
    Token,
    /// 等用户点按钮选择做多还是做空。
    Side,
    /// 等用户点按钮选择交易所。
    Venue,
    /// 首次手动下单：等用户发送金额、杠杆与止盈止损。
    Params,
}

/// 用户正在通过 `/listing open` 引导下单的草稿。
#[derive(Debug, Clone)]
struct ListingWizard {
    /// 卡片编号，回调时用于校验草稿。
    id: String,
    /// 已识别出的代币代码。
    token: Option<String>,
    /// 已选好的交易所；只有参数引导阶段会用到。
    venue: Option<String>,
    stage: ListingStage,
    expires_at: DateTime<Utc>,
}

/// 上币卡片上的按钮动作：选方向、选交易所、修改参数、确认开仓、取消。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListingStep {
    /// 放弃当前上币引导。
    Abort,
    Side {
        id: String,
        side: Side,
    },
    Venue {
        id: String,
        venue: String,
    },
    Params(String),
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
    /// 一键平仓：`token` 校验确认卡片，`confirm` 为 false 表示取消。
    CloseAll {
        token: String,
        confirm: bool,
    },
}

/// Telegram 机器人：配对绑定、查询、暂停/恢复与经确认的开仓。
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
    /// 每个用户进行中的上币下单引导。
    listing_wizards: Mutex<HashMap<i64, ListingWizard>>,
    /// 每个用户已选交易所、等待确认的上币单腿订单。
    listing_drafts: Mutex<HashMap<i64, ListingDraft>>,
    /// 待确认的一键平仓请求：编号 → (发起用户, 过期时间)。
    close_all: Mutex<HashMap<String, (i64, DateTime<Utc>)>>,
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
            listing_wizards: Mutex::new(HashMap::new()),
            listing_drafts: Mutex::new(HashMap::new()),
            close_all: Mutex::new(HashMap::new()),
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

        // 上币下单引导进行中时，用户下一条普通文本就是代币代码。
        if !text.starts_with('/') {
            if let Some(reply) = self.listing_input(state, user_id, lang, text).await {
                self.send_with_markup(chat_id, &reply.text, reply.markup.as_ref())
                    .await;
                return;
            }
        }

        let mut markup = None;
        let reply = match command.as_str() {
            // /menu、/panel 是旧版按钮面板留下的指令，Telegram 客户端的命令菜单
            // 会缓存一段时间，这里回落到帮助列表，避免用户点到后收到“未识别”。
            "/start" | "/help" | "/menu" | "/panel" => help_text(lang, self.enabled()),
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
                        "/closeall" => {
                            let prepared = self.prepare_close_all(state, user_id, lang);
                            markup = prepared.markup;
                            prepared.text
                        }
                        "/close" => self.close_one(state, user_id, lang, &args).await,
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
                    ListingStep::Abort => self.abort_listing(user_id, lang),
                    ListingStep::Side { id, side } => {
                        if self.can_trade(user_id) {
                            self.listing_side_step(state, user_id, lang, &id, side)
                                .await
                        } else {
                            BotReply::text(pick(
                                lang,
                                "该账号没有交易权限。",
                                "This account has no trading permission.",
                            ))
                        }
                    }
                    ListingStep::Venue { id, venue } => {
                        if self.can_trade(user_id) {
                            self.clear_listing_wizard(user_id);
                            self.listing_after_venue(state, user_id, lang, &id, &venue)
                                .await
                        } else {
                            BotReply::text(pick(
                                lang,
                                "该账号没有交易权限。",
                                "This account has no trading permission.",
                            ))
                        }
                    }
                    ListingStep::Params(id) => {
                        if self.can_trade(user_id) {
                            self.listing_params_step(state, user_id, lang, &id).await
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
                Some(Callback::CloseAll { token, confirm }) => {
                    if !confirm {
                        BotReply::text(self.cancel_close_all(user_id, lang, &token))
                    } else if self.can_trade(user_id) {
                        BotReply::text(self.confirm_close_all(state, user_id, lang, &token).await)
                    } else {
                        BotReply::text(pick(
                            lang,
                            "该账号没有交易权限。",
                            "This account has no trading permission.",
                        ))
                    }
                }
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

    /// `/open` 入口：交易对可随指令给出，开仓方式与交易所都用按钮选择，
    /// 金额和杠杆由用户输入。支持双腿对冲，也支持只开多或只开空。
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
        if args.len() > 1 {
            return BotReply::text(wizard_only_text(lang));
        }

        // 开始开仓向导时放弃可能残留的上币引导，避免用户下一条文本被另一套向导吃掉。
        self.clear_listing_wizard(user_id);
        let mut draft = OpenDraft {
            symbol: None,
            mode: None,
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
            draft.stage = OpenStage::Mode;
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
        legs: Vec<PendingLeg>,
        notional_usd: Decimal,
        leverage: Decimal,
    ) -> BotReply {
        for leg in &legs {
            if state.market.quote(&leg.venue, symbol).is_none() {
                return BotReply::text(match lang {
                    Lang::Zh => {
                        format!(
                            "{symbol} 在 {} 没有可用行情，无法下单。",
                            venue_label(&leg.venue)
                        )
                    }
                    Lang::En => {
                        format!(
                            "No market data for {symbol} on {}; cannot place the order.",
                            venue_label(&leg.venue)
                        )
                    }
                });
            }
        }

        let token = Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase();
        let expires_at = Utc::now() + chrono::Duration::seconds(PENDING_TTL_SECS);
        let legs_label = pending_legs_label(lang, &legs);
        let single_leg = legs.len() == 1;
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
                    legs,
                    notional_usd,
                    leverage,
                    expires_at,
                },
            );
        }

        let text = match lang {
            Lang::Zh => format!(
                "待确认 {token}\n{symbol}：{legs_label}\n{} {notional_usd} USD，杠杆 {leverage}x\n\n请点击下方按钮在 {PENDING_TTL_SECS} 秒内确认或取消。下单仍会经过执行器风控。",
                if single_leg { "单腿" } else { "每条腿" }
            ),
            Lang::En => format!(
                "Pending {token}\n{symbol}: {legs_label}\n{} {notional_usd} USD, leverage {leverage}x\n\nTap a button below within {PENDING_TTL_SECS}s to confirm or cancel. Risk checks still apply.",
                if single_leg { "Single leg" } else { "Per leg" }
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
        let hedge = draft.mode == Some(OpenMode::Hedge);
        match draft.stage {
            OpenStage::Symbol => BotReply::with_markup(
                pick(
                    lang,
                    "开仓第 1 步：发送交易对，例如 BTCUSDT（仅支持 USDT 本位永续）。",
                    "Entry step 1: send a symbol such as BTCUSDT (USDT-margined perpetuals only).",
                ),
                cancel_markup(lang),
            ),
            OpenStage::Mode => {
                let text = match lang {
                    Lang::Zh => format!(
                        "开仓 {symbol}\n第 2 步：选择开仓方式：双腿对冲，或只做多/只做空一条腿。"
                    ),
                    Lang::En => format!(
                        "Entry {symbol}\nStep 2: choose the entry mode: a hedged pair, or a single long/short leg."
                    ),
                };
                BotReply::with_markup(text, mode_markup(lang))
            }
            OpenStage::Long => {
                let text = match lang {
                    Lang::Zh => format!("开仓 {symbol}\n第 3 步：选择多头交易所。"),
                    Lang::En => format!("Entry {symbol}\nStep 3: choose the long venue."),
                };
                BotReply::with_markup(text, venue_markup(lang, "long", &venues, true))
            }
            OpenStage::Short => {
                let long = draft.long_venue.clone().unwrap_or_default();
                let long_label = venue_label(&long);
                let step = if hedge { 4 } else { 3 };
                let (options, text) = if hedge {
                    let options = venues
                        .iter()
                        .filter(|venue| venue.as_str() != long)
                        .cloned()
                        .collect();
                    let text = match lang {
                        Lang::Zh => format!(
                            "开仓 {symbol}\n多头 {long_label}\n第 {step} 步：选择空头交易所（不能与多头相同）。"
                        ),
                        Lang::En => format!(
                            "Entry {symbol}\nLong {long_label}\nStep {step}: choose the short venue (must differ from the long leg)."
                        ),
                    };
                    (options, text)
                } else {
                    let text = match lang {
                        Lang::Zh => {
                            format!("开仓 {symbol}\n第 {step} 步：选择做空交易所。")
                        }
                        Lang::En => {
                            format!("Entry {symbol}\nStep {step}: choose the short venue.")
                        }
                    };
                    (venues.clone(), text)
                };
                BotReply::with_markup(text, venue_markup(lang, "short", &options, true))
            }
            OpenStage::Amount => {
                let step = if hedge { 5 } else { 4 };
                let summary = draft_legs_summary(lang, draft);
                let amount_label = match lang {
                    Lang::Zh => {
                        if hedge {
                            "每条腿的 USD 金额"
                        } else {
                            "单腿的 USD 金额"
                        }
                    }
                    Lang::En => {
                        if hedge {
                            "the USD amount per leg"
                        } else {
                            "the USD amount for the single leg"
                        }
                    }
                };
                let text = match lang {
                    Lang::Zh => format!(
                        "开仓 {symbol}\n{summary}\n\n第 {step} 步：发送{amount_label}。\n如需自定义杠杆，发送「金额 杠杆」，例如 500 10；只发金额时杠杆默认 {}x。",
                        DEFAULT_LEVERAGE
                    ),
                    Lang::En => format!(
                        "Entry {symbol}\n{summary}\n\nStep {step}: send {amount_label}.\nTo set leverage, send \"<amount> <leverage>\", e.g. 500 10; amount alone uses the default {}x.",
                        DEFAULT_LEVERAGE
                    ),
                };
                BotReply::with_markup(text, cancel_markup(lang))
            }
        }
    }

    /// 向导里的按钮回调。
    fn open_step(&self, state: &AppState, user_id: i64, lang: Lang, step: OpenStep) -> BotReply {
        match step {
            OpenStep::Cancel => {
                self.clear_draft(user_id);
                BotReply::text(pick(lang, "已取消开仓。", "Entry cancelled."))
            }
            OpenStep::Mode(mode) => {
                let Some(draft) = self.current_draft(user_id) else {
                    return BotReply::text(expired_open_text(lang));
                };
                if draft.stage != OpenStage::Mode {
                    return BotReply::text(expired_open_text(lang));
                }
                let mut draft = draft;
                draft.mode = Some(mode);
                draft.stage = match mode {
                    OpenMode::ShortOnly => OpenStage::Short,
                    OpenMode::Hedge | OpenMode::LongOnly => OpenStage::Long,
                };
                draft.expires_at = draft_expiry();
                self.store_draft(user_id, draft.clone());
                self.open_prompt(state, lang, &draft)
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
                let hedge = draft.mode == Some(OpenMode::Hedge);
                draft.long_venue = Some(venue);
                draft.stage = if hedge {
                    OpenStage::Short
                } else {
                    OpenStage::Amount
                };
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
                if draft.mode == Some(OpenMode::Hedge) && draft.long_venue.as_ref() == Some(&venue)
                {
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
                    draft.stage = OpenStage::Mode;
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
                let (Some(symbol), Some(legs)) = (draft.symbol.clone(), draft_legs(&draft)) else {
                    self.clear_draft(user_id);
                    return Some(BotReply::text(expired_open_text(lang)));
                };
                self.clear_draft(user_id);
                Some(self.create_pending(state, user_id, lang, &symbol, legs, notional_usd, leverage))
            }
            OpenStage::Mode | OpenStage::Long | OpenStage::Short => Some(BotReply::text(pick(
                lang,
                "请点击上方按钮选择开仓方式或交易所，或重新发送 /open 开始新的开仓。",
                "Tap a button above to choose the entry mode or venue, or send /open to restart the entry.",
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

        let outcome = gateway::submit_legs(
            state,
            OrderBatchRequest {
                legs: trade
                    .legs
                    .iter()
                    .map(|leg| LegOrderRequest {
                        venue: leg.venue.clone(),
                        symbol: trade.symbol.clone(),
                        side: leg.side,
                        notional_usd: trade.notional_usd,
                        limit_price: None,
                        quantity: None,
                    })
                    .collect(),
                leverage: Some(trade.leverage),
                max_slippage_bps: None,
                strategy_id: None,
                source: None,
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
                let single_leg = order.intent.legs.len() == 1;
                let legs_label = order
                    .intent
                    .legs
                    .iter()
                    .map(|leg| {
                        format!("{} {}", venue_label(&leg.venue), side_label(lang, leg.side))
                    })
                    .collect::<Vec<_>>()
                    .join(" / ");
                let mut lines = match lang {
                    Lang::Zh => vec![
                        format!("订单 {:?}", order.status),
                        format!("编号 {}", order.id),
                        format!(
                            "{}：{legs_label}，{} {notional} USD",
                            order.intent.legs[0].symbol,
                            if single_leg { "单腿" } else { "每条腿" },
                            notional = trade.notional_usd
                        ),
                    ],
                    Lang::En => vec![
                        format!("Order {:?}", order.status),
                        format!("ID {}", order.id),
                        format!(
                            "{}: {legs_label}, {} {notional} USD",
                            order.intent.legs[0].symbol,
                            if single_leg { "single leg" } else { "per leg" },
                            notional = trade.notional_usd
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

    /// `/closeall`：汇总交易所仓位与上币/技能单腿持仓，回复带确认按钮的卡片。
    fn prepare_close_all(&self, state: &AppState, user_id: i64, lang: Lang) -> BotReply {
        let positions = state.positions.read().expect("仓位锁不可用").clone();
        let trades = state.listing.open_trades();
        if positions.is_empty() && trades.is_empty() {
            return BotReply::text(pick(lang, "暂无仓位，无需平仓。", "No open positions."));
        }

        let token = Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase();
        let expires_at = Utc::now() + chrono::Duration::seconds(CLOSE_ALL_TTL_SECS);
        {
            let mut pending = self.close_all.lock().expect("一键平仓锁不可用");
            pending.retain(|_, (_, expiry)| *expiry > Utc::now());
            pending.insert(token.clone(), (user_id, expires_at));
        }

        let mut lines = vec![match lang {
            Lang::Zh => format!("一键平仓确认 {token}"),
            Lang::En => format!("Close-all confirmation {token}"),
        }];
        lines.push(match lang {
            Lang::Zh => format!(
                "将平掉：交易所仓位 {} 条，上币/技能单腿 {} 笔。",
                positions.len(),
                trades.len()
            ),
            Lang::En => format!(
                "Will close {} exchange positions and {} listing/skill trades.",
                positions.len(),
                trades.len()
            ),
        });
        for position in positions.iter().take(8) {
            lines.push(format!(
                "{} {} {} {}",
                position.symbol,
                venue_label(&position.venue),
                side_label(lang, position.side),
                position.quantity
            ));
        }
        for trade in trades.iter().take(8) {
            lines.push(format!(
                "{} {} {} {}",
                trade.symbol,
                venue_label(&trade.venue),
                side_label(lang, trade.side),
                trade.quantity
            ));
        }
        lines.push(match lang {
            Lang::Zh => format!(
                "按最新盘口反向市价提交，{CLOSE_ALL_TTL_SECS} 秒内点击下方按钮确认。"
            ),
            Lang::En => format!(
                "Everything is market-closed at the latest quotes; tap a button within {CLOSE_ALL_TTL_SECS}s."
            ),
        });
        let markup = serde_json::json!({
            "inline_keyboard": [[
                {
                    "text": pick(lang, "确认全部平仓", "Confirm close-all"),
                    "callback_data": format!("closeall:confirm:{token}"),
                },
                {
                    "text": pick(lang, "取消", "Cancel"),
                    "callback_data": format!("closeall:cancel:{token}"),
                },
            ]],
        });
        BotReply::with_markup(lines.join("\n"), markup)
    }

    /// 用户点了「确认全部平仓」：先平上币/技能单腿，再按仓位组批量平掉交易所仓位。
    async fn confirm_close_all(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        token: &str,
    ) -> String {
        {
            let mut pending = self.close_all.lock().expect("一键平仓锁不可用");
            let valid = pending
                .get(token)
                .map(|(owner, expiry)| *owner == user_id && *expiry > Utc::now())
                .unwrap_or(false);
            if !valid {
                return pick(
                    lang,
                    "确认已过期，请重新发送 /closeall。",
                    "This confirmation expired; send /closeall again.",
                )
                .to_string();
            }
            pending.remove(token);
        }

        let mut closed_trades = 0usize;
        let mut failures: Vec<String> = Vec::new();
        // 平仓失败的单腿交易仍会被止盈止损监控接管，跳过对应仓位避免重复提交。
        let mut skipped: HashSet<(String, String, Side)> = HashSet::new();
        for trade in state.listing.open_trades() {
            match listing::close_position(state, trade.id, CloseReason::Manual).await {
                Ok(_) => closed_trades += 1,
                Err(error) => {
                    if !trade.paper {
                        skipped.insert((trade.venue.clone(), trade.symbol.clone(), trade.side));
                    }
                    failures.push(format!(
                        "{} {}: {}",
                        venue_label(&trade.venue),
                        trade.symbol,
                        describe_error(lang, &error)
                    ));
                }
            }
        }

        // 平仓会按成交即时减少内存仓位，这里重新取一次再平剩余部分。
        let positions = {
            let mut positions = state.positions.write().expect("仓位锁不可用");
            state.apply_realtime_prices(&mut positions);
            positions.clone()
        };
        let mut batches: Vec<(Option<Uuid>, Vec<arb_core::Position>)> = Vec::new();
        for position in positions {
            if skipped.contains(&(
                position.venue.clone(),
                position.symbol.clone(),
                position.side,
            )) {
                continue;
            }
            if let Some(group_id) = position.group_id {
                if let Some((_, members)) = batches.iter_mut().find(|(id, _)| *id == Some(group_id))
                {
                    members.push(position);
                    continue;
                }
                batches.push((Some(group_id), vec![position]));
            } else {
                batches.push((None, vec![position]));
            }
        }

        let mut closed_positions = 0usize;
        let mut closed_batches = 0usize;
        for (_, members) in batches {
            let mut legs = Vec::with_capacity(members.len());
            let mut invalid = None;
            for position in &members {
                let notional_usd = position.quantity * position.mark_price;
                if position.quantity <= Decimal::ZERO || notional_usd <= Decimal::ZERO {
                    invalid = Some(format!(
                        "{} {}",
                        venue_label(&position.venue),
                        position.symbol
                    ));
                    break;
                }
                legs.push(LegOrderRequest {
                    venue: position.venue.clone(),
                    symbol: position.symbol.clone(),
                    side: match position.side {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    },
                    notional_usd,
                    limit_price: None,
                    // 用同步回来的持仓数量，避免按名义价值换算留下零头。
                    quantity: Some(position.quantity),
                });
            }
            if let Some(label) = invalid {
                failures.push(match lang {
                    Lang::Zh => format!("{label} 仓位数量为 0，无法平仓"),
                    Lang::En => format!("{label} has zero size; cannot close"),
                });
                continue;
            }

            let outcome = gateway::submit_legs(
                state,
                OrderBatchRequest {
                    legs,
                    // 同一批共用一个杠杆，取组内最大值以免低估保证金需求。
                    leverage: Some(
                        members
                            .iter()
                            .map(|position| position.leverage)
                            .max()
                            .unwrap_or(Decimal::ONE),
                    ),
                    max_slippage_bps: None,
                    strategy_id: members.iter().find_map(|position| position.strategy_id),
                    source: None,
                    origin: OrderOrigin::Telegram,
                    idempotency_key: format!("telegram-closeall-{}", Uuid::new_v4()),
                    closing: true,
                    max_unhedged_secs: None,
                },
            )
            .await;

            match outcome {
                Ok(outcome) if outcome.order.status == OrderStatus::Filled => {
                    closed_positions += members.len();
                    closed_batches += 1;
                }
                Ok(outcome) => failures.push(format!(
                    "{}: {}",
                    members[0].symbol,
                    outcome
                        .order
                        .note
                        .unwrap_or_else(|| format!("{:?}", outcome.order.status))
                )),
                Err(error) => failures.push(format!(
                    "{}: {}",
                    members[0].symbol,
                    describe_error(lang, &error)
                )),
            }
        }

        let mut lines = vec![match lang {
            Lang::Zh => format!(
                "一键平仓完成\n交易所仓位 {} 条（{} 笔订单），上币/技能单腿 {} 笔",
                closed_positions, closed_batches, closed_trades
            ),
            Lang::En => format!(
                "Close-all finished\n{} exchange positions ({} orders) and {} listing/skill trades closed",
                closed_positions, closed_batches, closed_trades
            ),
        }];
        if closed_positions == 0 && closed_trades == 0 && failures.is_empty() {
            lines.push(pick(lang, "没有可平仓位。", "Nothing to close.").to_string());
        }
        if !failures.is_empty() {
            lines.push(match lang {
                Lang::Zh => "未完成：".to_string(),
                Lang::En => "Not closed:".to_string(),
            });
            for failure in failures.iter().take(8) {
                lines.push(format!("- {failure}"));
            }
        }
        lines.join("\n")
    }

    /// `/close <编号前缀>`：平掉单个仓位；编号来自 `/positions`，也兼容上币/技能单腿交易编号。
    async fn close_one(&self, state: &AppState, user_id: i64, lang: Lang, args: &[&str]) -> String {
        if !self.can_trade(user_id) {
            return pick(
                lang,
                "该账号没有交易权限。",
                "This account has no trading permission.",
            )
            .to_string();
        }
        let Some(prefix) = args
            .first()
            .map(|value| value.trim_start_matches('#').trim())
            .filter(|value| !value.is_empty())
        else {
            return pick(
                lang,
                "用法：/close <编号前缀>，编号见 /positions。",
                "Usage: /close <id prefix>; ids are listed by /positions.",
            )
            .to_string();
        };
        let prefix = prefix.to_ascii_lowercase();

        let matches: Vec<arb_core::Position> = {
            let mut positions = state.positions.write().expect("仓位锁不可用");
            state.apply_realtime_prices(&mut positions);
            positions
                .iter()
                .filter(|position| position.id.to_string().starts_with(&prefix))
                .cloned()
                .collect()
        };
        if matches.len() > 1 {
            return pick(
                lang,
                "匹配到多个仓位，请输入更长的编号前缀。",
                "Multiple positions match; send a longer id prefix.",
            )
            .to_string();
        }
        if let Some(position) = matches.first() {
            return self.close_position_now(state, lang, position).await;
        }

        match state.listing.find_open_by_prefix(&prefix) {
            Some(trade) => {
                match listing::close_position(state, trade.id, CloseReason::Manual).await {
                    Ok(closed) => listing_closed_text(lang, &closed),
                    Err(error) => match lang {
                        Lang::Zh => format!("平仓失败：{}", describe_error(lang, &error)),
                        Lang::En => format!("Close failed: {}", describe_error(lang, &error)),
                    },
                }
            }
            None => pick(lang, "没有找到匹配的仓位。", "No matching position.").to_string(),
        }
    }

    /// 单个交易所仓位的市价平仓；与一键平仓共用同一套网关风控。
    async fn close_position_now(
        &self,
        state: &AppState,
        lang: Lang,
        position: &arb_core::Position,
    ) -> String {
        let notional_usd = position.quantity * position.mark_price;
        if position.quantity <= Decimal::ZERO || notional_usd <= Decimal::ZERO {
            return match lang {
                Lang::Zh => format!("{} 仓位数量为 0，无法平仓。", position.symbol),
                Lang::En => format!("{} has zero size; cannot close.", position.symbol),
            };
        }
        let outcome = gateway::submit_legs(
            state,
            OrderBatchRequest {
                legs: vec![LegOrderRequest {
                    venue: position.venue.clone(),
                    symbol: position.symbol.clone(),
                    side: match position.side {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    },
                    notional_usd,
                    limit_price: None,
                    quantity: Some(position.quantity),
                }],
                leverage: Some(position.leverage),
                max_slippage_bps: None,
                strategy_id: position.strategy_id,
                source: None,
                origin: OrderOrigin::Telegram,
                idempotency_key: format!("telegram-close-{}", Uuid::new_v4()),
                closing: true,
                max_unhedged_secs: None,
            },
        )
        .await;
        match outcome {
            Ok(outcome) if outcome.order.status == OrderStatus::Filled => match lang {
                Lang::Zh => format!(
                    "已平仓 #{} {} {} {} {}",
                    short_id(position.id),
                    position.symbol,
                    venue_label(&position.venue),
                    side_label(lang, position.side),
                    position.quantity
                ),
                Lang::En => format!(
                    "Closed #{} {} {} {} {}",
                    short_id(position.id),
                    position.symbol,
                    venue_label(&position.venue),
                    side_label(lang, position.side),
                    position.quantity
                ),
            },
            Ok(outcome) => match lang {
                Lang::Zh => format!(
                    "平仓未成交：{}",
                    outcome
                        .order
                        .note
                        .unwrap_or_else(|| format!("{:?}", outcome.order.status))
                ),
                Lang::En => format!(
                    "Close not filled: {}",
                    outcome
                        .order
                        .note
                        .unwrap_or_else(|| format!("{:?}", outcome.order.status))
                ),
            },
            Err(error) => match lang {
                Lang::Zh => format!("平仓失败：{}", describe_error(lang, &error)),
                Lang::En => format!("Close failed: {}", describe_error(lang, &error)),
            },
        }
    }

    fn cancel_close_all(&self, user_id: i64, lang: Lang, token: &str) -> String {
        let mut pending = self.close_all.lock().expect("一键平仓锁不可用");
        match pending.get(token) {
            Some((owner, _)) if *owner == user_id => {
                pending.remove(token);
                pick(lang, "已取消一键平仓。", "Close-all cancelled.").to_string()
            }
            _ => pick(
                lang,
                "确认已过期，请重新发送 /closeall。",
                "This confirmation expired; send /closeall again.",
            )
            .to_string(),
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
                    side: card.side,
                    venue: venue.clone(),
                    expires_at: listing_expiry(),
                },
            );
        BotReply::with_markup(
            listing_confirm_text(lang, &card.announcement, card.side, &venue, &settings),
            listing_confirm_markup(lang, &id),
        )
    }

    /// 选好交易所后：首次手动下单先引导设置金额/杠杆/止盈止损，
    /// 保存过一次之后直接用上次的参数进入确认卡片。
    async fn listing_after_venue(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        id: &str,
        venue: &str,
    ) -> BotReply {
        let id = id.to_ascii_uppercase();
        let venue = venue.trim().to_ascii_lowercase();
        let settings = listing::manual_settings(state).await;
        if !listing_venues(state, &settings).contains(&venue) {
            return BotReply::text(unknown_venue_text(lang, &venue));
        }
        if state.listing.manual_rule().is_some() {
            return self
                .listing_preview(state, user_id, lang, &id, &venue)
                .await;
        }
        let wizard = ListingWizard {
            token: None,
            id,
            venue: Some(venue),
            stage: ListingStage::Params,
            expires_at: listing_expiry(),
        };
        self.store_listing_wizard(user_id, wizard);
        BotReply::with_markup(
            listing_params_prompt_text(lang, &settings),
            listing_cancel_markup(lang),
        )
    }

    /// 确认卡片上的「修改参数」：回到参数输入，改完再刷新确认卡片。
    async fn listing_params_step(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        id: &str,
    ) -> BotReply {
        let id = id.to_ascii_uppercase();
        let venue = {
            let drafts = self.listing_drafts.lock().expect("上币草稿锁不可用");
            drafts
                .get(&user_id)
                .filter(|draft| draft.id == id && draft.expires_at > Utc::now())
                .map(|draft| draft.venue.clone())
        };
        let Some(venue) = venue else {
            return BotReply::text(listing_expired_text(lang));
        };
        let wizard = ListingWizard {
            token: None,
            id,
            venue: Some(venue),
            stage: ListingStage::Params,
            expires_at: listing_expiry(),
        };
        self.store_listing_wizard(user_id, wizard);
        let settings = listing::manual_settings(state).await;
        BotReply::with_markup(
            listing_params_prompt_text(lang, &settings),
            listing_cancel_markup(lang),
        )
    }

    /// 用户点了「确认开仓」：按设置里的金额、杠杆与止盈止损单腿下单。
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
            side: draft.side,
            notional_usd: settings.notional_usd,
            leverage: settings.leverage,
            take_profit_pct: settings.take_profit_pct,
            stop_loss_pct: settings.stop_loss_pct,
            // 电报里手动确认的下单没有运行中的规则触发，仓位来源按手动处理。
            strategy_id: None,
            rule_id: None,
            rule_name: None,
            system: None,
            group_id: None,
            unit_index: 0,
            n_at_entry: None,
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

    /// 放弃当前上币引导与待确认草稿。
    fn abort_listing(&self, user_id: i64, lang: Lang) -> BotReply {
        self.listing_wizards
            .lock()
            .expect("上币引导锁不可用")
            .remove(&user_id);
        self.listing_drafts
            .lock()
            .expect("上币草稿锁不可用")
            .remove(&user_id);
        BotReply::text(pick(lang, "已取消上币下单。", "Listing order cancelled."))
    }

    fn store_listing_wizard(&self, user_id: i64, wizard: ListingWizard) {
        self.listing_wizards
            .lock()
            .expect("上币引导锁不可用")
            .insert(user_id, wizard);
    }

    fn current_listing_wizard(&self, user_id: i64) -> Option<ListingWizard> {
        let mut wizards = self.listing_wizards.lock().expect("上币引导锁不可用");
        if matches!(wizards.get(&user_id), Some(wizard) if wizard.expires_at <= Utc::now()) {
            wizards.remove(&user_id);
            return None;
        }
        wizards.get(&user_id).cloned()
    }

    fn clear_listing_wizard(&self, user_id: i64) {
        self.listing_wizards
            .lock()
            .expect("上币引导锁不可用")
            .remove(&user_id);
    }

    /// 上币引导的进度提示：按当前阶段告诉用户下一步该做什么。
    async fn listing_open_prompt(
        &self,
        state: &AppState,
        lang: Lang,
        wizard: &ListingWizard,
    ) -> BotReply {
        match wizard.stage {
            ListingStage::Token => BotReply::with_markup(
                pick(
                    lang,
                    "上币下单第 1 步：发送代币代码，例如 XYZ。",
                    "Listing entry step 1: send the token code, e.g. XYZ.",
                ),
                listing_cancel_markup(lang),
            ),
            ListingStage::Side => {
                let token = wizard.token.clone().unwrap_or_default();
                let text = match lang {
                    Lang::Zh => format!("上币下单 {token}\n第 2 步：选择做多还是做空。"),
                    Lang::En => format!("Listing entry {token}\nStep 2: choose long or short."),
                };
                BotReply::with_markup(text, listing_side_markup(lang, &wizard.id))
            }
            ListingStage::Venue => {
                let settings = listing::manual_settings(state).await;
                let venues = listing_venues(state, &settings);
                if venues.is_empty() {
                    return BotReply::text(listing_no_venue_text(lang));
                }
                let token = wizard.token.clone().unwrap_or_default();
                let side = self
                    .listing_cards
                    .lock()
                    .expect("上币卡片锁不可用")
                    .get(&wizard.id)
                    .map(|card| card.side)
                    .unwrap_or(Side::Buy);
                let text = match lang {
                    Lang::Zh => format!(
                        "上币下单 {token} {}\n第 3 步：选择交易所。",
                        side_label(lang, side)
                    ),
                    Lang::En => format!(
                        "Listing entry {token} {}\nStep 3: choose the venue.",
                        side_label(lang, side)
                    ),
                };
                BotReply::with_markup(text, listing_venue_markup(lang, &wizard.id, &venues))
            }
            ListingStage::Params => {
                let settings = listing::manual_settings(state).await;
                BotReply::with_markup(
                    listing_params_prompt_text(lang, &settings),
                    listing_cancel_markup(lang),
                )
            }
        }
    }

    /// 上币引导进行中收到的普通文本：目前只有代币代码需要手输。
    async fn listing_input(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        text: &str,
    ) -> Option<BotReply> {
        if !self.can_trade(user_id) {
            return None;
        }
        let mut wizard = self.current_listing_wizard(user_id)?;
        match wizard.stage {
            ListingStage::Token => {
                let Some(token) = parse_token(text) else {
                    return Some(BotReply::text(token_error_text(lang)));
                };
                wizard.token = Some(token);
                wizard.stage = ListingStage::Side;
                wizard.expires_at = listing_expiry();
                self.store_listing_wizard(user_id, wizard.clone());
                Some(self.listing_open_prompt(state, lang, &wizard).await)
            }
            ListingStage::Params => {
                let mut rule = listing::manual_settings(state).await;
                let Some((amount, leverage, take_profit, stop_loss)) =
                    parse_listing_params(text, &rule)
                else {
                    return Some(BotReply::text(listing_params_error_text(lang)));
                };
                let Some(venue) = wizard.venue.clone() else {
                    return Some(BotReply::text(listing_expired_text(lang)));
                };
                rule.notional_usd = amount;
                rule.leverage = leverage;
                rule.take_profit_pct = take_profit;
                rule.stop_loss_pct = stop_loss;
                state.listing.save_manual_rule(rule).await;
                self.clear_listing_wizard(user_id);
                Some(self.listing_preview(state, user_id, lang, &wizard.id, &venue).await)
            }
            _ => Some(BotReply::text(pick(
                lang,
                "请点击上方按钮选择方向或交易所，或发送 /listing open 重新开始。",
                "Tap a button above to choose the direction or venue, or send /listing open to restart.",
            ))),
        }
    }

    /// 用户点了做多/做空按钮：写入卡片并进入交易所选择。
    async fn listing_side_step(
        &self,
        state: &AppState,
        user_id: i64,
        lang: Lang,
        id: &str,
        side: Side,
    ) -> BotReply {
        let id = id.to_ascii_uppercase();
        let Some(mut wizard) = self.current_listing_wizard(user_id) else {
            return BotReply::text(listing_expired_text(lang));
        };
        if wizard.id != id || wizard.stage != ListingStage::Side {
            return BotReply::text(listing_expired_text(lang));
        }
        let Some(token) = wizard.token.clone() else {
            return BotReply::text(listing_expired_text(lang));
        };
        self.insert_listing_card(&id, &token, None, side, lang);
        wizard.stage = ListingStage::Venue;
        wizard.expires_at = listing_expiry();
        self.store_listing_wizard(user_id, wizard.clone());
        self.listing_open_prompt(state, lang, &wizard).await
    }

    /// 写入一张上币卡片，供后续交易所按钮与确认卡片读取。
    fn insert_listing_card(
        &self,
        id: &str,
        token: &str,
        symbol: Option<String>,
        side: Side,
        lang: Lang,
    ) {
        let announcement = ListingAnnouncement {
            token: token.to_string(),
            symbol: symbol.unwrap_or_else(|| format!("{token}USDT")),
            headline: manual_listing_headline(lang, side),
        };
        let mut cards = self.listing_cards.lock().expect("上币卡片锁不可用");
        cards.retain(|_, card| card.expires_at > Utc::now());
        cards.insert(
            id.to_string(),
            ListingCard {
                announcement,
                side,
                expires_at: listing_expiry(),
            },
        );
    }

    /// `/listing`：查看上币单腿交易；下单与平仓仍走统一风控网关。
    /// 金额、杠杆与止盈止损在首次下单引导里设置一次，之后自动复用。
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
        match sub.as_str() {
            "open" => {
                if !self.can_trade(user_id) {
                    return BotReply::text(listing_permission_text(lang));
                }
                self.clear_listing_wizard(user_id);
                self.clear_draft(user_id);

                // 不带代币时进入引导第 1 步，由用户下一条文本提供代币代码。
                let Some(token) = args.get(1).and_then(|value| parse_token(value)) else {
                    let wizard = ListingWizard {
                        id: listing_card_id(),
                        token: None,
                        venue: None,
                        stage: ListingStage::Token,
                        expires_at: listing_expiry(),
                    };
                    self.store_listing_wizard(user_id, wizard.clone());
                    return self.listing_open_prompt(state, lang, &wizard).await;
                };

                // 交易所与方向都可省略且顺序不限：第 3 个参数既可以是交易所，
                // 也可以是方向（long/short、多/空），第 4 个参数补交易对或方向。
                let mut venue = None;
                let mut side = None;
                let mut symbol = None;
                for raw in args.iter().skip(2).take(3) {
                    let value = raw.trim();
                    if let Some(parsed) = parse_side(value) {
                        side = Some(parsed);
                    } else if !value.is_empty() && venue.is_none() {
                        venue = Some(value.to_ascii_lowercase());
                    } else if !value.is_empty() {
                        symbol = Some(value.to_ascii_uppercase());
                    }
                }
                let has_side = side.is_some();
                let side = side.unwrap_or(Side::Buy);

                // 交易所已给出：首次下单先补参数，保存过就直接进入确认卡片。
                if let Some(venue) = venue {
                    let id = listing_card_id();
                    self.insert_listing_card(&id, &token, symbol, side, lang);
                    return self
                        .listing_after_venue(state, user_id, lang, &id, &venue)
                        .await;
                }

                // 交易所未给出：继续用按钮引导选择方向与交易所。
                let id = listing_card_id();
                if has_side {
                    self.insert_listing_card(&id, &token, symbol, side, lang);
                }
                let wizard = ListingWizard {
                    token: Some(token),
                    id,
                    venue: None,
                    stage: if has_side {
                        ListingStage::Venue
                    } else {
                        ListingStage::Side
                    },
                    expires_at: listing_expiry(),
                };
                self.store_listing_wizard(user_id, wizard.clone());
                self.listing_open_prompt(state, lang, &wizard).await
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
            "/open 开仓向导：按钮选开仓方式与交易所，输入金额与杠杆（默认 10x），支持单腿或双腿",
            "开仓会回复确认按钮，点击「确认开仓」后才真正下单",
            "/listing open 进入引导：代币 → 方向 → 交易所 → 参数",
            "/closeall 一键平仓：确认后平掉全部交易所仓位与上币/技能持仓",
            "/close <编号前缀> 平掉单个仓位（编号见 /positions）",
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
            "/open Entry wizard: pick the mode and venues with buttons, then type the amount and leverage (default 10x); single or hedged legs",
            "Entries reply with a confirm button; tap it to place the order",
            "/listing open starts the guide: token → direction → venue → params",
            "/closeall Close everything: all exchange positions and listing/skill trades, after confirmation",
            "/close <id prefix> closes one position (ids are listed by /positions)",
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

/// 解析方向输入：常见英文与中文写法都接受。
fn parse_side(value: &str) -> Option<Side> {
    match value.trim().to_ascii_lowercase().as_str() {
        "long" | "buy" | "多" | "做多" => Some(Side::Buy),
        "short" | "sell" | "空" | "做空" => Some(Side::Sell),
        _ => None,
    }
}

/// 解析代币代码：2–12 位字母数字，统一大写。
fn parse_token(value: &str) -> Option<String> {
    let token = value.trim().to_ascii_uppercase();
    if token.len() < 2 || token.len() > 12 {
        return None;
    }
    if !token.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    Some(token)
}

/// 解析手动下单参数：金额 [杠杆] [止盈] [止损]，省略的尾部沿用上次设置。
fn parse_listing_params(
    value: &str,
    current: &ListingRule,
) -> Option<(Decimal, Decimal, Decimal, Decimal)> {
    let numbers: Vec<Decimal> = value
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '/' || ch == '，')
        .filter(|part| !part.trim().is_empty())
        .map(|part| part.trim().parse::<Decimal>().ok())
        .collect::<Option<_>>()?;
    if numbers.is_empty() || numbers.len() > 4 {
        return None;
    }
    let amount = numbers[0];
    let leverage = numbers.get(1).copied().unwrap_or(current.leverage);
    let take_profit = numbers.get(2).copied().unwrap_or(current.take_profit_pct);
    let stop_loss = numbers.get(3).copied().unwrap_or(current.stop_loss_pct);
    if amount <= Decimal::ZERO
        || leverage <= Decimal::ZERO
        || take_profit < Decimal::ZERO
        || stop_loss < Decimal::ZERO
    {
        return None;
    }
    Some((amount, leverage, take_profit, stop_loss))
}

/// 第一次手动下单时，引导用户一次性设置金额、杠杆与止盈止损。
fn listing_params_prompt_text(lang: Lang, settings: &ListingRule) -> String {
    match lang {
        Lang::Zh => format!(
            "上币下单参数：发送「金额 杠杆 止盈 止损」，例如 100 1 20 10（止盈/止损填 0 关闭）。\n当前参数：{} USD · {}x · 止盈 {}% · 止损 {}%。设置一次后，下次直接复用。",
            settings.notional_usd,
            settings.leverage,
            settings.take_profit_pct,
            settings.stop_loss_pct
        ),
        Lang::En => format!(
            "Listing entry params: send \"amount leverage TP SL\", e.g. 100 1 20 10 (TP/SL 0 disables).\nCurrent: {} USD · {}x · TP {}% · SL {}%. Saved once and reused next time.",
            settings.notional_usd,
            settings.leverage,
            settings.take_profit_pct,
            settings.stop_loss_pct
        ),
    }
}

/// 手动下单参数格式不正确时的提示。
fn listing_params_error_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "参数不正确：请发送「金额 杠杆 止盈 止损」，金额与杠杆需大于 0，止盈止损可为 0（关闭）。",
        "Invalid parameters: send \"amount leverage TP SL\"; amount and leverage must be positive, TP/SL may be 0 to disable.",
    )
}

/// 手动发起上币单腿交易时的公告标题，按方向区分。
fn manual_listing_headline(lang: Lang, side: Side) -> String {
    match (lang, side) {
        (Lang::Zh, Side::Buy) => "手动发起的上币单腿做多",
        (Lang::Zh, Side::Sell) => "手动发起的上币单腿做空",
        (Lang::En, Side::Buy) => "Manually started listing long",
        (Lang::En, Side::Sell) => "Manually started listing short",
    }
    .to_string()
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

/// 向导草稿对应的下单腿：双腿返回多、空各一条，单腿只返回一条。
fn draft_legs(draft: &OpenDraft) -> Option<Vec<PendingLeg>> {
    match draft.mode? {
        OpenMode::Hedge => Some(vec![
            PendingLeg {
                venue: draft.long_venue.clone()?,
                side: Side::Buy,
            },
            PendingLeg {
                venue: draft.short_venue.clone()?,
                side: Side::Sell,
            },
        ]),
        OpenMode::LongOnly => Some(vec![PendingLeg {
            venue: draft.long_venue.clone()?,
            side: Side::Buy,
        }]),
        OpenMode::ShortOnly => Some(vec![PendingLeg {
            venue: draft.short_venue.clone()?,
            side: Side::Sell,
        }]),
    }
}

/// 向导金额步骤的摘要：双腿显示多空两边，单腿标注“单腿”。
fn draft_legs_summary(lang: Lang, draft: &OpenDraft) -> String {
    match draft.mode {
        Some(OpenMode::Hedge) => {
            let long = venue_label(&draft.long_venue.clone().unwrap_or_default());
            let short = venue_label(&draft.short_venue.clone().unwrap_or_default());
            match lang {
                Lang::Zh => format!("多头 {long} / 空头 {short}"),
                Lang::En => format!("Long {long} / short {short}"),
            }
        }
        Some(OpenMode::LongOnly) => {
            let long = venue_label(&draft.long_venue.clone().unwrap_or_default());
            match lang {
                Lang::Zh => format!("多头 {long}（单腿）"),
                Lang::En => format!("Long {long} (single leg)"),
            }
        }
        Some(OpenMode::ShortOnly) => {
            let short = venue_label(&draft.short_venue.clone().unwrap_or_default());
            match lang {
                Lang::Zh => format!("空头 {short}（单腿）"),
                Lang::En => format!("Short {short} (single leg)"),
            }
        }
        None => String::new(),
    }
}

/// 待确认订单的腿描述，例如 “binance 多 / okx 空”。
fn pending_legs_label(lang: Lang, legs: &[PendingLeg]) -> String {
    legs.iter()
        .map(|leg| format!("{} {}", venue_label(&leg.venue), side_label(lang, leg.side)))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 开仓方式按钮：双腿对冲 / 仅做多 / 仅做空。
fn mode_markup(lang: Lang) -> Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {
                    "text": pick(lang, "双腿对冲", "Hedged pair"),
                    "callback_data": "open:mode:hedge",
                },
                {
                    "text": pick(lang, "仅做多", "Long only"),
                    "callback_data": "open:mode:long",
                },
                {
                    "text": pick(lang, "仅做空", "Short only"),
                    "callback_data": "open:mode:short",
                },
            ],
            cancel_row(lang),
        ]
    })
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
                        "text": venue_label(venue),
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
        "开仓方式、交易所、金额与杠杆都在向导里选择。请发送 /open [交易对] 开始。",
        "The entry mode, venues, amount and leverage are all chosen in the wizard. Send /open [symbol] to start.",
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
        Lang::Zh => format!(
            "没有该交易所的可交易账户：{}，请用按钮选择。",
            venue_label(venue)
        ),
        Lang::En => format!(
            "No tradable account for venue {}; pick one of the buttons.",
            venue_label(venue)
        ),
    }
}

/// 交易所名称展示：首字母大写；内部匹配与回调数据仍使用小写。
fn venue_label(venue: &str) -> String {
    let mut chars = venue.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
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
    side: Side,
    venue: &str,
    settings: &ListingRule,
) -> String {
    let side_zh = if side == Side::Buy {
        "做多"
    } else {
        "做空"
    };
    let side_en = if side == Side::Buy { "long" } else { "short" };
    let action_zh = if side == Side::Buy {
        "买入"
    } else {
        "卖出"
    };
    let action_en = if side == Side::Buy { "buy" } else { "sell" };
    match lang {
        Lang::Zh => format!(
            "确认上币单腿开仓（{side_zh}）\n{} · {}\n市价{action_zh} {} USD，杠杆 {}x\n止盈 {} · 止损 {}\n\n点击「确认开仓」后才会下单；成交后由后台按标记价监控止盈止损。",
            announcement.symbol,
            venue_label(venue),
            settings.notional_usd.round_dp(2),
            settings.leverage.round_dp(2),
            pct_label(lang, settings.take_profit_pct),
            pct_label(lang, settings.stop_loss_pct),
        ),
        Lang::En => format!(
            "Confirm the listing {side_en}\n{} · {}\nMarket {action_en} {} USD, leverage {}x\nTP {} · SL {}\n\nThe order is placed only after you tap Confirm; once filled, the executor watches mark price for TP/SL.",
            announcement.symbol,
            venue_label(venue),
            settings.notional_usd.round_dp(2),
            settings.leverage.round_dp(2),
            pct_label(lang, settings.take_profit_pct),
            pct_label(lang, settings.stop_loss_pct),
        ),
    }
}

fn listing_confirm_markup(lang: Lang, id: &str) -> Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {
                    "text": pick(lang, "确认开仓", "Confirm entry"),
                    "callback_data": format!("listing:confirm:{id}"),
                },
                {
                    "text": pick(lang, "取消", "Cancel"),
                    "callback_data": format!("listing:cancel:{id}"),
                },
            ],
            [{
                "text": pick(lang, "修改参数", "Edit params"),
                "callback_data": format!("listing:params:{id}"),
            }],
        ],
    })
}

/// 上币引导里的「取消」按钮。
fn listing_cancel_markup(lang: Lang) -> Value {
    serde_json::json!({
        "inline_keyboard": [[{
            "text": pick(lang, "取消", "Cancel"),
            "callback_data": "listing:abort",
        }]],
    })
}

/// 上币引导第 2 步：做多/做空按钮。
fn listing_side_markup(lang: Lang, id: &str) -> Value {
    serde_json::json!({
        "inline_keyboard": [
            [
                {
                    "text": pick(lang, "做多", "Long"),
                    "callback_data": format!("listing:side:{id}:long"),
                },
                {
                    "text": pick(lang, "做空", "Short"),
                    "callback_data": format!("listing:side:{id}:short"),
                },
            ],
            [{
                "text": pick(lang, "取消", "Cancel"),
                "callback_data": "listing:abort",
            }],
        ],
    })
}

/// 上币引导第 3 步：交易所按钮，每行两个。
fn listing_venue_markup(lang: Lang, id: &str, venues: &[String]) -> Value {
    let mut rows: Vec<Vec<Value>> = venues
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|venue| {
                    serde_json::json!({
                        "text": venue_label(venue),
                        "callback_data": format!("listing:venue:{id}:{venue}"),
                    })
                })
                .collect()
        })
        .collect();
    rows.push(vec![serde_json::json!({
        "text": pick(lang, "取消", "Cancel"),
        "callback_data": "listing:abort",
    })]);
    serde_json::json!({ "inline_keyboard": rows })
}

async fn listing_status_text(lang: Lang, state: &AppState) -> String {
    let trades = state.listing.open_trades();
    let mut lines = Vec::new();
    // 策略规则与空仓计数不再回显，只在有持仓时列出可平仓的单子。
    if !trades.is_empty() {
        lines.push(match lang {
            Lang::Zh => format!("持仓 {} 笔", trades.len()),
            Lang::En => format!("Open trades: {}", trades.len()),
        });
    }
    for trade in trades.iter().take(10) {
        lines.push(match lang {
            Lang::Zh => format!(
                "#{} {} {} 入场 {}（止盈 {} / 止损 {}）",
                short_id(trade.id),
                trade.symbol,
                venue_label(&trade.venue),
                trade.entry_price.round_dp(6),
                pct_label(lang, trade.take_profit_pct),
                pct_label(lang, trade.stop_loss_pct),
            ),
            Lang::En => format!(
                "#{} {} {} entry {} (TP {} / SL {})",
                short_id(trade.id),
                trade.symbol,
                venue_label(&trade.venue),
                trade.entry_price.round_dp(6),
                pct_label(lang, trade.take_profit_pct),
                pct_label(lang, trade.stop_loss_pct),
            ),
        });
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(
        match lang {
            Lang::Zh => [
                "/listing 查看上币持仓",
                "/listing open 进入下单引导（首次设置金额/杠杆/止盈止损，之后复用）",
            ],
            Lang::En => [
                "/listing shows listing trades",
                "/listing open starts the entry guide (set size/leverage/TP/SL once, then reused)",
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
            "已开仓 #{}\n{} {} {} @ {}\n金额 {} USD · 杠杆 {}x\n止盈 {} · 止损 {}",
            short_id(trade.id),
            trade.symbol,
            venue_label(&trade.venue),
            side_label(lang, trade.side),
            trade.entry_price.round_dp(6),
            trade.notional_usd.round_dp(2),
            trade.leverage.round_dp(2),
            pct_label(lang, trade.take_profit_pct),
            pct_label(lang, trade.stop_loss_pct),
        ),
        Lang::En => format!(
            "Opened #{}\n{} {} {} @ {}\n{} USD · leverage {}x\nTP {} · SL {}",
            short_id(trade.id),
            trade.symbol,
            venue_label(&trade.venue),
            side_label(lang, trade.side),
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
            venue_label(&trade.venue),
            price.round_dp(6),
            trade.entry_price.round_dp(6),
        ),
        Lang::En => format!(
            "Closed #{}\n{} {} @ {} (entry {})",
            short_id(trade.id),
            trade.symbol,
            venue_label(&trade.venue),
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

/// 上币引导里代币代码格式不正确。
fn token_error_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "代币代码格式不正确，请发送 2–12 位字母数字，例如 XYZ。",
        "Invalid token code. Send 2–12 alphanumeric characters, e.g. XYZ.",
    )
}

/// 没有可用于上币下单的交易所账户。
fn listing_no_venue_text(lang: Lang) -> &'static str {
    pick(
        lang,
        "还没有可交易的上币下单交易所，请先在桌面端「账户」导入 API Key。",
        "No tradable venue for listing entries yet. Import API keys from Accounts in the desktop app first.",
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
            ("open", "发起开仓：单腿或双腿，需二次确认"),
            ("listing", "上币单腿交易：持仓与引导下单"),
            ("closeall", "一键平仓（需二次确认）"),
            ("close", "平掉单个仓位"),
            ("lang", "切换机器人语言"),
            ("unbind", "解除绑定"),
        ],
        Lang::En => &[
            ("status", "Executor status"),
            ("positions", "Current positions"),
            ("orders", "Recent orders"),
            ("news", "Latest listing announcements"),
            (
                "open",
                "Prepare an entry: single or hedged, needs confirmation",
            ),
            (
                "listing",
                "Listing single-leg trades: positions and guided entry",
            ),
            ("closeall", "Close everything (needs confirmation)"),
            ("close", "Close one position"),
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

/// 仓位方向的中英文标签。
fn side_label(lang: Lang, side: Side) -> &'static str {
    match (lang, side) {
        (Lang::Zh, Side::Buy) => "多",
        (Lang::Zh, Side::Sell) => "空",
        (Lang::En, Side::Buy) => "Long",
        (Lang::En, Side::Sell) => "Short",
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
            "#{} {} {} {} {} @ {}",
            short_id(position.id),
            position.symbol,
            venue_label(&position.venue),
            side_label(lang, position.side),
            position.quantity,
            position.entry_price
        ));
    }
    lines.push(
        pick(
            lang,
            "用 /close <编号> 平掉单个仓位，/closeall 全部平仓。",
            "Use /close <id> to close one position, or /closeall for everything.",
        )
        .to_string(),
    );
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
                let (step, choice) = value.split_once(':')?;
                if choice.is_empty() {
                    return None;
                }
                match step {
                    "mode" => match choice {
                        "hedge" => Some(Callback::Open(OpenStep::Mode(OpenMode::Hedge))),
                        "long" => Some(Callback::Open(OpenStep::Mode(OpenMode::LongOnly))),
                        "short" => Some(Callback::Open(OpenStep::Mode(OpenMode::ShortOnly))),
                        _ => None,
                    },
                    "long" => Some(Callback::Open(OpenStep::Long(choice.to_string()))),
                    "short" => Some(Callback::Open(OpenStep::Short(choice.to_string()))),
                    _ => None,
                }
            }
        },
        "listing" => {
            if value == "abort" {
                return Some(Callback::Listing(ListingStep::Abort));
            }
            let (step, rest) = value.split_once(':')?;
            match step {
                "side" => {
                    let (id, side) = rest.split_once(':')?;
                    if id.is_empty() {
                        return None;
                    }
                    let side = match side {
                        "long" => Side::Buy,
                        "short" => Side::Sell,
                        _ => return None,
                    };
                    Some(Callback::Listing(ListingStep::Side {
                        id: id.to_string(),
                        side,
                    }))
                }
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
                "params" => Some(Callback::Listing(ListingStep::Params(rest.to_string()))),
                "confirm" => Some(Callback::Listing(ListingStep::Confirm(rest.to_string()))),
                "cancel" => Some(Callback::Listing(ListingStep::Cancel(rest.to_string()))),
                _ => None,
            }
        }
        "closeall" => {
            let (step, token) = value.split_once(':')?;
            if token.is_empty() {
                return None;
            }
            let confirm = match step {
                "confirm" => true,
                "cancel" => false,
                _ => return None,
            };
            Some(Callback::CloseAll {
                token: token.to_string(),
                confirm,
            })
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
        assert!(!text.contains("/pause"));
        assert!(!text.contains("/resume"));
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
            assert!(!commands.iter().any(|item| item["command"] == "pause"));
            assert!(!commands.iter().any(|item| item["command"] == "resume"));
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
            parse_callback("open:mode:hedge"),
            Some(Callback::Open(OpenStep::Mode(OpenMode::Hedge)))
        );
        assert_eq!(
            parse_callback("open:mode:long"),
            Some(Callback::Open(OpenStep::Mode(OpenMode::LongOnly)))
        );
        assert_eq!(
            parse_callback("open:mode:short"),
            Some(Callback::Open(OpenStep::Mode(OpenMode::ShortOnly)))
        );
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
        assert_eq!(parse_callback("open:mode:"), None);
        assert_eq!(parse_callback("open:mode:other"), None);
        assert_eq!(parse_callback("open:side:binance"), None);
    }

    #[test]
    fn mode_markup_links_open_modes() {
        let markup = mode_markup(Lang::En);
        let rows = markup["inline_keyboard"]
            .as_array()
            .expect("内联键盘应为数组");
        assert_eq!(rows[0][0]["callback_data"], "open:mode:hedge");
        assert_eq!(rows[0][1]["callback_data"], "open:mode:long");
        assert_eq!(rows[0][2]["callback_data"], "open:mode:short");
        assert_eq!(rows[1][0]["callback_data"], "open:cancel");
    }

    #[test]
    fn drafts_single_or_hedged_legs() {
        let mut draft = OpenDraft {
            symbol: Some("BTCUSDT".to_string()),
            mode: Some(OpenMode::LongOnly),
            long_venue: Some("binance".to_string()),
            short_venue: None,
            stage: OpenStage::Amount,
            expires_at: draft_expiry(),
        };
        let legs = draft_legs(&draft).expect("单腿做多应有下单腿");
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].venue, "binance");
        assert_eq!(legs[0].side, Side::Buy);
        assert_eq!(draft_legs_summary(Lang::Zh, &draft), "多头 Binance（单腿）");

        draft.mode = Some(OpenMode::ShortOnly);
        draft.long_venue = None;
        draft.short_venue = Some("okx".to_string());
        let legs = draft_legs(&draft).expect("单腿做空应有下单腿");
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].venue, "okx");
        assert_eq!(legs[0].side, Side::Sell);

        draft.mode = Some(OpenMode::Hedge);
        draft.long_venue = Some("binance".to_string());
        let legs = draft_legs(&draft).expect("双腿应有两条腿");
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].side, Side::Buy);
        assert_eq!(legs[1].side, Side::Sell);
        assert_eq!(pending_legs_label(Lang::Zh, &legs), "Binance 多 / Okx 空");
    }

    #[test]
    fn parses_listing_callbacks() {
        assert_eq!(
            parse_callback("listing:abort"),
            Some(Callback::Listing(ListingStep::Abort))
        );
        assert_eq!(
            parse_callback("listing:side:ABC123:long"),
            Some(Callback::Listing(ListingStep::Side {
                id: "ABC123".to_string(),
                side: Side::Buy,
            }))
        );
        assert_eq!(
            parse_callback("listing:side:ABC123:short"),
            Some(Callback::Listing(ListingStep::Side {
                id: "ABC123".to_string(),
                side: Side::Sell,
            }))
        );
        assert_eq!(
            parse_callback("listing:venue:ABC123:bitget"),
            Some(Callback::Listing(ListingStep::Venue {
                id: "ABC123".to_string(),
                venue: "bitget".to_string(),
            }))
        );
        assert_eq!(
            parse_callback("listing:params:ABC123"),
            Some(Callback::Listing(ListingStep::Params("ABC123".to_string())))
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
        assert_eq!(parse_callback("listing:side:ABC123:other"), None);
        assert_eq!(parse_callback("listing:side::long"), None);
        assert_eq!(parse_callback("listing:venue:ABC123"), None);
        assert_eq!(parse_callback("listing:venue::bitget"), None);
        assert_eq!(parse_callback("listing:unknown:ABC123"), None);
    }

    #[test]
    fn parses_listing_tokens() {
        assert_eq!(parse_token("xyz"), Some("XYZ".to_string()));
        assert_eq!(parse_token(" XYZ "), Some("XYZ".to_string()));
        assert_eq!(parse_token("X"), None);
        assert_eq!(parse_token("XYZ-USD"), None);
        assert_eq!(parse_token(""), None);
    }

    #[test]
    fn parses_listing_params_inputs() {
        let current = ListingRule {
            notional_usd: Decimal::from(100),
            leverage: Decimal::from(2),
            take_profit_pct: Decimal::from(20),
            stop_loss_pct: Decimal::from(10),
            ..ListingRule::default()
        };
        assert_eq!(
            parse_listing_params("300 1 30 5", &current),
            Some((
                Decimal::from(300),
                Decimal::from(1),
                Decimal::from(30),
                Decimal::from(5),
            ))
        );
        // 省略的尾部沿用上次设置。
        assert_eq!(
            parse_listing_params("50", &current),
            Some((
                Decimal::from(50),
                Decimal::from(2),
                Decimal::from(20),
                Decimal::from(10),
            ))
        );
        assert_eq!(
            parse_listing_params("50 3 0 0", &current),
            Some((
                Decimal::from(50),
                Decimal::from(3),
                Decimal::ZERO,
                Decimal::ZERO,
            ))
        );
        assert_eq!(parse_listing_params("0 1 20 10", &current), None);
        assert_eq!(parse_listing_params("100 0 20 10", &current), None);
        assert_eq!(parse_listing_params("100 1 -1 10", &current), None);
        assert_eq!(parse_listing_params("100 1 20 10 5", &current), None);
        assert_eq!(parse_listing_params("abc", &current), None);
    }

    #[test]
    fn parses_close_all_callbacks() {
        assert_eq!(
            parse_callback("closeall:confirm:ABC123"),
            Some(Callback::CloseAll {
                token: "ABC123".to_string(),
                confirm: true,
            })
        );
        assert_eq!(
            parse_callback("closeall:cancel:ABC123"),
            Some(Callback::CloseAll {
                token: "ABC123".to_string(),
                confirm: false,
            })
        );
        assert_eq!(parse_callback("closeall:confirm:"), None);
        assert_eq!(parse_callback("closeall:other:ABC123"), None);
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
    fn parses_side_inputs() {
        assert_eq!(parse_side("long"), Some(Side::Buy));
        assert_eq!(parse_side("多"), Some(Side::Buy));
        assert_eq!(parse_side("short"), Some(Side::Sell));
        assert_eq!(parse_side(" 空 "), Some(Side::Sell));
        assert_eq!(parse_side("btcusdt"), None);
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
        assert_eq!(rows[0][0]["text"], "Binance");
        assert_eq!(rows[0][1]["text"], "Okx");
        assert_eq!(rows[1][0]["text"], "Bybit");
        assert_eq!(rows[2][0]["callback_data"], "open:cancel");
    }

    #[test]
    fn venue_labels_capitalize_first_letter() {
        assert_eq!(venue_label("gate"), "Gate");
        assert_eq!(venue_label("binance"), "Binance");
        assert_eq!(venue_label("okx"), "Okx");
        assert_eq!(venue_label("Gate"), "Gate");
        assert_eq!(venue_label(""), "");
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
