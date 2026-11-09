use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    http_client::HttpClient,
    store::{EquityPoint, Store},
};

/// 支持导入 API Key 的 CEX，以及用钱包地址查询、用签名私钥下单的 DEX。
pub const SUPPORTED_VENUES: [&str; 11] = [
    "binance",
    "okx",
    "bybit",
    "bitget",
    "gate",
    "aster",
    "htx",
    "mexc",
    "bingx",
    "hyperliquid",
    "lighter",
];

/// 这些 DEX 用钱包地址（Lighter 也可用账户索引）查询公开接口，用签名私钥下单。
pub const WALLET_VENUES: [&str; 2] = ["hyperliquid", "lighter"];

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
    pub htx: String,
    pub mexc: String,
    pub bingx: String,
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
            "htx" => Some(&self.htx),
            "mexc" => Some(&self.mexc),
            "bingx" => Some(&self.bingx),
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

/// 账户池：凭证文件 + 最新快照 + 权益历史。
pub struct Accounts {
    http: Option<Arc<HttpClient>>,
    endpoints: AccountEndpoints,
    data_file: PathBuf,
    store: Arc<Store>,
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
        let accounts = load_accounts(&data_file);
        let service = Arc::new(Self {
            http,
            endpoints,
            data_file,
            store,
            accounts: RwLock::new(accounts),
            snapshots: RwLock::new(HashMap::new()),
            price_cache: Mutex::new(None),
            last_point: Mutex::new(HashMap::new()),
        });
        service.spawn_poller(poll_secs);
        service
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
    pub fn credentials_for(&self, venue: &str) -> Option<AccountCredentials> {
        let accounts = self.accounts.read().expect("账户锁不可用");
        accounts
            .iter()
            .find(|account| account.venue == venue && has_trading_credentials(account))
            .or_else(|| accounts.iter().find(|account| account.venue == venue))
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
            Ok(Ok((equity, balances))) => AccountSnapshotView {
                venue,
                label,
                equity_usd: equity,
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
        write_accounts(&self.data_file, &accounts)
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

fn load_accounts(path: &Path) -> Vec<AccountCredentials> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "读取账户文件失败");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Vec<AccountCredentials>>(&raw) {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "解析账户文件失败，忽略已有账户");
            Vec::new()
        }
    }
}

fn write_accounts(path: &Path, accounts: &[AccountCredentials]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建账户目录失败: {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(accounts).context("序列化账户失败")?;
    std::fs::write(path, json).with_context(|| format!("写入账户文件失败: {}", path.display()))?;
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
) -> anyhow::Result<(Decimal, Vec<AssetBalance>)> {
    let (raw, override_equity) = match credentials.venue.as_str() {
        "binance" => fetch_binance(http, base_url, binance_fapi, credentials)?,
        "okx" => fetch_okx(http, base_url, credentials)?,
        "bybit" => fetch_bybit(http, base_url, credentials)?,
        "bitget" => fetch_bitget(http, base_url, credentials)?,
        "gate" => fetch_gate(http, base_url, credentials)?,
        "aster" => fetch_aster(http, base_url, credentials)?,
        "htx" => fetch_htx(http, base_url, credentials)?,
        "mexc" => fetch_mexc(http, base_url, credentials)?,
        "bingx" => fetch_bingx(http, base_url, credentials)?,
        "hyperliquid" => fetch_hyperliquid(http, base_url, credentials)?,
        "lighter" => fetch_lighter(http, base_url, credentials)?,
        other => bail!("暂不支持该交易所: {other}"),
    };

    let (valued, balances) = value_balances(raw, prices);
    Ok((override_equity.unwrap_or(valued), balances))
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
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
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
            for row in rows {
                let asset = row.get("asset").and_then(Value::as_str).unwrap_or_default();
                let wallet =
                    decimal(row.get("walletBalance").unwrap_or(&Value::Null)).unwrap_or_default();
                let available = decimal(row.get("availableBalance").unwrap_or(&Value::Null))
                    .unwrap_or_default();
                if wallet > Decimal::ZERO {
                    balances.push((asset.to_string(), available, wallet - available));
                }
            }
        }
    }

    Ok((balances, None))
}

fn fetch_okx(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
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
    if let Some(details) = account["details"].as_array() {
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
            balances.push((
                asset.to_string(),
                available,
                (total - available).max(Decimal::ZERO),
            ));
        }
    }
    Ok((balances, equity))
}

fn fetch_bybit(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
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
    Ok((balances, equity))
}

