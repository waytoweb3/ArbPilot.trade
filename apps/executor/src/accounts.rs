use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context};
use arb_core::{Position, Side};
use base64::Engine as _;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

use crate::{
    exchange::aster,
    http_client::HttpClient,
    secret::{restrict_permissions, CredentialCipher, StorageBackend, StorageInfo},
    store::{EquityPoint, Store},
};

/// 支持导入 API Key 的 CEX，以及用钱包地址 + 签名私钥认证的 DEX 与 Aster。
pub const SUPPORTED_VENUES: [&str; 8] = [
    "binance",
    "okx",
    "bybit",
    "bitget",
    "gate",
    "aster",
    "hyperliquid",
    "lighter",
];

/// 这些交易所用钱包地址（Lighter 也可用账户索引）查询私有接口，用签名私钥在本机签名。
/// Aster 的 V3 接口同样走「主账户地址 + API 钱包私钥」，不再使用 API Key。
pub const WALLET_VENUES: [&str; 3] = ["hyperliquid", "lighter", "aster"];

/// 这些交易所在签名之外还需要 passphrase。
pub fn requires_passphrase(venue: &str) -> bool {
    matches!(venue, "okx" | "bitget")
}

pub fn is_supported(venue: &str) -> bool {
    SUPPORTED_VENUES.contains(&venue)
}

pub fn is_wallet_venue(venue: &str) -> bool {
    WALLET_VENUES.contains(&venue)
}

/// 各交易所私有接口的根地址，复用行情配置，方便走代理。
#[derive(Debug, Clone)]
pub struct AccountEndpoints {
    pub binance_spot: String,
    /// 合约钱包余额与持仓走 USDT 本位永续接口，和现货不是同一个域名。
    pub binance_fapi: String,
    pub okx: String,
    pub bybit: String,
    pub bitget: String,
    pub gate: String,
    pub aster: String,
    pub hyperliquid: String,
    pub lighter: String,
}

impl AccountEndpoints {
    fn get(&self, venue: &str) -> Option<&str> {
        match venue {
            "binance" => Some(&self.binance_spot),
            "okx" => Some(&self.okx),
            "bybit" => Some(&self.bybit),
            "bitget" => Some(&self.bitget),
            "gate" => Some(&self.gate),
            "aster" => Some(&self.aster),
            "hyperliquid" => Some(&self.hyperliquid),
            "lighter" => Some(&self.lighter),
            _ => None,
        }
    }
}

/// 保存在执行器本机的交易所凭证，永远不会通过 API 回传。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCredentials {
    pub id: Uuid,
    pub venue: String,
    pub label: String,
    /// CEX 的 API Key；DEX 只读账户为空字符串。
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_secret: String,
    #[serde(default)]
    pub passphrase: Option<String>,
    /// DEX 钱包地址；Lighter 也接受数字账户索引。
    #[serde(default)]
    pub wallet_address: Option<String>,
    /// DEX 签名私钥：Hyperliquid 的 API Wallet（Agent）私钥或 Lighter 的 API Key 私钥。
    /// 只保存在本机，且无法提现；为空时该账户仅能查询余额。
    #[serde(default)]
    pub signer_key: Option<String>,
    /// Lighter 的 API Key 序号（0-254），Hyperliquid 不使用。
    #[serde(default)]
    pub key_index: Option<u8>,
    /// Lighter 的数字账户索引，签名下单时必须，导入时可由钱包地址自动解析。
    #[serde(default)]
    pub account_index: Option<i64>,
    pub created_at: DateTime<Utc>,
}

/// 导入账户的入参，DEX 与 CEX 共用。
#[derive(Debug, Clone, Default)]
pub struct ImportRequest {
    pub venue: String,
    pub label: Option<String>,
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: Option<String>,
    pub wallet_address: Option<String>,
    pub signer_key: Option<String>,
    pub key_index: Option<u8>,
    pub account_index: Option<i64>,
}

/// 单个资产的余额，`usd_value` 在缺少价格时为 null。
#[derive(Debug, Clone, Serialize)]
pub struct AssetBalance {
    pub asset: String,
    pub free: Decimal,
    pub locked: Decimal,
    pub usd_value: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountSnapshotView {
    pub venue: String,
    pub label: String,
    pub equity_usd: Decimal,
    /// 合约账户当前可用于开仓的保证金（美元）；无法取得时为 null。
    pub available_margin_usd: Option<Decimal>,
    pub balances: Vec<AssetBalance>,
    pub observed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    pub id: Uuid,
    pub venue: String,
    /// `wallet` 表示钱包地址账户，`api_key` 表示密钥账户。
    pub kind: &'static str,
    pub label: String,
    pub api_key_preview: String,
    /// 是否具备下单所需凭证：CEX 的 Key/Secret，或 DEX 的签名私钥。
    pub can_trade: bool,
    pub created_at: DateTime<Utc>,
    pub snapshot: Option<AccountSnapshotView>,
}

/// 某个交易所当前的真实持仓，用于覆盖执行器内存里的仓位记录。
pub struct VenuePositions {
    pub venue: String,
    pub positions: Vec<Position>,
}

/// 账户池：凭证文件 + 最新快照 + 权益历史。
pub struct Accounts {
    http: Option<Arc<HttpClient>>,
    endpoints: AccountEndpoints,
    data_file: PathBuf,
    store: Arc<Store>,
    cipher: Option<CredentialCipher>,
    storage: StorageInfo,
    accounts: RwLock<Vec<AccountCredentials>>,
    snapshots: RwLock<HashMap<Uuid, AccountSnapshotView>>,
    price_cache: Mutex<Option<(Instant, HashMap<String, Decimal>)>>,
    last_point: Mutex<HashMap<Uuid, Instant>>,
}

impl Accounts {
    pub fn start(
        http: Option<Arc<HttpClient>>,
        endpoints: AccountEndpoints,
        data_file: PathBuf,
        poll_secs: u64,
        store: Arc<Store>,
    ) -> Arc<Self> {
        // 主密钥优先放系统钥匙串；拿不到时退回本机密钥文件。初始化失败则拒绝写入，
        // 避免把交易所凭证静默降级成明文。
        let cipher = match CredentialCipher::open(&data_file) {
            Ok(cipher) => Some(cipher),
            Err(error) => {
                tracing::error!(%error, path = %data_file.display(), "初始化凭证加密失败，将无法写入新的交易所凭证");
                None
            }
        };
        let storage = cipher
            .as_ref()
            .map(|cipher| cipher.info(&data_file))
            .unwrap_or_else(|| StorageInfo {
                encrypted: false,
                backend: StorageBackend::Unavailable,
                path: data_file.display().to_string(),
                exists: data_file.exists(),
                permissions: crate::secret::file_permissions(&data_file),
            });
        let (accounts, plaintext) = load_accounts(&data_file, cipher.as_ref());
        // 旧版本留下的是明文 JSON；首次读取成功后立即改写为加密信封。
        if plaintext && cipher.is_some() {
            match write_accounts(&data_file, &accounts, cipher.as_ref()) {
                Ok(()) => {
                    tracing::info!(path = %data_file.display(), "已将明文账户文件迁移为加密存储")
                }
                Err(error) => tracing::warn!(%error, "迁移账户文件为加密存储失败"),
            }
        }
        let service = Arc::new(Self {
            http,
            endpoints,
            data_file,
            store,
            cipher,
            storage,
            accounts: RwLock::new(accounts),
            snapshots: RwLock::new(HashMap::new()),
            price_cache: Mutex::new(None),
            last_point: Mutex::new(HashMap::new()),
        });
        service.spawn_poller(poll_secs);
        service
    }

    /// 凭证存储方式，供桌面端展示加密状态。
    pub fn storage(&self) -> StorageInfo {
        self.storage.clone()
    }

    /// 清除全部凭证并从本机删除凭证文件；返回被清除的账户数量。
    pub async fn clear(&self) -> anyhow::Result<usize> {
        let mut accounts = self.accounts.write().expect("账户锁不可用");
        let removed = accounts.len();

        // 先删文件再清空内存：删除失败时保留账户，避免界面显示已清除但重启后又出现。
        match std::fs::remove_file(&self.data_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow!(
                    "删除凭证文件失败 ({}): {error}",
                    self.data_file.display()
                ));
            }
        }
        accounts.clear();
        drop(accounts);

