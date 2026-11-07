use std::{
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

use anyhow::Context;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{error::ApiError, state::AppState};

/// 账号服务签发的授权令牌前缀；格式为 `arb1.<claims>.<signature>`。
const TOKEN_PREFIX: &str = "arb1";
/// Ed25519 SubjectPublicKeyInfo 的 DER 前缀，用于从 SPKI 里取出 32 字节原始公钥。
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseClaims {
    /// 账号 ID。
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    /// 已开通的套餐，如 `arbitrage` / `strategy`。
    #[serde(default)]
    pub plans: Vec<String>,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub iat: i64,
    #[serde(default)]
    pub exp: i64,
    #[serde(default = "default_version")]
    pub v: u32,
}

fn default_status() -> String {
    "none".to_string()
}

fn default_version() -> u32 {
    1
}

/// 返回给桌面端的授权状态，不含签名本身。
#[derive(Debug, Clone, Serialize)]
pub struct LicenseStatus {
    /// 执行器是否要求有效授权（ARBPILOT_LICENSE_REQUIRED）。
    pub required: bool,
    /// 是否已安装令牌。
    pub installed: bool,
    /// 是否仍在有效期内（含离线宽限期）。
    pub valid: bool,
    /// 已过期但仍处于宽限期内。
    pub in_grace: bool,
    pub plans: Vec<String>,
    /// active / canceling / expired / none。
    pub status: String,
    pub email: Option<String>,
    pub subject: Option<String>,
    pub device: Option<String>,
    pub expires_at: Option<i64>,
    pub grace_until: Option<i64>,
    /// 读取或校验令牌时的错误，便于桌面端提示。
    pub error: Option<String>,
}

/// 校验并保存授权令牌。
///
/// 令牌由官方账号服务用 Ed25519 私钥签名，执行器只用公钥离线验证，
/// 不需要联网、也拿不到任何交易凭证。
pub struct LicenseManager {
    required: bool,
    public_key: Option<Vec<u8>>,
    grace_secs: i64,
    file: PathBuf,
    claims: RwLock<Option<LicenseClaims>>,
    error: RwLock<Option<String>>,
}

impl LicenseManager {
    pub fn start(
        required: bool,
        public_key: Option<&str>,
        grace_hours: u64,
        file: PathBuf,
    ) -> Self {
        let public_key = public_key.and_then(|raw| match decode_public_key(raw) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::error!(%error, "ARBPILOT_LICENSE_PUBLIC_KEY 无效，授权校验将全部失败");
                None
            }
        });
        if required && public_key.is_none() {
            tracing::warn!(
                "已开启授权校验（ARBPILOT_LICENSE_REQUIRED=true）但未配置 ARBPILOT_LICENSE_PUBLIC_KEY，付费模块将无法访问"
            );
        }

        let manager = Self {
            required,
            public_key,
            grace_secs: (grace_hours as i64) * 3_600,
            file,
            claims: RwLock::new(None),
            error: RwLock::new(None),
        };

        if let Some(token) = read_saved_token(&manager.file) {
            match manager.install(&token) {
                Ok(claims) => tracing::info!(
                    subject = %claims.sub,
                    plans = ?claims.plans,
                    exp = claims.exp,
                    "已恢复本地授权令牌"
                ),
                Err(error) => tracing::warn!(%error, "本地授权令牌已失效，需要重新登录桌面端"),
            }
        }

        manager
    }

    pub fn required(&self) -> bool {
        self.required
    }

    /// 校验并保存令牌；失败时保留原令牌不变。
    pub fn install(&self, token: &str) -> Result<LicenseClaims, String> {
        match self.verify_token(token) {
            Ok(claims) => {
                if let Err(error) = persist_token(&self.file, token.trim()) {
                    tracing::warn!(%error, "授权令牌写入本地失败，重启后需要重新登录");
                }
                *self.claims.write().expect("授权令牌锁不可用") = Some(claims.clone());
                *self.error.write().expect("授权令牌锁不可用") = None;
                Ok(claims)
            }
            Err(error) => {
                *self.error.write().expect("授权令牌锁不可用") = Some(error.clone());
                Err(error)
            }
        }
    }

    fn verify_token(&self, token: &str) -> Result<LicenseClaims, String> {
        let Some(key) = self.public_key.as_deref() else {
            return Err("执行器未配置 ARBPILOT_LICENSE_PUBLIC_KEY，无法校验授权令牌".to_string());
        };
        verify(token, key)
    }

    /// 校验请求携带的用户令牌，不落盘、不影响整机授权。
    pub fn verify_claims(&self, token: &str) -> Result<LicenseClaims, String> {
        self.verify_token(token)
    }

    pub fn clear(&self) {
        *self.claims.write().expect("授权令牌锁不可用") = None;
        *self.error.write().expect("授权令牌锁不可用") = None;
        if let Err(error) = fs::remove_file(&self.file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %self.file.display(), "删除本地授权令牌失败");
            }
        }
    }

    pub fn claims(&self) -> Option<LicenseClaims> {
        self.claims.read().expect("授权令牌锁不可用").clone()
    }

    /// 授权是否仍然有效；过期后在宽限期内仍允许离线运行。
    pub fn is_valid(&self) -> bool {
        self.claims().is_some_and(|claims| self.claims_valid(&claims))
    }

    fn claims_valid(&self, claims: &LicenseClaims) -> bool {
        if !matches!(claims.status.as_str(), "active" | "canceling") {
            return false;
        }
        !self.expired(claims)
    }

    /// 是否已过有效期（宽限期结束）；过期令牌仍可用于提示用户重新登录。
    pub fn expired(&self, claims: &LicenseClaims) -> bool {
        claims.exp + self.grace_secs <= Utc::now().timestamp()
    }

    /// 判断某个套餐是否被授权；`strategy` 包含 `arbitrage` 的权益。
    pub fn allows(&self, plan: &str) -> bool {
        match self.claims() {
            Some(claims) => self.allows_claims(&claims, plan),
            None => !self.required,
        }
    }

    /// 按某份令牌判断套餐；用于托管实例的按用户校验。
    pub fn allows_claims(&self, claims: &LicenseClaims, plan: &str) -> bool {
        if !self.required {
            return true;
        }
        if !self.claims_valid(claims) {
            return false;
        }
        claims.plans.iter().any(|item| item == plan)
            || (plan == "arbitrage" && claims.plans.iter().any(|item| item == "strategy"))
    }

    pub fn status(&self) -> LicenseStatus {
        let claims = self.claims();
        self.status_of(claims.as_ref(), self.error.read().expect("授权令牌锁不可用").clone())
    }

    /// 请求携带用户令牌时的授权状态；不暴露整机安装的那份授权。
    pub fn status_for(&self, claims: &LicenseClaims) -> LicenseStatus {
        self.status_of(Some(claims), None)
    }

    /// 未携带用户令牌时的空授权；托管实例不暴露整机授权。
    pub fn anonymous_status(&self) -> LicenseStatus {
        self.status_of(None, None)
    }

    fn status_of(&self, claims: Option<&LicenseClaims>, error: Option<String>) -> LicenseStatus {
        let now = Utc::now().timestamp();
        let expired = claims.is_some_and(|item| self.expired(item));
        let beyond_expiry = claims.is_some_and(|item| item.exp <= now);
        let valid = claims.is_some_and(|item| self.claims_valid(item));
        LicenseStatus {
            required: self.required,
            installed: claims.is_some(),
            valid,
            in_grace: beyond_expiry && !expired && valid,
            plans: claims.map(|item| item.plans.clone()).unwrap_or_default(),
            status: if expired {
                "expired".to_string()
            } else {
                claims
                    .map(|item| item.status.clone())
                    .unwrap_or_else(|| "none".to_string())
            },
            email: claims.and_then(|item| item.email.clone()),
            subject: claims.map(|item| item.sub.clone()),
            device: claims.and_then(|item| item.device.clone()),
            expires_at: claims.map(|item| item.exp),
            grace_until: claims.map(|item| item.exp + self.grace_secs),
            error,
        }
    }
}