fn fetch_bitget(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| anyhow!("Bitget 需要 Passphrase"))?;
    let timestamp = Utc::now().timestamp_millis().to_string();

    // 现货资产
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
    let spot = http.get_json_with_headers(&format!("{base_url}{spot_path}"), &spot_headers)?;
    if spot["code"].as_str().unwrap_or("00000") != "00000" {
        bail!(
            "Bitget 返回错误: {}",
            spot["msg"].as_str().unwrap_or("未知错误")
        );
    }

    let mut balances = Vec::new();
    if let Some(rows) = spot["data"].as_array() {
        for row in rows {
            let asset = row.get("coin").and_then(Value::as_str).unwrap_or_default();
            let available =
                decimal(row.get("available").unwrap_or(&Value::Null)).unwrap_or_default();
            let frozen = decimal(row.get("frozen").unwrap_or(&Value::Null)).unwrap_or_default();
            balances.push((asset.to_string(), available, frozen));
        }
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
    if let Ok(futures) =
        http.get_json_with_headers(&format!("{base_url}{futures_path}"), &futures_headers)
    {
        if let Some(rows) = futures["data"].as_array() {
            for row in rows {
                let equity =
                    decimal(row.get("accountEquity").unwrap_or(&Value::Null)).unwrap_or_default();
                if equity > futures_equity {
                    futures_equity = equity;
                }
            }
            if futures_equity > Decimal::ZERO {
                let available = rows
                    .first()
                    .and_then(|row| decimal(row.get("available").unwrap_or(&Value::Null)))
                    .unwrap_or_default();
                balances.push((
                    "USDT".to_string(),
                    available,
                    (futures_equity - available).max(Decimal::ZERO),
                ));
            }
        }
    }

    Ok((balances, None))
}

fn fetch_gate(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
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
    if let Ok(futures) = gate_get(http, base_url, futures_path, credentials) {
        let total = decimal(&futures["total"]).unwrap_or_default();
        let available = decimal(&futures["available"]).unwrap_or_default();
        if total > Decimal::ZERO {
            balances.push((
                "USDT".to_string(),
                available,
                (total - available).max(Decimal::ZERO),
            ));
        }
    }

    Ok((balances, None))
}

/// Aster 合约私有接口与 Binance 完全兼容，签名方式一致。
fn fetch_aster(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let query = format!(
        "timestamp={}&recvWindow=5000",
        Utc::now().timestamp_millis()
    );
    let signature = hmac_sha256_hex(&credentials.api_secret, &query)?;
    let url = format!("{base_url}/fapi/v2/account?{query}&signature={signature}");
    let value =
        http.get_json_with_headers(&url, &[("X-MBX-APIKEY", credentials.api_key.as_str())])?;

    let rows = value["assets"]
        .as_array()
        .ok_or_else(|| binance_error(&value))?;
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

    let equity = decimal(&value["totalWalletBalance"]).filter(|value| *value > Decimal::ZERO);
    Ok((balances, equity))
}

/// Hyperliquid 余额查询：永续账户权益走 `clearinghouseState`，现货余额走 `spotClearinghouseState`。
fn fetch_hyperliquid(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let address = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| anyhow!("Hyperliquid 需要钱包地址"))?;

    let perp = http.post_json(
        &format!("{base_url}/info"),
        &json!({ "type": "clearinghouseState", "user": address }),
    )?;
    let mut balances = Vec::new();
    if let Some(account_value) = decimal(&perp["marginSummary"]["accountValue"]) {
        if account_value > Decimal::ZERO {
            // 永续账户权益本身就是 USDC，按 1:1 计入。
            balances.push(("USDC".to_string(), account_value, Decimal::ZERO));
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
                    balances.push((coin.to_string(), (total - hold).max(Decimal::ZERO), hold));
                }
            }
        }
    }

    Ok((balances, None))
}

/// Lighter 余额查询：按 L1 地址或数字账户索引查询全部子账户的资产。
fn fetch_lighter(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
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

    let mut balances = Vec::new();
    for account in accounts {
        let Some(assets) = account["assets"].as_array() else {
            continue;
        };
        for asset in assets {
            let symbol = asset
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let balance = decimal(asset.get("balance").unwrap_or(&Value::Null)).unwrap_or_default();
            let locked =
                decimal(asset.get("locked_balance").unwrap_or(&Value::Null)).unwrap_or_default();
            if balance > Decimal::ZERO {
                balances.push((
                    symbol.to_string(),
                    (balance - locked).max(Decimal::ZERO),
                    locked,
                ));
            }
        }
    }

    Ok((balances, None))
}

/// HTX（火币）U 本位永续：只读取全仓保证金账户的 USDT 权益。
fn fetch_htx(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let path = "/linear-swap-api/v1/swap_cross_account_info";
    let value = htx_get(http, base_url, path, &[], credentials)?;
    if value["status"].as_str() != Some("ok") {
        bail!("HTX 返回错误: {}", htx_error(&value));
    }

    let account = &value["data"][0];
    let equity = decimal(&account["margin_balance"]).unwrap_or_default();
    let available = decimal(&account["margin_available"]).unwrap_or_default();
    let mut balances = Vec::new();
    if equity > Decimal::ZERO {
        balances.push((
            "USDT".to_string(),
            available,
            (equity - available).max(Decimal::ZERO),
        ));
    }

    Ok((balances, None))
}