        self.snapshots.write().expect("快照锁不可用").clear();
        Ok(removed)
    }

    fn spawn_poller(self: &Arc<Self>, poll_secs: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(poll_secs.max(30)));
            loop {
                ticker.tick().await;
                this.refresh_all().await;
            }
        });
    }

    pub fn list(&self) -> Vec<AccountView> {
        let accounts = self.accounts.read().expect("账户锁不可用").clone();
        let snapshots = self.snapshots.read().expect("快照锁不可用");
        accounts
            .iter()
            .map(|account| view_of(account, snapshots.get(&account.id).cloned()))
            .collect()
    }

    /// 实盘下单使用的账户凭证；同一交易所优先取已配置签名凭证的账户。
    ///
    /// 现货腿（如 `binance-spot`、`okx-spot`、`bybit-spot`）与永续腿共用同一个
    /// 交易所账户，按去掉 `-spot` 后的交易所名查找凭证。
    pub fn credentials_for(&self, venue: &str) -> Option<AccountCredentials> {
        let venue = venue.trim().to_ascii_lowercase();
        let base = venue.strip_suffix("-spot").unwrap_or(&venue);
        let accounts = self.accounts.read().expect("账户锁不可用");
        accounts
            .iter()
            .find(|account| account.venue == base && has_trading_credentials(account))
            .or_else(|| accounts.iter().find(|account| account.venue == base))
            .cloned()
    }

    pub fn total_equity_usd(&self) -> Decimal {
        self.snapshots
            .read()
            .expect("快照锁不可用")
            .values()
            .map(|snapshot| snapshot.equity_usd)
            .sum()
    }

    /// 导入前先用私有接口验证一次，失败则不落盘。
    pub async fn import(&self, request: ImportRequest) -> anyhow::Result<AccountView> {
        let venue = request.venue.trim().to_ascii_lowercase();
        if !is_supported(&venue) {
            bail!("暂不支持该交易所: {venue}");
        }
        let wallet = is_wallet_venue(&venue);
        let wallet_address = request
            .wallet_address
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        // DEX 的签名私钥用于下单，导入时必填。
        let signer_key = request
            .signer_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let api_key = if wallet {
            String::new()
        } else {
            request.api_key.trim().to_string()
        };
        let api_secret = if wallet {
            String::new()
        } else {
            request.api_secret.trim().to_string()
        };

        let mut account_index = None;
        let mut key_index = None;
        if wallet {
            let address = wallet_address
                .as_deref()
                .ok_or_else(|| anyhow!("该交易所需要填写钱包地址或账户索引"))?;
            // 下单必须在本地签名，缺少私钥的账户无法交易，因此导入时直接要求必填。
            let key = signer_key
                .as_deref()
                .ok_or_else(|| anyhow!("DEX 账户需要填写签名私钥，否则无法下单"))?;
            // 私钥格式错误时直接拒绝导入，避免下单时才暴露。
            crate::exchange::dex::validate_signer_key(&venue, key)?;
            if venue == "lighter" {
                key_index = Some(request.key_index.unwrap_or(0));
                // 签名交易必须带上数字账户索引，钱包地址可直接解析出唯一主账户。
                account_index = Some(match request.account_index {
                    Some(index) => index,
                    None => self.resolve_lighter_account(address).await?,
                });
            }
        } else {
            if api_key.is_empty() || api_secret.is_empty() {
                bail!("API Key 与 Secret 不能为空");
            }
            if requires_passphrase(&venue)
                && request
                    .passphrase
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
            {
                bail!("该交易所需要填写 Passphrase");
            }
        }

        let credentials = AccountCredentials {
            id: Uuid::new_v4(),
            label: request
                .label
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| venue.to_ascii_uppercase()),
            venue,
            api_key,
            api_secret,
            passphrase: request
                .passphrase
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            wallet_address,
            signer_key,
            key_index,
            account_index,
            created_at: Utc::now(),
        };

        let snapshot = self.fetch(&credentials).await;
        if let Some(error) = &snapshot.error {
            bail!("验证失败：{error}");
        }

        self.accounts
            .write()
            .expect("账户锁不可用")
            .push(credentials.clone());
        self.persist()?;
        self.commit_snapshot(&credentials, snapshot).await;

        let snapshots = self.snapshots.read().expect("快照锁不可用");
        Ok(view_of(
            &credentials,
            snapshots.get(&credentials.id).cloned(),
        ))
    }

    /// 把 Lighter 的钱包地址或账户索引解析成数字账户索引。
    async fn resolve_lighter_account(&self, target: &str) -> anyhow::Result<i64> {
        if let Ok(index) = target.trim().parse::<i64>() {
            return Ok(index);
        }
        let http = self
            .http
            .clone()
            .ok_or_else(|| anyhow!("行情 HTTP 客户端不可用，无法解析 Lighter 账户索引"))?;
        let base_url = self
            .endpoints
            .get("lighter")
            .ok_or_else(|| anyhow!("缺少 Lighter 接口地址"))?
            .trim_end_matches('/')
            .to_string();
        let target = target.trim().to_string();

        tokio::task::spawn_blocking(move || {
            let value = http.get_json(&format!(
                "{base_url}/api/v1/account?by=l1_address&value={target}"
            ))?;
            let account = value["accounts"]
                .as_array()
                .and_then(|rows| rows.first())
                .ok_or_else(|| anyhow!("Lighter 未找到该钱包的账户，请确认地址或改填账户索引"))?;
            account["account_index"]
                .as_i64()
                .or_else(|| account["index"].as_i64())
                .ok_or_else(|| anyhow!("Lighter 响应缺少 account_index"))
        })
        .await
        .context("解析 Lighter 账户索引任务失败")?
    }

    pub async fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        let removed = {
            let mut accounts = self.accounts.write().expect("账户锁不可用");
            let before = accounts.len();
            accounts.retain(|account| account.id != id);
            before != accounts.len()
        };
        if !removed {
            bail!("账户不存在");
        }
        self.persist()?;
        self.snapshots.write().expect("快照锁不可用").remove(&id);
        self.last_point
            .lock()
            .expect("快照节流锁不可用")
            .remove(&id);
        self.store.clear_equity_points(id).await?;
        Ok(())
    }

    pub async fn refresh(&self, id: Uuid) -> anyhow::Result<AccountView> {
        let credentials = self
            .accounts
            .read()
            .expect("账户锁不可用")
            .iter()
            .find(|account| account.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("账户不存在"))?;

        let snapshot = self.fetch(&credentials).await;
        self.commit_snapshot(&credentials, snapshot).await;

        let snapshots = self.snapshots.read().expect("快照锁不可用");
        Ok(view_of(
            &credentials,
            snapshots.get(&credentials.id).cloned(),
        ))
    }

    pub async fn refresh_all(&self) {
        let accounts = self.accounts.read().expect("账户锁不可用").clone();
        for account in accounts {
            let snapshot = self.fetch(&account).await;
            self.commit_snapshot(&account, snapshot).await;
        }
    }

    /// 拉取所有已导入账户在交易所的真实持仓。
    ///
    /// 只有该交易所的全部账户都查询成功时才返回结果；任一账户失败则跳过该
    /// 交易所，避免网络抖动把用户真实仓位误判为已平仓。
    pub async fn sync_open_positions(&self) -> Vec<VenuePositions> {
        let accounts = self.accounts.read().expect("账户锁不可用").clone();
        let mut grouped: HashMap<String, (bool, Vec<Position>)> = HashMap::new();

        for account in accounts {
            let entry = grouped
                .entry(account.venue.clone())
                .or_insert((true, Vec::new()));
            match self.fetch_open_positions(&account).await {
                Ok(positions) => entry.1.extend(positions),
                Err(error) => {
                    tracing::warn!(venue = %account.venue, %error, "拉取交易所持仓失败");
                    entry.0 = false;
                }
            }
        }

        grouped
            .into_iter()
            .filter(|(_, (ok, _))| *ok)
            .map(|(venue, (_, positions))| VenuePositions { venue, positions })
            .collect()
    }

    /// 查询单个交易所当前的真实持仓（自检与调试用）。
    pub async fn open_positions_for(&self, venue: &str) -> anyhow::Result<Vec<Position>> {
        let credentials = self
            .credentials_for(venue)
            .ok_or_else(|| anyhow!("{venue} 尚未导入账户"))?;
        self.fetch_open_positions(&credentials).await
    }

    async fn fetch_open_positions(
        &self,
        credentials: &AccountCredentials,
    ) -> anyhow::Result<Vec<Position>> {
        let http = self
            .http
            .clone()
            .ok_or_else(|| anyhow!("行情 HTTP 客户端不可用"))?;
        let base_url = self
            .endpoints
            .get(&credentials.venue)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("暂不支持该交易所: {}", credentials.venue))?;
        let binance_fapi = self.endpoints.binance_fapi.clone();
        let credentials = credentials.clone();

        tokio::task::spawn_blocking(move || {
            fetch_positions(&http, &base_url, &binance_fapi, &credentials)
        })
        .await
        .context("查询持仓任务失败")?
    }

    /// 权益历史：按小时或 5 分钟桶聚合，返回总权益曲线。
    pub async fn history(&self, hours: i64, bucket_secs: i64) -> anyhow::Result<Vec<EquityPoint>> {
        let since = Utc::now() - chrono::Duration::hours(hours);
        let points = self.store.equity_history(since, None).await?;
        Ok(aggregate(points, bucket_secs))
    }

    async fn commit_snapshot(
        &self,
        credentials: &AccountCredentials,
        snapshot: AccountSnapshotView,
    ) {
        let success = snapshot.error.is_none();
        self.snapshots
            .write()
            .expect("快照锁不可用")
            .insert(credentials.id, snapshot.clone());

        if !success {
            return;
        }
        {
            let mut last = self.last_point.lock().expect("快照节流锁不可用");
            if let Some(at) = last.get(&credentials.id) {
                // 手动刷新与定时采集可能挨得很近，一分钟内只记一条历史。
                if at.elapsed() < Duration::from_secs(60) {
                    return;
                }
            }
            last.insert(credentials.id, Instant::now());
        }

        let point = EquityPoint {
            account_id: credentials.id,
            venue: credentials.venue.clone(),
            equity_usd: snapshot.equity_usd,
            observed_at: snapshot.observed_at,
        };
        if let Err(error) = self.store.save_equity_point(&point).await {
            tracing::warn!(%error, account_id = %credentials.id, "写入账户权益快照失败");
        }
    }

    async fn fetch(&self, credentials: &AccountCredentials) -> AccountSnapshotView {
        let now = Utc::now();
        let fallback = |error: String| AccountSnapshotView {
            venue: credentials.venue.clone(),
            label: credentials.label.clone(),
            equity_usd: Decimal::ZERO,
            available_margin_usd: None,
            balances: Vec::new(),
            observed_at: now,
            error: Some(error),
        };

        let Some(http) = self.http.clone() else {
            return fallback("行情 HTTP 客户端不可用".to_string());
        };
        let Some(base_url) = self.endpoints.get(&credentials.venue).map(str::to_string) else {
            return fallback(format!("暂不支持该交易所: {}", credentials.venue));
        };

        let prices = self.usd_prices().await.unwrap_or_default();
        let venue = credentials.venue.clone();
        let label = credentials.label.clone();
        let credentials = credentials.clone();
        let binance_fapi = self.endpoints.binance_fapi.clone();
        let result = tokio::task::spawn_blocking(move || {
            fetch_snapshot(&http, &base_url, &binance_fapi, &credentials, &prices)
        })
        .await;

        match result {
            Ok(Ok((equity, available_margin, balances))) => AccountSnapshotView {
                venue,
                label,
                equity_usd: equity,
                available_margin_usd: available_margin,
                balances,
                observed_at: Utc::now(),
                error: None,
            },
            Ok(Err(error)) => fallback(error.to_string()),
            Err(error) => fallback(format!("查询余额任务失败: {error}")),
        }
    }

    /// 用 Binance 现货全量价格把非稳定币资产折算成美元，缓存 60 秒。
    async fn usd_prices(&self) -> anyhow::Result<HashMap<String, Decimal>> {
        {
            let cache = self.price_cache.lock().expect("价格缓存锁不可用");
            if let Some((at, prices)) = cache.as_ref() {
                if at.elapsed() < Duration::from_secs(60) {
                    return Ok(prices.clone());
                }
            }
        }

        let http = self
            .http
            .clone()
            .ok_or_else(|| anyhow!("行情 HTTP 客户端不可用"))?;
        let url = format!(
            "{}/api/v3/ticker/price",
            self.endpoints.binance_spot.trim_end_matches('/')
        );
        let value = tokio::task::spawn_blocking(move || http.get_json(&url))
            .await
            .context("获取美元价格任务失败")??;

        let mut prices = HashMap::new();
        if let Some(rows) = value.as_array() {
            for row in rows {
                let Some(symbol) = row.get("symbol").and_then(Value::as_str) else {
                    continue;
                };
                let Some(price) = decimal(row.get("price").unwrap_or(&Value::Null)) else {
                    continue;
                };
                if let Some(asset) = symbol.strip_suffix("USDT") {
                    prices.insert(asset.to_string(), price);
                }
            }
        }
        if prices.is_empty() {
            bail!("未获取到任何美元价格");
        }

        *self.price_cache.lock().expect("价格缓存锁不可用") =
            Some((Instant::now(), prices.clone()));
        Ok(prices)
    }

    fn persist(&self) -> anyhow::Result<()> {
        let accounts = self.accounts.read().expect("账户锁不可用");
        write_accounts(&self.data_file, &accounts, self.cipher.as_ref())
    }
}