/// 校验 `arb1.<payload>.<signature>` 的签名并解析出授权内容。
pub fn verify(token: &str, public_key: &[u8]) -> Result<LicenseClaims, String> {
    let mut parts = token.trim().split('.');
    let prefix = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if parts.next().is_some() || payload.is_empty() || signature.is_empty() {
        return Err("授权令牌格式无效".to_string());
    }
    if prefix != TOKEN_PREFIX {
        return Err("授权令牌版本不受支持".to_string());
    }

    let signature = decode_base64(signature)?;
    let message = format!("{TOKEN_PREFIX}.{payload}");
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message.as_bytes(), &signature)
        .map_err(|_| "授权令牌签名校验失败".to_string())?;

    let payload = decode_base64(payload)?;
    serde_json::from_slice(&payload).map_err(|error| format!("授权令牌内容无效: {error}"))
}

/// 接受 32 字节原始公钥或 base64(SPKI DER)，返回原始公钥。
fn decode_public_key(raw: &str) -> Result<Vec<u8>, String> {
    let decoded = decode_base64(raw.trim())?;
    if decoded.len() == 32 {
        return Ok(decoded);
    }
    if decoded.len() == 44 && decoded.starts_with(&ED25519_SPKI_PREFIX) {
        return Ok(decoded[ED25519_SPKI_PREFIX.len()..].to_vec());
    }
    Err("公钥应为 32 字节原始键或 base64(SPKI DER)".to_string())
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(value) {
            return Ok(bytes);
        }
    }
    Err("base64 解码失败".to_string())
}