/// MEXC 合约账户：私有接口用 `ApiKey + 时间戳 + 参数串` 做 HMAC-SHA256。
fn fetch_mexc(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let value = mexc_get(
        http,
        base_url,
        "/api/v1/private/account/assets",
        "",
        credentials,
    )?;
    if value["success"].as_bool() != Some(true) {
        bail!(
            "MEXC 返回错误: {}",
            value["message"].as_str().unwrap_or("未知错误")
        );
    }

    let rows = value["data"]
        .as_array()
        .ok_or_else(|| anyhow!("MEXC 响应缺少 data"))?;
    let mut balances = Vec::with_capacity(rows.len());
    for row in rows {
        let currency = row
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let equity = decimal(&row["equity"]).unwrap_or_default();
        let available = decimal(&row["availableBalance"]).unwrap_or_default();
        if equity > Decimal::ZERO {
            balances.push((
                currency.to_string(),
                available,
                (equity - available).max(Decimal::ZERO),
            ));
        }
    }

    Ok((balances, None))
}

/// BingX 永续账户：签名是 `timestamp` 参数串的 HMAC-SHA256，放在 query 上。
fn fetch_bingx(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<(RawBalances, Option<Decimal>)> {
    let base_url = base_url.trim_end_matches('/');
    let path = "/openApi/swap/v2/user/balance";
    let timestamp = Utc::now().timestamp_millis();
    let params = format!("timestamp={timestamp}");
    let signature = hmac_sha256_hex(&credentials.api_secret, &params)?;
    let value = http.get_json_with_headers(
        &format!("{base_url}{path}?{params}&signature={signature}"),
        &[("X-BX-APIKEY", credentials.api_key.as_str())],
    )?;
    if value["code"].as_i64().unwrap_or(-1) != 0 {
        bail!(
            "BingX 返回错误: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        );
    }

    let balance = &value["data"]["balance"];
    let asset = balance["asset"].as_str().unwrap_or("USDT").to_string();
    let total = decimal(&balance["balance"]).unwrap_or_default();
    let available = decimal(&balance["availableMargin"]).unwrap_or_default();
    let mut balances = Vec::new();
    if total > Decimal::ZERO {
        balances.push((asset, available, (total - available).max(Decimal::ZERO)));
    }

    Ok((balances, None))
}

fn gate_get(
    http: &HttpClient,
    base_url: &str,
    path: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Value> {
    let timestamp = Utc::now().timestamp().to_string();
    let hashed_payload = hex::encode(Sha256::digest(b""));
    let sign_string = format!("GET\n{path}\n\n{hashed_payload}\n{timestamp}");
    let signature = hmac_sha256_hex(&credentials.api_secret, &sign_string)?;
    let headers = [
        ("KEY", credentials.api_key.as_str()),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    http.get_json_with_headers(&format!("{base_url}{path}"), &headers)
}

/// HTX 签名查询串：参数按编码后的键排序，签名覆盖 `方法\n主机\n路径\n参数串`。
fn htx_get(
    http: &HttpClient,
    base_url: &str,
    path: &str,
    params: &[(&str, &str)],
    credentials: &AccountCredentials,
) -> anyhow::Result<Value> {
    let base_url = base_url.trim_end_matches('/');
    let host = base_url.split("://").nth(1).unwrap_or(base_url);
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    let mut all: Vec<(&str, &str)> = vec![
        ("AccessKeyId", credentials.api_key.as_str()),
        ("SignatureMethod", "HmacSHA256"),
        ("SignatureVersion", "2"),
        ("Timestamp", timestamp.as_str()),
    ];
    all.extend_from_slice(params);
    all.sort_by(|left, right| left.0.cmp(right.0));

    let query = all
        .iter()
        .map(|(name, value)| format!("{}={}", htx_encode(name), htx_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let signature = hmac_sha256_base64(
        &credentials.api_secret,
        &format!("GET\n{host}\n{path}\n{query}"),
    )?;
    let url = format!(
        "{base_url}{path}?{query}&Signature={}",
        htx_encode(&signature)
    );
    http.get_json(&url)
}

fn htx_error(value: &Value) -> String {
    value["err-msg"]
        .as_str()
        .or_else(|| value["err_msg"].as_str())
        .unwrap_or("未知错误")
        .to_string()
}

/// MEXC 合约私有接口：签名 = HMAC-SHA256(secret, apiKey + 毫秒时间戳 + 参数串)。
fn mexc_get(
    http: &HttpClient,
    base_url: &str,
    path: &str,
    query: &str,
    credentials: &AccountCredentials,
) -> anyhow::Result<Value> {
    let timestamp = Utc::now().timestamp_millis().to_string();
    let signature = hmac_sha256_hex(
        &credentials.api_secret,
        &format!("{}{}{}", credentials.api_key, timestamp, query),
    )?;
    let headers = [
        ("ApiKey", credentials.api_key.as_str()),
        ("Request-Time", timestamp.as_str()),
        ("Signature", signature.as_str()),
        ("Recv-Window", "10"),
    ];
    let url = if query.is_empty() {
        format!("{base_url}{path}")
    } else {
        format!("{base_url}{path}?{query}")
    };
    http.get_json_with_headers(&url, &headers)
}

/// RFC3986 编码，交易所签名对空格、斜杠等字符的编码方式要求一致。
fn htx_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
}