/// 该账户是否具备下单所需凭证：CEX 的 Key/Secret，或 DEX 的签名私钥。
fn has_trading_credentials(credentials: &AccountCredentials) -> bool {
    if is_wallet_venue(&credentials.venue) {
        credentials.signer_key.is_some()
    } else {
        !credentials.api_key.is_empty() && !credentials.api_secret.is_empty()
    }
}

fn view_of(credentials: &AccountCredentials, snapshot: Option<AccountSnapshotView>) -> AccountView {
    let wallet = is_wallet_venue(&credentials.venue);
    AccountView {
        id: credentials.id,
        venue: credentials.venue.clone(),
        kind: if wallet { "wallet" } else { "api_key" },
        label: credentials.label.clone(),
        api_key_preview: preview_key(if wallet {
            credentials.wallet_address.as_deref().unwrap_or_default()
        } else {
            &credentials.api_key
        }),
        can_trade: has_trading_credentials(credentials),
        created_at: credentials.created_at,
        snapshot,
    }
}

fn preview_key(key: &str) -> String {
    if key.len() <= 10 {
        return "••••".to_string();
    }
    format!("{}••••{}", &key[..4], &key[key.len() - 4..])
}

/// 读取账户文件，返回 `(账户列表, 是否为未加密的旧格式)`。
fn load_accounts(
    path: &Path,
    cipher: Option<&CredentialCipher>,
) -> (Vec<AccountCredentials>, bool) {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), false),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "读取账户文件失败");
            return (Vec::new(), false);
        }
    };

    let encrypted = CredentialCipher::is_encrypted(&raw);
    let plain = if encrypted {
        let Some(cipher) = cipher else {
            tracing::error!(path = %path.display(), "账户文件已加密，但本机主密钥不可用");
            return (Vec::new(), false);
        };
        match cipher.unseal(&raw) {
            Ok(plain) => plain,
            Err(error) => {
                tracing::error!(%error, path = %path.display(), "解密账户文件失败，保留原文件不做覆盖");
                return (Vec::new(), false);
            }
        }
    } else {
        raw
    };

    match serde_json::from_slice::<Vec<AccountCredentials>>(&plain) {
        Ok(accounts) => (accounts, !encrypted),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "解析账户文件失败，忽略已有账户");
            (Vec::new(), false)
        }
    }
}

fn write_accounts(
    path: &Path,
    accounts: &[AccountCredentials],
    cipher: Option<&CredentialCipher>,
) -> anyhow::Result<()> {
    let cipher = cipher.ok_or_else(|| anyhow!("本机主密钥不可用，已拒绝以明文写入交易所凭证"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建账户目录失败: {}", parent.display()))?;
    }

    let json = serde_json::to_vec_pretty(accounts).context("序列化账户失败")?;
    let sealed = cipher.seal(&json)?;

    // 先写临时文件再原子替换，避免进程中断留下半截凭证文件。
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("accounts.json");
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, sealed)
        .with_context(|| format!("写入临时账户文件失败: {}", tmp.display()))?;
    restrict_permissions(&tmp)?;
    if std::fs::rename(&tmp, path).is_err() {
        // Windows 不允许 rename 覆盖已存在文件，退化为先删除再替换。
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
            .with_context(|| format!("保存账户文件失败: {}", path.display()))?;
    }
    restrict_permissions(path)?;
    Ok(())
}