fn read_saved_token(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("token")
        .and_then(|token| token.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn persist_token(path: &Path, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("创建 {}", parent.display()))?;
        }
    }
    let body = serde_json::json!({ "token": token, "saved_at": Utc::now().to_rfc3339() });
    fs::write(path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("写入 {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 各路由所属套餐；返回 None 表示免费模块，不校验授权。
pub fn required_plan(path: &str) -> Option<&'static str> {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    if path.starts_with("/opportunities")
        || path.starts_with("/funding")
        || path.starts_with("/news")
    {
        return Some("arbitrage");
    }
    if path.starts_with("/strategies")
        || path.starts_with("/sandbox")
        || path.starts_with("/market")
    {
        return Some("strategy");
    }
    None
}

/// 请求头/查询参数里携带用户授权令牌的字段名；WebSocket 只能用查询参数。
pub const LICENSE_HEADER: &str = "x-arbpilot-license";

/// 中间件：校验请求携带的用户令牌并写入请求上下文。
///
/// 托管实例据此按用户判定套餐；自托管未携带时继续使用整机授权。
pub async fn attach_identity(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get(LICENSE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| query_param(request.uri().query(), "license"));
    if let Some(token) = token {
        let claims = state
            .license
            .verify_claims(&token)
            .map_err(|error| ApiError::UnauthorizedWith(format!("授权令牌无效: {error}")))?;
        request.extensions_mut().insert(claims);
    }
    Ok(next.run(request).await)
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key && !value.is_empty()).then(|| value.to_string())
    })
}

/// 中间件：按套餐限制付费模块。未开启校验时直接放行。
pub async fn require_plan(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.license.required() {
        return Ok(next.run(request).await);
    }
    let Some(plan) = required_plan(request.uri().path()) else {
        return Ok(next.run(request).await);
    };
    let identity = request.extensions().get::<LicenseClaims>().cloned();
    let allowed = match identity.as_ref() {
        Some(claims) => state.license.allows_claims(claims, plan),
        // 托管实例按请求里的用户身份计费，不回退到整机授权。
        None if state.config.public_mode => false,
        None => state.license.allows(plan),
    };
    if allowed {
        return Ok(next.run(request).await);
    }

    let message = match identity.as_ref() {
        None if state.config.public_mode => {
            return Err(ApiError::UnauthorizedWith(
                "官方托管实例需要先登录 ArbPilot 账号并同步授权".to_string(),
            ));
        }
        None => {
            let status = state.license.status();
            if !status.installed {
                "付费模块需要先登录 ArbPilot 账号，授权令牌尚未同步到执行器"
            } else if !status.valid {
                "授权令牌已过期，请在桌面端重新登录"
            } else {
                "当前套餐不包含该模块"
            }
        }
        Some(claims) => {
            if state.license.expired(claims) {
                "授权令牌已过期，请在桌面端重新登录"
            } else if !matches!(claims.status.as_str(), "active" | "canceling") {
                "账号订阅未生效，请检查订阅状态"
            } else {
                "当前套餐不包含该模块"
            }
        }
    };
    Err(ApiError::PaymentRequired(message.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_mapping_covers_paid_modules() {
        assert_eq!(required_plan("/api/v1/funding/status"), Some("arbitrage"));
        assert_eq!(required_plan("/api/v1/news"), Some("arbitrage"));
        assert_eq!(required_plan("/api/v1/strategies/abc"), Some("strategy"));
        assert_eq!(required_plan("/api/v1/market/leaderboard"), Some("strategy"));
        assert_eq!(required_plan("/api/v1/orders"), None);
        assert_eq!(required_plan("/api/v1/license"), None);
    }
}