/// 把权益快照按固定时间桶聚合，输出每个桶结束时所有账户的合计权益。
fn aggregate(points: Vec<EquityPoint>, bucket_secs: i64) -> Vec<EquityPoint> {
    if points.is_empty() {
        return Vec::new();
    }
    let bucket_secs = bucket_secs.max(60);

    let mut buckets: std::collections::BTreeMap<i64, HashMap<Uuid, (String, Decimal)>> =
        std::collections::BTreeMap::new();
    for point in points {
        let bucket = point.observed_at.timestamp().div_euclid(bucket_secs) * bucket_secs;
        buckets
            .entry(bucket)
            .or_default()
            .insert(point.account_id, (point.venue.clone(), point.equity_usd));
    }

    let mut latest: HashMap<Uuid, (String, Decimal)> = HashMap::new();
    let mut out = Vec::with_capacity(buckets.len());
    for (bucket, updates) in buckets {
        latest.extend(updates);
        let equity_usd = latest.values().map(|(_, equity)| *equity).sum();
        let venue = latest
            .values()
            .next()
            .map(|(venue, _)| venue.clone())
            .unwrap_or_default();
        out.push(EquityPoint {
            // 聚合点没有单一账户，使用全零 UUID 占位。
            account_id: Uuid::nil(),
            venue,
            equity_usd,
            observed_at: Utc
                .timestamp_opt(bucket, 0)
                .single()
                .unwrap_or_else(Utc::now),
        });
    }
    out
}

/// 返回 (总权益, 资产明细)。
fn fetch_snapshot(
    http: &HttpClient,
    base_url: &str,
    binance_fapi: &str,
    credentials: &AccountCredentials,
    prices: &HashMap<String, Decimal>,
) -> anyhow::Result<(Decimal, Option<Decimal>, Vec<AssetBalance>)> {
    let (raw, override_equity, available_margin) = match credentials.venue.as_str() {
        "binance" => fetch_binance(http, base_url, binance_fapi, credentials)?,
        "okx" => fetch_okx(http, base_url, credentials)?,
        "bybit" => fetch_bybit(http, base_url, credentials)?,
        "bitget" => fetch_bitget(http, base_url, credentials)?,
        "gate" => fetch_gate(http, base_url, credentials)?,
        "aster" => fetch_aster(http, base_url, credentials)?,
        "hyperliquid" => fetch_hyperliquid(http, base_url, credentials)?,
        "lighter" => fetch_lighter(http, base_url, credentials)?,
        other => bail!("暂不支持该交易所: {other}"),
    };

    let (valued, balances) = value_balances(raw, prices);
    Ok((
        override_equity.unwrap_or(valued),
        available_margin,
        balances,
    ))
}

type RawBalances = Vec<(String, Decimal, Decimal)>;

fn value_balances(
    raw: RawBalances,
    prices: &HashMap<String, Decimal>,
) -> (Decimal, Vec<AssetBalance>) {
    let mut merged: HashMap<String, (Decimal, Decimal)> = HashMap::new();
    for (asset, free, locked) in raw {
        let entry = merged
            .entry(asset)
            .or_insert((Decimal::ZERO, Decimal::ZERO));
        entry.0 += free;
        entry.1 += locked;
    }

    let mut total = Decimal::ZERO;
    let mut balances = Vec::with_capacity(merged.len());
    for (asset, (free, locked)) in merged {
        let amount = free + locked;
        if amount <= Decimal::ZERO {
            continue;
        }
        let usd_value = usd_price(&asset, prices).map(|price| amount * price);
        if let Some(value) = usd_value {
            total += value;
        }
        balances.push(AssetBalance {
            asset,
            free,
            locked,
            usd_value,
        });
    }

    balances.sort_by(|left, right| {
        right
            .usd_value
            .unwrap_or_default()
            .cmp(&left.usd_value.unwrap_or_default())
    });
    balances.truncate(20);
    (total, balances)
}

fn usd_price(asset: &str, prices: &HashMap<String, Decimal>) -> Option<Decimal> {
    const STABLES: [&str; 8] = [
        "USDT", "USDC", "FDUSD", "BUSD", "DAI", "TUSD", "USDP", "PYUSD",
    ];
    if STABLES.contains(&asset) {
        return Some(Decimal::ONE);
    }
    prices.get(asset).copied()
}

fn fetch_binance(
    http: &HttpClient,
    base_url: &str,
    fapi_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let query = format!(
        "timestamp={}&recvWindow=5000",
        Utc::now().timestamp_millis()
    );
    let signature = hmac_sha256_hex(&credentials.api_secret, &query)?;
    let url = format!("{base_url}/api/v3/account?{query}&signature={signature}");
    let value =
        http.get_json_with_headers(&url, &[("X-MBX-APIKEY", credentials.api_key.as_str())])?;

    let rows = value["balances"]
        .as_array()
        .ok_or_else(|| binance_error(&value))?;
    let mut balances = Vec::with_capacity(rows.len());
    for row in rows {
        let asset = row.get("asset").and_then(Value::as_str).unwrap_or_default();
        let free = decimal(row.get("free").unwrap_or(&Value::Null)).unwrap_or_default();
        let locked = decimal(row.get("locked").unwrap_or(&Value::Null)).unwrap_or_default();
        balances.push((asset.to_string(), free, locked));
    }

    // 合约钱包与现货钱包相互独立，权益需要相加；合约接口必须走 fapi 域名。
    let futures_url = format!(
        "{}/fapi/v2/account?{query}&signature={signature}",
        fapi_url.trim_end_matches('/')
    );
    if let Ok(futures) = http.get_json_with_headers(
        &futures_url,
        &[("X-MBX-APIKEY", credentials.api_key.as_str())],
    ) {
        if let Some(rows) = futures["assets"].as_array() {
            let mut available_margin = Decimal::ZERO;
            for row in rows {
                let asset = row.get("asset").and_then(Value::as_str).unwrap_or_default();
                let wallet =
                    decimal(row.get("walletBalance").unwrap_or(&Value::Null)).unwrap_or_default();
                let available = decimal(row.get("availableBalance").unwrap_or(&Value::Null))
                    .unwrap_or_default();
                if wallet > Decimal::ZERO {
                    if asset == "USDT" {
                        available_margin += available;
                    }
                    balances.push((asset.to_string(), available, wallet - available));
                }
            }
            return Ok((balances, None, Some(available_margin)));
        }
    }

    Ok((balances, None, None))
}

fn fetch_okx(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let path = "/api/v5/account/balance";
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_sha256_base64(&credentials.api_secret, &format!("{timestamp}GET{path}"))?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("OKX 需要 Passphrase"))?;

    let headers = [
        ("OK-ACCESS-KEY", credentials.api_key.as_str()),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http.get_json_with_headers(&format!("{base_url}{path}"), &headers)?;
    if value["code"].as_str().unwrap_or("0") != "0" {
        bail!(
            "OKX 返回错误: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        );
    }

    let account = value["data"]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or(Value::Null);
    let equity = decimal(&account["totalEq"]).filter(|value| *value > Decimal::ZERO);

    let mut balances = Vec::new();
    let mut available_margin = None;
    if let Some(details) = account["details"].as_array() {
        let mut available_total = Decimal::ZERO;
        for detail in details {
            let asset = detail
                .get("ccy")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let total = decimal(detail.get("eq").unwrap_or(&Value::Null))
                .or_else(|| decimal(detail.get("cashBal").unwrap_or(&Value::Null)))
                .unwrap_or_default();
            let available =
                decimal(detail.get("availBal").unwrap_or(&Value::Null)).unwrap_or_default();
            if asset == "USDT" {
                available_total += available;
            }
            balances.push((
                asset.to_string(),
                available,
                (total - available).max(Decimal::ZERO),
            ));
        }
        available_margin = Some(available_total);
    }
    Ok((balances, equity, available_margin))
}

fn fetch_bybit(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let timestamp = Utc::now().timestamp_millis().to_string();
    let recv_window = "5000";
    let query = "accountType=UNIFIED";
    let signature = hmac_sha256_hex(
        &credentials.api_secret,
        &format!("{timestamp}{}{recv_window}{query}", credentials.api_key),
    )?;

    let headers = [
        ("X-BAPI-API-KEY", credentials.api_key.as_str()),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let url = format!("{base_url}/v5/account/wallet-balance?{query}");
    let value = http.get_json_with_headers(&url, &headers)?;
    if value["retCode"].as_i64().unwrap_or(0) != 0 {
        bail!(
            "Bybit 返回错误: {}",
            value["retMsg"].as_str().unwrap_or("未知错误")
        );
    }

    let account = value["result"]["list"]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or(Value::Null);
    let equity = decimal(&account["totalEquity"]).filter(|value| *value > Decimal::ZERO);
    // 统一账户的可用保证金以账户级字段为准，避免把已占用保证金算成可用。
    let available_margin = decimal(&account["totalAvailableBalance"]);

    let mut balances = Vec::new();
    if let Some(coins) = account["coin"].as_array() {
        for coin in coins {
            let asset = coin.get("coin").and_then(Value::as_str).unwrap_or_default();
            let wallet =
                decimal(coin.get("walletBalance").unwrap_or(&Value::Null)).unwrap_or_default();
            let locked = decimal(coin.get("locked").unwrap_or(&Value::Null)).unwrap_or_default();
            balances.push((
                asset.to_string(),
                (wallet - locked).max(Decimal::ZERO),
                locked,
            ));
        }
    }
    Ok((balances, equity, available_margin))
}

fn fetch_bitget(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("Bitget 需要 Passphrase"))?;
    let timestamp = Utc::now().timestamp_millis().to_string();

    // 现货资产：只开通合约权限的 Key 会返回 40014（缺少 spot 权限），
    // 这类 Key 仍可用于永续套利，因此先记录错误并继续查询合约账户。
    let spot_path = "/api/v2/spot/account/assets";
    let spot_sign = hmac_sha256_base64(
        &credentials.api_secret,
        &format!("{timestamp}GET{spot_path}"),
    )?;
    let spot_headers = [
        ("ACCESS-KEY", credentials.api_key.as_str()),
        ("ACCESS-SIGN", spot_sign.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let mut balances = Vec::new();
    let mut spot_error = None;
    match http.get_json_with_headers(&format!("{base_url}{spot_path}"), &spot_headers) {
        Ok(spot) if spot["code"].as_str().unwrap_or("00000") == "00000" => {
            if let Some(rows) = spot["data"].as_array() {
                for row in rows {
                    let asset = row.get("coin").and_then(Value::as_str).unwrap_or_default();
                    let available =
                        decimal(row.get("available").unwrap_or(&Value::Null)).unwrap_or_default();
                    let frozen =
                        decimal(row.get("frozen").unwrap_or(&Value::Null)).unwrap_or_default();
                    balances.push((asset.to_string(), available, frozen));
                }
            }
        }
        Ok(spot) => {
            spot_error = Some(format!(
                "Bitget 现货账户: {}",
                spot["msg"].as_str().unwrap_or("未知错误")
            ))
        }
        Err(error) => spot_error = Some(format!("Bitget 现货账户: {error}")),
    }

    // 合约账户：USDT 本位全仓账户各交易对返回相同权益，取最大值避免重复累加。
    let futures_path = "/api/v2/mix/account/accounts?productType=USDT-FUTURES";
    let futures_sign = hmac_sha256_base64(
        &credentials.api_secret,
        &format!("{timestamp}GET{futures_path}"),
    )?;
    let futures_headers = [
        ("ACCESS-KEY", credentials.api_key.as_str()),
        ("ACCESS-SIGN", futures_sign.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let mut futures_equity = Decimal::ZERO;
    let mut futures_available = None;
    let mut futures_error = None;
    match http.get_json_with_headers(&format!("{base_url}{futures_path}"), &futures_headers) {
        Ok(futures) if futures["code"].as_str().unwrap_or("00000") == "00000" => {
            if let Some(rows) = futures["data"].as_array() {
                for row in rows {
                    let equity = decimal(row.get("accountEquity").unwrap_or(&Value::Null))
                        .unwrap_or_default();
                    if equity > futures_equity {
                        futures_equity = equity;
                    }
                }
                if futures_equity > Decimal::ZERO {
                    let available = rows
                        .first()
                        .and_then(|row| decimal(row.get("available").unwrap_or(&Value::Null)))
                        .unwrap_or_default();
                    futures_available = Some(available);
                    balances.push((
                        "USDT".to_string(),
                        available,
                        (futures_equity - available).max(Decimal::ZERO),
                    ));
                }
            }
        }
        Ok(futures) => {
            futures_error = Some(format!(
                "Bitget 合约账户: {}",
                futures["msg"].as_str().unwrap_or("未知错误")
            ))
        }
        Err(error) => futures_error = Some(format!("Bitget 合约账户: {error}")),
    }

    // 任一账户查询成功即可导入；两个都失败通常是 Key 权限不足或密钥错误。
    if let (Some(spot), Some(futures)) = (spot_error, futures_error) {
        bail!("{spot}；{futures}。请在 Bitget API Key 中开启现货或合约的读取权限");
    }

    Ok((balances, None, futures_available))
}

fn fetch_gate(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let spot_path = "/api/v4/spot/accounts";
    let spot = gate_get(http, base_url, spot_path, credentials)?;
    let mut balances = Vec::new();
    if let Some(rows) = spot.as_array() {
        for row in rows {
            let asset = row
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let available =
                decimal(row.get("available").unwrap_or(&Value::Null)).unwrap_or_default();
            let locked = decimal(row.get("locked").unwrap_or(&Value::Null)).unwrap_or_default();
            balances.push((asset.to_string(), available, locked));
        }
    }

    let futures_path = "/api/v4/futures/usdt/accounts";
    let mut futures_available = None;
    if let Ok(futures) = gate_get(http, base_url, futures_path, credentials) {
        let total = decimal(&futures["total"]).unwrap_or_default();
        let available = decimal(&futures["available"]).unwrap_or_default();
        if total > Decimal::ZERO {
            futures_available = Some(available);
            balances.push((
                "USDT".to_string(),
                available,
                (total - available).max(Decimal::ZERO),
            ));
        }
    }

    Ok((balances, None, futures_available))
}

/// Aster V3 不再支持 API Key，改为「主账户地址 + 已授权的 API 钱包私钥」。
fn aster_signer(
    credentials: &AccountCredentials,
) -> anyhow::Result<crate::exchange::aster::AsterSigner> {
    let user = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Aster 需要填写主账户钱包地址"))?;
    let key = credentials
        .signer_key
        .as_deref()
        .ok_or_else(|| anyhow!("Aster 需要填写 API 钱包私钥"))?;
    aster::AsterSigner::new(user, key)
}

/// Aster 合约账户信息，签名参数全部拼在查询串上。
fn fetch_aster(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let signer = aster_signer(credentials)?;
    let value = aster::request(http, "GET", base_url, "/fapi/v3/account", &signer, &[])
        .map_err(|error| {
            // 私钥对应的 API 钱包没有在主账户下授权时，服务端只回 No agent found。
            if error.to_string().contains("No agent found") {
                anyhow!(
                    "该私钥对应的 API 钱包未在主账户下授权（No agent found），请到 https://www.asterdex.com/zh-CN/api-wallet 重新授权并确认主账户地址"
                )
            } else {
                error
            }
        })?;

    let rows = value["assets"]
        .as_array()
        .ok_or_else(|| anyhow!(aster::error_of(&value)))?;
    let mut balances = Vec::with_capacity(rows.len());
    for row in rows {
        let asset = row.get("asset").and_then(Value::as_str).unwrap_or_default();
        let wallet = decimal(row.get("walletBalance").unwrap_or(&Value::Null)).unwrap_or_default();
        let available =
            decimal(row.get("availableBalance").unwrap_or(&Value::Null)).unwrap_or_default();
        if wallet > Decimal::ZERO {
            balances.push((
                asset.to_string(),
                available,
                (wallet - available).max(Decimal::ZERO),
            ));
        }
    }

    // 可用保证金取账户口径字段：USDT 的 walletBalance 会因手续费或浮亏为负，
    // 逐资产按「持有」过滤后再累加会把 USDT 整行漏掉，导致显示为 0。
    let available_margin = decimal(&value["availableBalance"]).or_else(|| {
        rows.iter()
            .find(|row| row.get("asset").and_then(Value::as_str) == Some("USDT"))
            .and_then(|row| decimal(row.get("availableBalance").unwrap_or(&Value::Null)))
    });
    let equity = decimal(&value["totalWalletBalance"]).filter(|value| *value > Decimal::ZERO);
    Ok((balances, equity, available_margin))
}

/// Hyperliquid 是否处于统一账户模式（`unifiedAccount` / `portfolioMargin`）。
///
/// 统一账户下现货 USDC 本身就是永续保证金，官方文档说明 `clearinghouseState`
/// 返回的权益对用户没有参考意义；此时若把永续权益与现货余额相加，同一笔 USDC
/// 会被计算两次，余额会明显偏大。读取失败时按非统一账户处理，保持原有口径。
fn hyperliquid_unified_account(http: &HttpClient, base_url: &str, address: &str) -> bool {
    http.post_json(
        &format!("{base_url}/info"),
        &json!({ "type": "userAbstraction", "user": address }),
    )
    .ok()
    .and_then(|value| value.as_str().map(str::to_string))
    .map(|mode| matches!(mode.as_str(), "unifiedAccount" | "portfolioMargin"))
    .unwrap_or(false)
}

/// Hyperliquid 余额查询：永续账户权益走 `clearinghouseState`，现货余额走 `spotClearinghouseState`。
fn fetch_hyperliquid(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let address = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Hyperliquid 需要钱包地址"))?;
    let unified = hyperliquid_unified_account(http, base_url, address);

    let mut available_margin = None;
    let mut balances = Vec::new();
    if !unified {
        let perp = http.post_json(
            &format!("{base_url}/info"),
            &json!({ "type": "clearinghouseState", "user": address }),
        )?;
        // 永续账户的 withdrawable 才是当前可动用的保证金；从未开过永续时它恒为 0，
        // 此时不能当作「可用额度为 0」，可动用额度要看现货 USDC。
        let perp_account_value =
            decimal(&perp["marginSummary"]["accountValue"]).unwrap_or_default();
        if perp_account_value > Decimal::ZERO {
            available_margin = decimal(&perp["withdrawable"]);
            // 永续账户权益本身就是 USDC，按 1:1 计入；标准账户下与现货是两个账本，需要相加。
            balances.push(("USDC".to_string(), perp_account_value, Decimal::ZERO));
        }
    }

    // 现货接口失败时仍然返回永续权益，不整体报错。
    if let Ok(spot) = http.post_json(
        &format!("{base_url}/info"),
        &json!({ "type": "spotClearinghouseState", "user": address }),
    ) {
        if let Some(rows) = spot["balances"].as_array() {
            for row in rows {
                let coin = row.get("coin").and_then(Value::as_str).unwrap_or_default();
                let total = decimal(row.get("total").unwrap_or(&Value::Null)).unwrap_or_default();
                let hold = decimal(row.get("hold").unwrap_or(&Value::Null)).unwrap_or_default();
                if total > Decimal::ZERO {
                    let free = (total - hold).max(Decimal::ZERO);
                    // 统一账户或没有永续权益时，现货可用额就是能立即动用的保证金。
                    if coin == "USDC" && available_margin.is_none() {
                        available_margin = Some(free);
                    }
                    balances.push((coin.to_string(), free, hold));
                }
            }
        }
    }

    Ok((balances, None, available_margin))
}

/// Lighter 余额查询：按 L1 地址或数字账户索引查询全部子账户的资产。
///
/// `assets[].balance` 是现货钱包余额，永续保证金实际记在 `assets[].margin_balance`，
/// 账户权益（含未实现盈亏）只能取 `total_asset_value`；直接对 `balance` 求和会让
/// 有持仓的账户显示成 0 或接近 0，因此这里统一用权益覆盖。
fn fetch_lighter(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let target = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Lighter 需要钱包地址或账户索引"))?;
    let by = if target.chars().all(|ch| ch.is_ascii_digit()) {
        "index"
    } else {
        "l1_address"
    };
    let value = http.get_json(&format!("{base_url}/api/v1/account?by={by}&value={target}"))?;

    if value["code"].as_i64().unwrap_or(0) != 200 {
        bail!("Lighter 返回错误: {}", value["code"]);
    }
    let accounts = value["accounts"]
        .as_array()
        .ok_or_else(|| anyhow!("Lighter 响应缺少 accounts"))?;
    if accounts.is_empty() {
        bail!("Lighter 未找到该钱包的账户");
    }

    let mut equity = Decimal::ZERO;
    let mut available_margin = None;
    for account in accounts {
        let value = decimal(account.get("total_asset_value").unwrap_or(&Value::Null))
            .ok_or_else(|| anyhow!("Lighter 响应缺少 total_asset_value"))?;
        equity += value;
        if let Some(available) = decimal(account.get("available_balance").unwrap_or(&Value::Null)) {
            *available_margin.get_or_insert(Decimal::ZERO) += available;
        }
    }

    // Lighter 的权益本身就是 USDC 计价，按 1:1 计入。
    let free = equity.max(Decimal::ZERO);
    Ok((
        vec![("USDC".to_string(), free, Decimal::ZERO)],
        Some(equity),
        available_margin,
    ))
}

/// 各交易所持仓解析后的统一结构。
struct RawPosition {
    symbol: String,
    side: Side,
    quantity: Decimal,
    entry_price: Decimal,
    mark_price: Decimal,
    leverage: Decimal,
    unrealized_pnl: Decimal,
}

fn fetch_positions(
    http: &HttpClient,
    base_url: &str,
    binance_fapi: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<Position>> {
    let raw = match credentials.venue.as_str() {
        "binance" => positions_binance(http, binance_fapi, credentials)?,
        // Aster 的 V3 持仓接口与 Binance 字段一致，但认证方式不同。
        "aster" => positions_aster(http, base_url, credentials)?,
        "okx" => positions_okx(http, base_url, credentials)?,
        "bybit" => positions_bybit(http, base_url, credentials)?,
        "bitget" => positions_bitget(http, base_url, credentials)?,
        "gate" => positions_gate(http, base_url, credentials)?,
        "hyperliquid" => positions_hyperliquid(http, base_url, credentials)?,
        "lighter" => positions_lighter(http, base_url, credentials)?,
        other => bail!("暂不支持该交易所: {other}"),
    };

    Ok(raw
        .into_iter()
        .filter_map(|item| to_position(credentials.id, &credentials.venue, item))
        .collect())
}

/// 同一账户、同一交易对与方向生成固定 UUID，避免每次轮询都把仓位当成新记录。
fn to_position(account_id: Uuid, venue: &str, raw: RawPosition) -> Option<Position> {
    if raw.quantity <= Decimal::ZERO || raw.symbol.is_empty() {
        return None;
    }
    let side_label = match raw.side {
        Side::Buy => "long",
        Side::Sell => "short",
    };
    let mark_price = if raw.mark_price > Decimal::ZERO {
        raw.mark_price
    } else {
        raw.entry_price
    };
    Some(Position {
        id: Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("{account_id}:{venue}:{}:{side_label}", raw.symbol).as_bytes(),
        ),
        strategy_id: None,
        source: None,
        group_id: None,
        venue: venue.to_string(),
        symbol: raw.symbol,
        side: raw.side,
        quantity: raw.quantity,
        entry_price: raw.entry_price,
        mark_price,
        leverage: if raw.leverage > Decimal::ZERO {
            raw.leverage
        } else {
            Decimal::ONE
        },
        unrealized_pnl: raw.unrealized_pnl,
        updated_at: Utc::now(),
    })
}

/// 统一各交易所的合约名：`BTC-USDT-SWAP` / `BTC_USDT` / `BTC` 都转成 `BTCUSDT`。
fn normalized_symbol(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    let stripped = upper.strip_suffix("-SWAP").unwrap_or(&upper);
    let mut compact = stripped.replace('-', "").replace('_', "");
    if compact.is_empty() {
        return compact;
    }
    if !(compact.ends_with("USDT") || compact.ends_with("USDC") || compact.ends_with("USD")) {
        compact.push_str("USDT");
    }
    compact
}

fn positions_binance(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let base_url = base_url.trim_end_matches('/');
    let query = format!(
        "timestamp={}&recvWindow=5000",
        Utc::now().timestamp_millis()
    );
    let signature = hmac_sha256_hex(&credentials.api_secret, &query)?;
    let url = format!("{base_url}/fapi/v2/positionRisk?{query}&signature={signature}");
    let value =
        http.get_json_with_headers(&url, &[("X-MBX-APIKEY", credentials.api_key.as_str())])?;
    let rows = value.as_array().ok_or_else(|| binance_error(&value))?;
    Ok(parse_binance_positions(rows))
}

/// `/fapi/v3/positionRisk` 的返回字段与 Binance 一致，可直接复用解析逻辑。
fn positions_aster(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let signer = aster_signer(credentials)?;
    let value = aster::request(http, "GET", base_url, "/fapi/v3/positionRisk", &signer, &[])?;
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow!(aster::error_of(&value)))?;
    Ok(parse_binance_positions(rows))
}

fn parse_binance_positions(rows: &[Value]) -> Vec<RawPosition> {
    let mut positions = Vec::new();
    for row in rows {
        let Some(amount) = decimal(&row["positionAmt"]) else {
            continue;
        };
        if amount.is_zero() {
            continue;
        }
        positions.push(RawPosition {
            symbol: normalized_symbol(row["symbol"].as_str().unwrap_or_default()),
            side: if amount > Decimal::ZERO {
                Side::Buy
            } else {
                Side::Sell
            },
            quantity: amount.abs(),
            entry_price: decimal(&row["entryPrice"]).unwrap_or_default(),
            mark_price: decimal(&row["markPrice"]).unwrap_or_default(),
            leverage: decimal(&row["leverage"]).unwrap_or(Decimal::ONE),
            unrealized_pnl: decimal(&row["unRealizedProfit"]).unwrap_or_default(),
        });
    }
    positions
}

fn positions_okx(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let base_url = base_url.trim_end_matches('/');
    let path = "/api/v5/account/positions?instType=SWAP";
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_sha256_base64(&credentials.api_secret, &format!("{timestamp}GET{path}"))?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("OKX 需要 Passphrase"))?;

    let headers = [
        ("OK-ACCESS-KEY", credentials.api_key.as_str()),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http.get_json_with_headers(&format!("{base_url}{path}"), &headers)?;
    if value["code"].as_str().unwrap_or("0") != "0" {
        bail!(
            "OKX 返回错误: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        );
    }

    let mut positions = Vec::new();
    if let Some(rows) = value["data"].as_array() {
        // OKX 的 pos 单位是「张」，必须乘以合约面值 ctVal 才能还原成币数量；
        // 用 notionalUsd / markPx 推导会因名义价值被交易所四舍五入而产生误差（如 0.001 变成 0.000999）。
        let contract_values = okx_swap_contract_values(http, base_url);
        for row in rows {
            let Some(pos) = decimal(&row["pos"]) else {
                continue;
            };
            let mark = decimal(&row["markPx"]).unwrap_or_default();
            let inst_id = row["instId"].as_str().unwrap_or_default();
            let quantity = match contract_values.get(inst_id).copied() {
                Some(ct_val) if ct_val > Decimal::ZERO => pos.abs() * ct_val,
                // 兜底：拿不到合约面值时退回名义价值推导。
                _ => {
                    let notional = decimal(&row["notionalUsd"]).unwrap_or_default().abs();
                    if mark > Decimal::ZERO && notional > Decimal::ZERO {
                        notional / mark
                    } else {
                        pos.abs()
                    }
                }
            };
            let is_long = match row["posSide"].as_str().unwrap_or("net") {
                "long" => true,
                "short" => false,
                _ => pos > Decimal::ZERO,
            };
            positions.push(RawPosition {
                symbol: normalized_symbol(inst_id),
                side: if is_long { Side::Buy } else { Side::Sell },
                quantity,
                entry_price: decimal(&row["avgPx"]).unwrap_or_default(),
                mark_price: mark,
                leverage: decimal(&row["lever"]).unwrap_or(Decimal::ONE),
                unrealized_pnl: decimal(&row["upl"]).unwrap_or_default(),
            });
        }
    }
    Ok(positions)
}

/// 拉取 OKX 全部永续合约面值（instId -> ctVal），供持仓数量换算使用；
/// 请求失败时返回空表，由调用方退回名义价值推导。
fn okx_swap_contract_values(http: &HttpClient, base_url: &str) -> HashMap<String, Decimal> {
    let mut values = HashMap::new();
    let url = format!("{base_url}/api/v5/public/instruments?instType=SWAP");
    let Ok(value) = http.get_json(&url) else {
        return values;
    };
    if value["code"].as_str().unwrap_or("0") != "0" {
        return values;
    }
    if let Some(rows) = value["data"].as_array() {
        for row in rows {
            let Some(inst_id) = row["instId"].as_str() else {
                continue;
            };
            if let Some(ct_val) = decimal(&row["ctVal"]).filter(|value| *value > Decimal::ZERO) {
                values.insert(inst_id.to_string(), ct_val);
            }
        }
    }
    values
}

fn positions_bybit(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let base_url = base_url.trim_end_matches('/');
    let timestamp = Utc::now().timestamp_millis().to_string();
    let recv_window = "5000";
    let query = "category=linear&settleCoin=USDT&limit=200";
    let signature = hmac_sha256_hex(
        &credentials.api_secret,
        &format!("{timestamp}{}{recv_window}{query}", credentials.api_key),
    )?;

    let headers = [
        ("X-BAPI-API-KEY", credentials.api_key.as_str()),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let value =
        http.get_json_with_headers(&format!("{base_url}/v5/position/list?{query}"), &headers)?;
    if value["retCode"].as_i64().unwrap_or(0) != 0 {
        bail!(
            "Bybit 返回错误: {}",
            value["retMsg"].as_str().unwrap_or("未知错误")
        );
    }

    let mut positions = Vec::new();
    if let Some(rows) = value["result"]["list"].as_array() {
        for row in rows {
            let Some(size) = decimal(&row["size"]) else {
                continue;
            };
            if size.is_zero() {
                continue;
            }
            let side = match row["side"].as_str().unwrap_or_default() {
                "Buy" => Side::Buy,
                "Sell" => Side::Sell,
                _ => {
                    if size > Decimal::ZERO {
                        Side::Buy
                    } else {
                        Side::Sell
                    }
                }
            };
            positions.push(RawPosition {
                symbol: normalized_symbol(row["symbol"].as_str().unwrap_or_default()),
                side,
                quantity: size.abs(),
                entry_price: decimal(&row["avgPrice"]).unwrap_or_default(),
                mark_price: decimal(&row["markPrice"]).unwrap_or_default(),
                leverage: decimal(&row["leverage"]).unwrap_or(Decimal::ONE),
                unrealized_pnl: decimal(&row["unrealisedPnl"]).unwrap_or_default(),
            });
        }
    }
    Ok(positions)
}

fn positions_bitget(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let base_url = base_url.trim_end_matches('/');
    let path = "/api/v2/mix/position/all-position?productType=USDT-FUTURES&marginCoin=USDT";
    let timestamp = Utc::now().timestamp_millis().to_string();
    let signature = hmac_sha256_base64(&credentials.api_secret, &format!("{timestamp}GET{path}"))?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("Bitget 需要 Passphrase"))?;

    let headers = [
        ("ACCESS-KEY", credentials.api_key.as_str()),
        ("ACCESS-SIGN", signature.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let value = http.get_json_with_headers(&format!("{base_url}{path}"), &headers)?;
    if value["code"].as_str().unwrap_or("00000") != "00000" {
        bail!(
            "Bitget 返回错误: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        );
    }

    let mut positions = Vec::new();
    if let Some(rows) = value["data"].as_array() {
        for row in rows {
            let Some(total) = decimal(&row["total"]) else {
                continue;
            };
            if total.is_zero() {
                continue;
            }
            positions.push(RawPosition {
                symbol: normalized_symbol(row["symbol"].as_str().unwrap_or_default()),
                side: if row["holdSide"].as_str() == Some("short") {
                    Side::Sell
                } else {
                    Side::Buy
                },
                quantity: total.abs(),
                entry_price: decimal(&row["openPriceAvg"]).unwrap_or_default(),
                mark_price: decimal(&row["markPrice"]).unwrap_or_default(),
                leverage: decimal(&row["leverage"]).unwrap_or(Decimal::ONE),
                unrealized_pnl: decimal(&row["unrealizedPL"]).unwrap_or_default(),
            });
        }
    }
    Ok(positions)
}

fn positions_gate(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let rows = gate_get(
        http,
        base_url.trim_end_matches('/'),
        "/api/v4/futures/usdt/positions",
        credentials,
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();

    let mut positions = Vec::new();
    for row in &rows {
        let size = decimal(&row["size"]).unwrap_or_default();
        if size.is_zero() {
            continue;
        }
        let mark = decimal(&row["mark_price"]).unwrap_or_default();
        // Gate 的持仓数量是张数，`value` 是名义价值，可直接换算成币数量。
        let value = decimal(&row["value"]).unwrap_or_default().abs();
        let quantity = if mark > Decimal::ZERO && value > Decimal::ZERO {
            value / mark
        } else {
            size.abs()
        };
        let mode = row["mode"].as_str().unwrap_or_default();
        let side = if mode.contains("short") {
            Side::Sell
        } else if mode.contains("long") || size > Decimal::ZERO {
            Side::Buy
        } else {
            Side::Sell
        };
        // Gate 全仓模式的 `leverage` 恒为 0，实际杠杆上限在 `cross_leverage_limit`。
        let leverage = decimal(&row["leverage"])
            .filter(|value| *value > Decimal::ZERO)
            .or_else(|| {
                decimal(&row["cross_leverage_limit"]).filter(|value| *value > Decimal::ZERO)
            })
            .unwrap_or(Decimal::ONE);
        positions.push(RawPosition {
            symbol: normalized_symbol(row["contract"].as_str().unwrap_or_default()),
            side,
            quantity,
            entry_price: decimal(&row["entry_price"]).unwrap_or_default(),
            mark_price: mark,
            leverage,
            unrealized_pnl: decimal(&row["unrealised_pnl"]).unwrap_or_default(),
        });
    }
    Ok(positions)
}

fn positions_hyperliquid(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let address = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Hyperliquid 需要钱包地址"))?;
    let value = http.post_json(
        &format!("{}/info", base_url.trim_end_matches('/')),
        &json!({ "type": "clearinghouseState", "user": address }),
    )?;

    let mut positions = Vec::new();
    if let Some(rows) = value["assetPositions"].as_array() {
        for row in rows {
            let position = &row["position"];
            let Some(size) = decimal(&position["szi"]) else {
                continue;
            };
            if size.is_zero() {
                continue;
            }
            let entry = decimal(&position["entryPx"]).unwrap_or_default();
            let notional = decimal(&position["positionValue"])
                .unwrap_or_default()
                .abs();
            let quantity = size.abs();
            let mark = if notional > Decimal::ZERO {
                notional / quantity
            } else {
                entry
            };
            positions.push(RawPosition {
                symbol: normalized_symbol(position["coin"].as_str().unwrap_or_default()),
                side: if size > Decimal::ZERO {
                    Side::Buy
                } else {
                    Side::Sell
                },
                quantity,
                entry_price: entry,
                mark_price: mark,
                leverage: decimal(&position["leverage"]["value"]).unwrap_or(Decimal::ONE),
                unrealized_pnl: decimal(&position["unrealizedPnl"]).unwrap_or_default(),
            });
        }
    }
    Ok(positions)
}

fn positions_lighter(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Vec<RawPosition>> {
    let base_url = base_url.trim_end_matches('/');
    let target = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Lighter 需要钱包地址或账户索引"))?;
    let by = if target.chars().all(|ch| ch.is_ascii_digit()) {
        "index"
    } else {
        "l1_address"
    };
    let value = http.get_json(&format!("{base_url}/api/v1/account?by={by}&value={target}"))?;
    if value["code"].as_i64().unwrap_or(0) != 200 {
        bail!("Lighter 返回错误: {}", value["code"]);
    }

    let mut positions = Vec::new();
    if let Some(accounts) = value["accounts"].as_array() {
        for account in accounts {
            let Some(rows) = account["positions"].as_array() else {
                continue;
            };
            for row in rows {
                let Some(size) = decimal(&row["position"]) else {
                    continue;
                };
                if size.is_zero() {
                    continue;
                }
                let sign =
                    row["sign"]
                        .as_i64()
                        .unwrap_or_else(|| if size < Decimal::ZERO { -1 } else { 1 });
                let quantity = size.abs();
                let position_value = decimal(&row["position_value"]).unwrap_or_default().abs();
                let entry = decimal(&row["avg_entry_price"]).unwrap_or_default();
                let mark = if position_value > Decimal::ZERO {
                    position_value / quantity
                } else {
                    entry
                };
                let leverage = decimal(&row["initial_margin_fraction"])
                    .filter(|value| *value > Decimal::ZERO)
                    .map(|value| Decimal::from(10_000) / value)
                    .unwrap_or(Decimal::ONE);
                positions.push(RawPosition {
                    symbol: normalized_symbol(row["symbol"].as_str().unwrap_or_default()),
                    side: if sign < 0 { Side::Sell } else { Side::Buy },
                    quantity,
                    entry_price: entry,
                    mark_price: mark,
                    leverage,
                    unrealized_pnl: decimal(&row["unrealized_pnl"]).unwrap_or_default(),
                });
            }
        }
    }
    Ok(positions)
}

fn gate_get(
    http: &HttpClient,
    base_url: &str,
    path: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Value> {
    let timestamp = Utc::now().timestamp().to_string();
    // Gate v4 要求：查询串与路径分开，body 哈希与 HMAC 均为 SHA-512。
    let (url_path, query) = path.split_once('?').unwrap_or((path, ""));
    let hashed_payload = hex::encode(Sha512::digest(b""));
    let sign_string = format!("GET\n{url_path}\n{query}\n{hashed_payload}\n{timestamp}");
    let signature = hmac_sha512_hex(&credentials.api_secret, &sign_string)?;
    let headers = [
        ("KEY", credentials.api_key.as_str()),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    http.get_json_with_headers(&format!("{base_url}{path}"), &headers)
}

fn binance_error(value: &Value) -> anyhow::Error {
    anyhow!(
        "Binance 返回错误: {}",
        value["msg"].as_str().unwrap_or("返回格式异常")
    )
}

fn decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::String(text) => text.trim().parse().ok(),
        Value::Number(number) => number.to_string().parse().ok(),
        _ => None,
    }
}

fn hmac_sha256_hex(secret: &str, payload: &str) -> anyhow::Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .context("初始化 HMAC 失败（密钥长度非法）")?;
    mac.update(payload.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn hmac_sha512_hex(secret: &str, payload: &str) -> anyhow::Result<String> {
    let mut mac = Hmac::<Sha512>::new_from_slice(secret.as_bytes())
        .context("初始化 HMAC 失败（密钥长度非法）")?;
    mac.update(payload.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn hmac_sha256_base64(secret: &str, payload: &str) -> anyhow::Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .context("初始化 HMAC 失败（密钥长度非法）")?;
    mac.update(payload.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(account: u128, equity: i64, secs: i64) -> EquityPoint {
        EquityPoint {
            account_id: Uuid::from_u128(account),
            venue: "binance".to_string(),
            equity_usd: Decimal::from(equity),
            observed_at: Utc.timestamp_opt(secs, 0).single().unwrap(),
        }
    }

    #[test]
    fn aggregates_equity_per_bucket() {
        let points = vec![
            point(1, 100, 0),
            point(2, 50, 0),
            point(1, 120, 3600),
            point(2, 80, 3600),
        ];
        let out = aggregate(points, 3600);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].equity_usd, Decimal::from(150));
        assert_eq!(out[1].equity_usd, Decimal::from(200));
    }

    #[test]
    fn preview_masks_key() {
        assert_eq!(preview_key("abcdefghijklmnop"), "abcd••••mnop");
        assert_eq!(preview_key("short"), "••••");
    }

    #[test]
    fn migrates_plaintext_accounts_to_envelope() {
        let dir = std::env::temp_dir().join(format!("arbpilot-accounts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("accounts.json");
        let account = AccountCredentials {
            id: Uuid::from_u128(1),
            venue: "binance".to_string(),
            label: "main".to_string(),
            api_key: "key-1234567890".to_string(),
            api_secret: "secret-value".to_string(),
            passphrase: None,
            wallet_address: None,
            signer_key: None,
            key_index: None,
            account_index: None,
            created_at: Utc::now(),
        };
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&vec![account]).expect("序列化明文账户"),
        )
        .unwrap();

        let cipher = CredentialCipher::open(&path).expect("初始化加密器");
        let (loaded, plaintext) = load_accounts(&path, Some(&cipher));
        assert!(plaintext);
        assert_eq!(loaded.len(), 1);

        write_accounts(&path, &loaded, Some(&cipher)).expect("写入加密凭证");
        let raw = std::fs::read(&path).unwrap();
        assert!(CredentialCipher::is_encrypted(&raw));
        assert!(!String::from_utf8_lossy(&raw).contains("secret-value"));

        let (reloaded, plaintext) = load_accounts(&path, Some(&cipher));
        assert!(!plaintext);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].api_key, "key-1234567890");
    }
}
