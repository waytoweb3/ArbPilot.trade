use std::sync::Arc;

use arb_core::{ExchangeError, PlaceOrderRequest, Side, VenueOrderAck};
use base64::Engine as _;
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};

use crate::{
    accounts::{AccountCredentials, Accounts},
    exchange::{
        aster,
        dex::{
            hyperliquid_cloid, hyperliquid_update_leverage_action, sign_hyperliquid_action,
            sign_lighter_leverage, sign_lighter_order, HyperliquidOrder, LighterLeverage,
            LighterOrder, LIGHTER_MAINNET_CHAIN_ID,
        },
    },
    http_client::{HttpClient, HttpStatusError},
};

/// 实盘下单使用的各交易所私有接口根地址，复用行情配置里的域名。
#[derive(Debug, Clone)]
pub struct LiveEndpoints {
    pub binance: String,
    pub okx: String,
    pub bybit: String,
    pub bitget: String,
    pub gate: String,
    pub aster: String,
    pub htx: String,
    pub mexc: String,
    pub bingx: String,
    /// Hyperliquid 与 Lighter 的接口根地址，签名在本机完成后提交。
    pub hyperliquid: String,
    pub lighter: String,
}

/// 实盘交易所路由：按合约腿所属交易所取已导入的凭证，调用对应私有下单接口。
pub struct LiveExchange {
    http: Option<Arc<HttpClient>>,
    endpoints: LiveEndpoints,
    accounts: Arc<Accounts>,
}

impl LiveExchange {
    pub fn new(
        http: Option<Arc<HttpClient>>,
        endpoints: LiveEndpoints,
        accounts: Arc<Accounts>,
    ) -> Self {
        Self {
            http,
            endpoints,
            accounts,
        }
    }

    /// 向指定交易所提交一张限价单，返回交易所订单号。
    pub async fn place_order(
        &self,
        venue: &str,
        request: PlaceOrderRequest,
    ) -> Result<VenueOrderAck, ExchangeError> {
        let http = self.http.clone().ok_or_else(|| {
            ExchangeError::Transport("HTTP 客户端不可用，无法提交实盘订单".to_string())
        })?;
        let venue = venue.trim().to_ascii_lowercase();
        let credentials = self.accounts.credentials_for(&venue).ok_or_else(|| {
            ExchangeError::Credentials(format!("{venue} 尚未导入账户，无法提交实盘订单"))
        })?;
        let endpoints = self.endpoints.clone();

        tokio::task::spawn_blocking(move || {
            place(&http, &endpoints, &venue, &credentials, &request)
        })
        .await
        .map_err(|error| ExchangeError::Transport(format!("下单任务失败: {error}")))?
    }

    /// 在下单前把杠杆设置到交易所。
    ///
    /// 只有调用方显式给出杠杆时才需要调用；MEXC / HTX 的杠杆随下单请求提交，
    /// 这里直接返回成功。
    pub async fn set_leverage(
        &self,
        venue: &str,
        symbol: &str,
        leverage: Decimal,
    ) -> Result<(), ExchangeError> {
        if leverage <= Decimal::ZERO {
            return Err(ExchangeError::Rejected("杠杆必须大于 0".to_string()));
        }
        let http = self.http.clone().ok_or_else(|| {
            ExchangeError::Transport("HTTP 客户端不可用，无法设置杠杆".to_string())
        })?;
        let venue = venue.trim().to_ascii_lowercase();
        let credentials = self.accounts.credentials_for(&venue).ok_or_else(|| {
            ExchangeError::Credentials(format!("{venue} 尚未导入账户，无法设置杠杆"))
        })?;
        let endpoints = self.endpoints.clone();
        let symbol = symbol.to_string();

        tokio::task::spawn_blocking(move || {
            set_leverage_blocking(&http, &endpoints, &venue, &credentials, &symbol, leverage)
        })
        .await
        .map_err(|error| ExchangeError::Transport(format!("设置杠杆任务失败: {error}")))?
    }

    /// 查询交易所当前为该合约设置的杠杆。
    ///
    /// 部分交易所没有按合约查询杠杆的接口，此时返回 `None`，由调用方回退到持仓里的杠杆。
    pub async fn current_leverage(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Option<Decimal>, ExchangeError> {
        let Some(http) = self.http.clone() else {
            return Ok(None);
        };
        let venue = venue.trim().to_ascii_lowercase();
        let Some(credentials) = self.accounts.credentials_for(&venue) else {
            return Ok(None);
        };
        let endpoints = self.endpoints.clone();
        let symbol = symbol.to_string();

        tokio::task::spawn_blocking(move || {
            current_leverage_blocking(&http, &endpoints, &venue, &credentials, &symbol)
        })
        .await
        .map_err(|error| ExchangeError::Transport(format!("查询杠杆任务失败: {error}")))?
    }
}

fn place(
    http: &HttpClient,
    endpoints: &LiveEndpoints,
    venue: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
) -> Result<VenueOrderAck, ExchangeError> {
    if request.quantity <= Decimal::ZERO || request.limit_price <= Decimal::ZERO {
        return Err(ExchangeError::Rejected(
            "下单数量与限价必须大于 0".to_string(),
        ));
    }
    let client_order_id = client_id(&request.client_order_id);
    match venue {
        "binance" => binance_place(
            http,
            &endpoints.binance,
            credentials,
            request,
            &client_order_id,
        ),
        // Aster V3 用主账户地址 + API 钱包私钥签名，接口与字段仍是 Binance 风格。
        "aster" => aster_place(
            http,
            &endpoints.aster,
            credentials,
            request,
            &client_order_id,
        ),
        "okx" => okx_place(http, &endpoints.okx, credentials, request, &client_order_id),
        "bybit" => bybit_place(
            http,
            &endpoints.bybit,
            credentials,
            request,
            &client_order_id,
        ),
        "bitget" => bitget_place(
            http,
            &endpoints.bitget,
            credentials,
            request,
            &client_order_id,
        ),
        "gate" => gate_place(
            http,
            &endpoints.gate,
            credentials,
            request,
            &client_order_id,
        ),
        "htx" => htx_place(http, &endpoints.htx, credentials, request, &client_order_id),
        "mexc" => mexc_place(
            http,
            &endpoints.mexc,
            credentials,
            request,
            &client_order_id,
        ),
        "bingx" => bingx_place(
            http,
            &endpoints.bingx,
            credentials,
            request,
            &client_order_id,
        ),
        "hyperliquid" => hyperliquid_place(
            http,
            &endpoints.hyperliquid,
            credentials,
            request,
            &client_order_id,
        ),
        "lighter" => lighter_place(
            http,
            &endpoints.lighter,
            credentials,
            request,
            &client_order_id,
        ),
        other => Err(ExchangeError::Unsupported(format!(
            "{other} 暂未接入实盘下单"
        ))),
    }
}

/// 各交易所的杠杆设置入口。`symbol` 为内部交易对（如 `BTCUSDT`）。
///
/// MEXC / HTX 把杠杆放在下单请求里，这里无需额外请求。
fn set_leverage_blocking(
    http: &HttpClient,
    endpoints: &LiveEndpoints,
    venue: &str,
    credentials: &AccountCredentials,
    symbol: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let symbol = symbol.trim().to_ascii_uppercase();
    let base = symbol
        .strip_suffix("USDT")
        .filter(|base| !base.is_empty())
        .ok_or_else(|| {
            ExchangeError::Unsupported(format!("仅支持 USDT 本位永续: {symbol}"))
        })?;
    match venue {
        "binance" => binance_leverage(http, &endpoints.binance, credentials, &symbol, leverage),
        "aster" => aster_leverage(http, &endpoints.aster, credentials, &symbol, leverage),
        "okx" => okx_leverage(http, &endpoints.okx, credentials, base, leverage),
        "bybit" => bybit_leverage(http, &endpoints.bybit, credentials, &symbol, leverage),
        "bitget" => bitget_leverage(http, &endpoints.bitget, credentials, &symbol, leverage),
        "gate" => gate_leverage(http, &endpoints.gate, credentials, base, leverage),
        "bingx" => bingx_leverage(http, &endpoints.bingx, credentials, base, leverage),
        "hyperliquid" => {
            hyperliquid_leverage(http, &endpoints.hyperliquid, credentials, base, leverage)
        }
        "lighter" => lighter_leverage(http, &endpoints.lighter, credentials, base, leverage),
        // 杠杆随下单请求提交（见 `mexc_place` / `htx_place`）。
        "mexc" | "htx" => Ok(()),
        other => Err(ExchangeError::Unsupported(format!(
            "{other} 暂未接入杠杆设置"
        ))),
    }
}

/// 各交易所当前杠杆的查询入口。`symbol` 为内部交易对（如 `BTCUSDT`）。
///
/// MEXC / HTX 的杠杆随下单请求提交，无法单独查询，返回 `None` 由调用方回退。
fn current_leverage_blocking(
    http: &HttpClient,
    endpoints: &LiveEndpoints,
    venue: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let symbol = symbol.trim().to_ascii_uppercase();
    let base = symbol
        .strip_suffix("USDT")
        .filter(|base| !base.is_empty())
        .ok_or_else(|| ExchangeError::Unsupported(format!("仅支持 USDT 本位永续: {symbol}")))?;
    match venue {
        "binance" => binance_current_leverage(http, &endpoints.binance, credentials, &symbol),
        "aster" => aster_current_leverage(http, &endpoints.aster, credentials, &symbol),
        "okx" => okx_current_leverage(http, &endpoints.okx, credentials, base),
        "bybit" => bybit_current_leverage(http, &endpoints.bybit, credentials, &symbol),
        "bitget" => bitget_current_leverage(http, &endpoints.bitget, credentials, &symbol),
        "gate" => gate_current_leverage(http, &endpoints.gate, credentials, base),
        "bingx" => bingx_current_leverage(http, &endpoints.bingx, credentials, base),
        "hyperliquid" => {
            hyperliquid_current_leverage(http, &endpoints.hyperliquid, credentials, base)
        }
        "lighter" => lighter_current_leverage(http, &endpoints.lighter, credentials, base),
        "mexc" | "htx" => Ok(None),
        other => Err(ExchangeError::Unsupported(format!(
            "{other} 暂未接入杠杆查询"
        ))),
    }
}

/// Binance 的 `positionRisk` 不要求有持仓，直接返回该合约当前杠杆。
fn binance_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let params = format!(
        "symbol={symbol}&recvWindow=5000&timestamp={}",
        now_millis()
    );
    let signature = hmac_hex(secret, &params)?;
    let value = http
        .get_json_with_headers(
            &format!("{base_url}/fapi/v2/positionRisk?{params}&signature={signature}"),
            &[("X-MBX-APIKEY", key)],
        )
        .map_err(transport)?;
    Ok(find_binance_leverage(&value, symbol))
}

/// Aster V3 与 Binance 同构。
fn aster_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let user = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要主账户钱包地址".to_string()))?;
    let signer_key = credentials
        .signer_key
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要 API 钱包私钥".to_string()))?;
    let signer = aster::AsterSigner::new(user, signer_key)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;
    let value = aster::request(
        http,
        "GET",
        base_url,
        "/fapi/v3/positionRisk",
        &signer,
        &[("symbol", symbol.to_string())],
    )
    .map_err(transport)?;
    Ok(find_binance_leverage(&value, symbol))
}

fn find_binance_leverage(value: &Value, symbol: &str) -> Option<Decimal> {
    let rows = value.as_array()?;
    rows.iter()
        .find(|row| row["symbol"].as_str() == Some(symbol))
        .and_then(|row| decimal(&row["leverage"]))
        .filter(|leverage| *leverage > Decimal::ZERO)
}

/// OKX 的杠杆查询接口不要求有持仓。
fn okx_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("OKX 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let path = format!("/api/v5/account/leverage-info?instId={base}-USDT-SWAP&mgnMode=cross");
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_base64(secret, &format!("{timestamp}GET{path}"))?;
    let headers = [
        ("OK-ACCESS-KEY", key),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}{path}"), &headers)
        .map_err(transport)?;
    if value["code"].as_str().unwrap_or("0") != "0" {
        return Ok(None);
    }
    Ok(value["data"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| decimal(&row["lever"]))
        .filter(|leverage| *leverage > Decimal::ZERO))
}

/// Bybit 交易所只在有持仓时返回该合约的杠杆。
fn bybit_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let timestamp = now_millis().to_string();
    let recv_window = "5000";
    let query = format!("category=linear&symbol={symbol}");
    let signature = hmac_hex(secret, &format!("{timestamp}{key}{recv_window}{query}"))?;
    let headers = [
        ("X-BAPI-API-KEY", key),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}/v5/position/list?{query}"), &headers)
        .map_err(transport)?;
    if value["retCode"].as_i64().unwrap_or(0) != 0 {
        return Ok(None);
    }
    Ok(value["result"]["list"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| decimal(&row["leverage"]))
        .filter(|leverage| *leverage > Decimal::ZERO))
}

/// Bitget 的合约账户接口同时返回全仓与逐仓杠杆。
fn bitget_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Bitget 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let path = format!("/api/v2/mix/account/account?symbol={symbol}&productType=USDT-FUTURES&marginCoin=USDT");
    let timestamp = now_millis().to_string();
    let signature = hmac_base64(secret, &format!("{timestamp}GET{path}"))?;
    let headers = [
        ("ACCESS-KEY", key),
        ("ACCESS-SIGN", signature.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}{path}"), &headers)
        .map_err(transport)?;
    if value["code"].as_str().unwrap_or("00000") != "00000" {
        return Ok(None);
    }
    let row = value["data"]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .or_else(|| value["data"].is_object().then(|| value["data"].clone()));
    let Some(row) = row else { return Ok(None); };
    let isolated = row["marginMode"]
        .as_str()
        .map(|mode| mode.contains("isolated"))
        .unwrap_or(false);
    let leverage = if isolated {
        decimal(&row["isolatedLongLever"])
            .filter(|value| *value > Decimal::ZERO)
            .or_else(|| decimal(&row["isolatedShortLever"]))
    } else {
        decimal(&row["crossedMarginLeverage"])
    };
    Ok(leverage.filter(|value| *value > Decimal::ZERO))
}

/// Gate 的逐合约持仓接口，全仓模式下实际杠杆在 `cross_leverage_limit`。
fn gate_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let base_url = base_url.trim_end_matches('/');
    let contract = format!("{base}_USDT");
    let path = format!("/api/v4/futures/usdt/positions/{contract}");
    let timestamp = Utc::now().timestamp().to_string();
    let signature = gate_sign(
        &credentials.api_secret,
        "GET",
        &path,
        "",
        "",
        &timestamp,
    )?;
    let headers = [
        ("KEY", credentials.api_key.as_str()),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    let value = match http.get_json_with_headers(&format!("{base_url}{path}"), &headers) {
        Ok(value) => value,
        // 该合约没有持仓时 Gate 返回 404 POSITION_NOT_FOUND：只代表查不到杠杆，不是故障。
        Err(error) => {
            if let Some(status) = error.downcast_ref::<HttpStatusError>() {
                if status.status == 404 {
                    return Ok(None);
                }
            }
            return Err(transport(error));
        }
    };
    if value["label"].is_string() || value["message"].is_string() {
        return Ok(None);
    }
    Ok(decimal(&value["leverage"])
        .filter(|leverage| *leverage > Decimal::ZERO)
        .or_else(|| {
            decimal(&value["cross_leverage_limit"]).filter(|leverage| *leverage > Decimal::ZERO)
        }))
}

/// BingX 的杠杆查询按多空两侧返回，设置杠杆时两侧一致，取先取到的一侧。
fn bingx_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let symbol = format!("{base}-USDT");
    let timestamp = now_millis().to_string();
    let mut params = [
        ("symbol", symbol.as_str()),
        ("recvWindow", "5000"),
        ("timestamp", timestamp.as_str()),
    ];
    params.sort_by(|left, right| left.0.cmp(right.0));
    let query = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let signature = hmac_hex(secret, &query)?;
    let value = http
        .get_json_with_headers(
            &format!("{base_url}/openApi/swap/v2/trade/leverage?{query}&signature={signature}"),
            &[("X-BX-APIKEY", key)],
        )
        .map_err(transport)?;
    if value["code"].as_i64().unwrap_or(-1) != 0 {
        return Ok(None);
    }
    let data = &value["data"];
    Ok(decimal(&data["longLeverage"])
        .filter(|leverage| *leverage > Decimal::ZERO)
        .or_else(|| {
            decimal(&data["shortLeverage"]).filter(|leverage| *leverage > Decimal::ZERO)
        }))
}

/// Hyperliquid 只在有持仓时返回该合约的杠杆。
fn hyperliquid_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    coin: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let address = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Hyperliquid 需要钱包地址".to_string()))?;
    let value = http
        .post_json(
            &format!("{}/info", base_url.trim_end_matches('/')),
            &json!({ "type": "clearinghouseState", "user": address }),
        )
        .map_err(transport)?;
    Ok(value["assetPositions"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["position"]["coin"].as_str() == Some(coin))
        })
        .and_then(|row| decimal(&row["position"]["leverage"]["value"]))
        .filter(|leverage| *leverage > Decimal::ZERO))
}

/// Lighter 只在有持仓时返回该市场杠杆，`initial_margin_fraction = 10000 / 杠杆`。
fn lighter_current_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    coin: &str,
) -> Result<Option<Decimal>, ExchangeError> {
    let base_url = base_url.trim_end_matches('/');
    let target = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Lighter 需要钱包地址或账户索引".to_string()))?;
    let by = if target.chars().all(|ch| ch.is_ascii_digit()) {
        "index"
    } else {
        "l1_address"
    };
    let value = http
        .get_json(&format!("{base_url}/api/v1/account?by={by}&value={target}"))
        .map_err(transport)?;
    if value["code"].as_i64().unwrap_or(0) != 200 {
        return Ok(None);
    }
    let leverage = value["accounts"]
        .as_array()
        .and_then(|accounts| {
            accounts.iter().find_map(|account| {
                account["positions"].as_array().and_then(|rows| {
                    rows.iter()
                        .find(|row| {
                            row["symbol"]
                                .as_str()
                                .map(|symbol| symbol.eq_ignore_ascii_case(coin))
                                .unwrap_or(false)
                        })
                        .and_then(|row| decimal(&row["initial_margin_fraction"]))
                })
            })
        })
        .filter(|fraction| *fraction > Decimal::ZERO)
        .map(|fraction| Decimal::from(10_000) / fraction);
    Ok(leverage.filter(|value| *value > Decimal::ZERO))
}

/// Binance USDT 本位永续设置杠杆，并强制切到全仓模式。
fn binance_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let params = format!(
        "symbol={symbol}&leverage={}&recvWindow=5000&timestamp={}",
        leverage_integer(symbol, leverage)?,
        now_millis(),
    );
    let signature = hmac_hex(secret, &params)?;
    let url = format!("{base_url}/fapi/v1/leverage?{params}&signature={signature}");
    let value = http
        .post_with_headers(&url, &[("X-MBX-APIKEY", key)])
        .map_err(transport)?;
    reject_binance_style("Binance", &value)?;
    ensure_binance_style_cross(http, base_url, credentials, "Binance", "/fapi/v1/marginType", symbol)
}

/// Binance 系的保证金模式接口：-4046 表示已经是全仓，视为成功。
fn ensure_binance_style_cross(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    venue: &str,
    path: &str,
    symbol: &str,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let params = format!(
        "symbol={symbol}&marginType=CROSSED&recvWindow=5000&timestamp={}",
        now_millis(),
    );
    let signature = hmac_hex(secret, &params)?;
    let url = format!("{base_url}{path}?{params}&signature={signature}");
    let value = http
        .post_with_headers(&url, &[("X-MBX-APIKEY", key)])
        .map_err(transport)?;
    match reject_binance_style(venue, &value) {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("-4046") => Ok(()),
        Err(error) => Err(error),
    }
}

/// Aster V3 与 Binance 同构，只是签名换成 API 钱包。
fn aster_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let user = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要主账户钱包地址".to_string()))?;
    let signer_key = credentials
        .signer_key
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要 API 钱包私钥".to_string()))?;
    let signer = aster::AsterSigner::new(user, signer_key)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;
    let value = aster::request(
        http,
        "POST",
        base_url,
        "/fapi/v3/leverage",
        &signer,
        &[
            ("symbol", symbol.to_string()),
            ("leverage", leverage_integer(symbol, leverage)?),
        ],
    )
    .map_err(transport)?;
    reject_binance_style("Aster", &value)?;
    let value = aster::request(
        http,
        "POST",
        base_url,
        "/fapi/v3/marginType",
        &signer,
        &[
            ("symbol", symbol.to_string()),
            ("marginType", "CROSSED".to_string()),
        ],
    )
    .map_err(transport)?;
    match reject_binance_style("Aster", &value) {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("-4046") => Ok(()),
        Err(error) => Err(error),
    }
}

/// OKX 设置全仓杠杆。
fn okx_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("OKX 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let body = json!({
        "instId": format!("{base}-USDT-SWAP"),
        "lever": num(leverage),
        "mgnMode": "cross",
    })
    .to_string();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_base64(secret, &format!("{timestamp}POST/api/v5/account/set-leverage{body}"))?;
    let headers = [
        ("OK-ACCESS-KEY", key),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http
        .post_body_with_headers(&format!("{base_url}/api/v5/account/set-leverage"), &headers, &body)
        .map_err(transport)?;
    let data = &value["data"][0];
    if data["sCode"].as_str().unwrap_or("0") != "0" {
        return Err(ExchangeError::Rejected(format!(
            "OKX 设置杠杆失败: {}",
            data["sMsg"].as_str().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

/// Bybit 同时设置多空两个方向的杠杆。
fn bybit_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let text = num(leverage);
    let body = json!({
        "category": "linear",
        "symbol": symbol,
        "buyLeverage": text,
        "sellLeverage": text,
    })
    .to_string();
    let timestamp = now_millis().to_string();
    let recv_window = "5000";
    let signature = hmac_hex(secret, &format!("{timestamp}{key}{recv_window}{body}"))?;
    let headers = [
        ("X-BAPI-API-KEY", key),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let value = http
        .post_body_with_headers(&format!("{base_url}/v5/position/set-leverage"), &headers, &body)
        .map_err(transport)?;
    let code = value["retCode"].as_i64().unwrap_or(0);
    // 110043 表示杠杆与当前一致，视为成功。
    if code != 0 && code != 110043 {
        return Err(ExchangeError::Rejected(format!(
            "Bybit 设置杠杆失败: {}",
            value["retMsg"].as_str().unwrap_or("未知错误")
        )));
    }
    // 全仓是 Bybit 的默认模式；显式切换一次，失败（例如组合保证金账户）不影响下单。
    let body = json!({ "setMarginMode": "REGULAR_MARGIN" }).to_string();
    let timestamp = now_millis().to_string();
    let recv_window = "5000";
    let signature = hmac_hex(secret, &format!("{timestamp}{key}{recv_window}{body}"))?;
    let headers = [
        ("X-BAPI-API-KEY", key),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let _ = http.post_body_with_headers(
        &format!("{base_url}/v5/account/set-margin-mode"),
        &headers,
        &body,
    );
    Ok(())
}

/// Bitget 设置合约杠杆，保证金模式沿用下单请求里的全仓设置。
fn bitget_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Bitget 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let body = json!({
        "symbol": symbol,
        "productType": "USDT-FUTURES",
        "marginCoin": "USDT",
        "leverage": num(leverage),
    })
    .to_string();
    let timestamp = now_millis().to_string();
    let signature = hmac_base64(
        secret,
        &format!("{timestamp}POST/api/v2/mix/account/set-leverage{body}"),
    )?;
    let headers = [
        ("ACCESS-KEY", key),
        ("ACCESS-SIGN", signature.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let value = http
        .post_body_with_headers(
            &format!("{base_url}/api/v2/mix/account/set-leverage"),
            &headers,
            &body,
        )
        .map_err(transport)?;
    if value["code"].as_str().unwrap_or("00000") != "00000" {
        return Err(ExchangeError::Rejected(format!(
            "Bitget 设置杠杆失败: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

/// Gate 永续设置逐合约杠杆。
fn gate_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let base_url = base_url.trim_end_matches('/');
    let contract = format!("{base}_USDT");
    let path = format!("/api/v4/futures/usdt/positions/{contract}/leverage");
    // Gate 用 leverage=0 表示全仓，全仓杠杆上限放在 cross_leverage_limit；
    // 默认走全仓，避免单一仓位占用全部保证金后很快触发强平预警。
    let query = format!("leverage=0&cross_leverage_limit={}", num(leverage));
    let timestamp = Utc::now().timestamp().to_string();
    let signature = gate_sign(&credentials.api_secret, "POST", &path, &query, "", &timestamp)?;
    let headers = [
        ("KEY", credentials.api_key.as_str()),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    let value = http
        .post_with_headers(&format!("{base_url}{path}?{query}"), &headers)
        .map_err(transport)?;
    if value["label"].is_string() || value["message"].is_string() {
        return Err(ExchangeError::Rejected(format!(
            "Gate 设置杠杆失败: {}",
            value["message"].as_str().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

/// BingX 双向持仓需要分别设置多空两侧的杠杆。
fn bingx_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    for side in ["LONG", "SHORT"] {
        bingx_leverage_once(http, base_url, credentials, base, side, leverage)?;
    }
    Ok(())
}

fn bingx_leverage_once(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    base: &str,
    side: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let symbol = format!("{base}-USDT");
    let leverage = num(leverage);
    let timestamp = now_millis().to_string();
    let mut params = [
        ("leverage", leverage.as_str()),
        ("recvWindow", "5000"),
        ("side", side),
        ("symbol", symbol.as_str()),
        ("timestamp", timestamp.as_str()),
    ];
    params.sort_by(|left, right| left.0.cmp(right.0));
    let query = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let signature = hmac_hex(secret, &query)?;
    let value = http
        .post_with_headers(
            &format!("{base_url}/openApi/swap/v2/trade/leverage?{query}&signature={signature}"),
            &[("X-BX-APIKEY", key)],
        )
        .map_err(transport)?;
    if value["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(ExchangeError::Rejected(format!(
            "BingX 设置杠杆失败: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

/// Hyperliquid 通过 `updateLeverage` action 设置逐合约杠杆（全仓）。
fn hyperliquid_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    coin: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let private_key = credentials.signer_key.as_deref().ok_or_else(|| {
        ExchangeError::Credentials(
            "Hyperliquid 设置杠杆需要 API Wallet（Agent）私钥".to_string(),
        )
    })?;
    let base_url = base_url.trim_end_matches('/');
    let leverage = leverage_integer(coin, leverage)?
        .parse::<u32>()
        .map_err(|_| ExchangeError::Rejected(format!("{coin} 杠杆超出范围")))?;

    let meta = http
        .post_json(&format!("{base_url}/info"), &json!({ "type": "meta" }))
        .map_err(transport)?;
    let universe = meta["universe"]
        .as_array()
        .ok_or_else(|| ExchangeError::Rejected(format!("Hyperliquid meta 响应异常: {meta}")))?;
    let asset = universe
        .iter()
        .position(|entry| entry["name"].as_str() == Some(coin))
        .ok_or_else(|| ExchangeError::Rejected(format!("Hyperliquid 未上架该合约: {coin}")))?
        as u32;

    let action = hyperliquid_update_leverage_action(asset, true, leverage);
    let nonce = now_millis() as u64;
    let mainnet = !base_url.contains("testnet");
    let signature = sign_hyperliquid_action(&action, nonce, private_key, mainnet)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;
    let body = json!({
        "action": {
            "type": "updateLeverage",
            "asset": asset,
            "isCross": true,
            "leverage": leverage,
        },
        "nonce": nonce,
        "signature": {
            "r": signature.r,
            "s": signature.s,
            "v": signature.v,
        },
        "vaultAddress": Value::Null,
    })
    .to_string();
    let value = http
        .post_body_with_headers(&format!("{base_url}/exchange"), &[], &body)
        .map_err(transport)?;
    if value["status"].as_str() != Some("ok") {
        return Err(ExchangeError::Rejected(format!(
            "Hyperliquid 设置杠杆失败: {value}"
        )));
    }
    Ok(())
}

/// Lighter 的 nonce 按 API Key 严格递增（`previous_nonce + 1`），不是时间戳；
/// 直接从 `nextNonce` 接口取，避免同一毫秒内重复导致 21104 invalid nonce。
fn lighter_next_nonce(
    http: &HttpClient,
    base_url: &str,
    account_index: i64,
    api_key_index: u8,
) -> Result<i64, ExchangeError> {
    let value = http
        .get_json(&format!(
            "{base_url}/api/v1/nextNonce?account_index={account_index}&api_key_index={api_key_index}"
        ))
        .map_err(transport)?;
    value["nonce"]
        .as_i64()
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter nextNonce 响应异常: {value}")))
}

/// Lighter 通过 `L2UpdateLeverage` 交易设置逐市场杠杆（全仓）。
fn lighter_leverage(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    coin: &str,
    leverage: Decimal,
) -> Result<(), ExchangeError> {
    let private_key = credentials.signer_key.as_deref().ok_or_else(|| {
        ExchangeError::Credentials("Lighter 设置杠杆需要 API Key 私钥".to_string())
    })?;
    let account_index = credentials
        .account_index
        .ok_or_else(|| ExchangeError::Credentials("Lighter 账户缺少数字账户索引".to_string()))?;
    let base_url = base_url.trim_end_matches('/');

    let details = http
        .get_json(&format!("{base_url}/api/v1/orderBookDetails"))
        .map_err(transport)?;
    let rows = details["order_book_details"]
        .as_array()
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter 合约列表响应异常: {details}")))?;
    let market_index = rows
        .iter()
        .find(|row| {
            row["symbol"].as_str() == Some(coin)
                && row["market_type"].as_str().unwrap_or("perp") == "perp"
        })
        .and_then(|row| row["market_id"].as_i64())
        .and_then(|value| i16::try_from(value).ok())
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter 未上架该合约: {coin}")))?;

    let initial_margin_fraction = (Decimal::from(10_000u32) / leverage)
        .round_dp(0)
        .to_u16()
        .filter(|value| *value > 0)
        .ok_or_else(|| ExchangeError::Rejected(format!("{coin} 杠杆超出 Lighter 支持范围")))?;

    let nonce = lighter_next_nonce(
        http,
        base_url,
        account_index,
        credentials.key_index.unwrap_or(0),
    )?;
    let expired_at = now_millis() + 24 * 60 * 60 * 1000;
    let chain_id = if base_url.contains("testnet") {
        300
    } else {
        LIGHTER_MAINNET_CHAIN_ID
    };
    let signed = sign_lighter_leverage(
        &LighterLeverage {
            account_index,
            api_key_index: credentials.key_index.unwrap_or(0),
            market_index,
            initial_margin_fraction,
            nonce,
            expired_at,
        },
        private_key,
        chain_id,
    )
    .map_err(|error| ExchangeError::Credentials(error.to_string()))?;

    let form = format!("tx_type={}&tx_info={}", signed.tx_type, pct(&signed.tx_info));
    let value = http
        .post_body_with_headers(
            &format!("{base_url}/api/v1/sendTx"),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            &form,
        )
        .map_err(transport)?;
    if value["code"].as_i64().unwrap_or(0) != 200 {
        return Err(ExchangeError::Rejected(format!(
            "Lighter 设置杠杆失败: {}",
            value["message"].as_str().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

/// 交易所杠杆多数只接受整数，非整数会被交易所拒绝。
fn leverage_integer(symbol: &str, leverage: Decimal) -> Result<String, ExchangeError> {
    if leverage.fract() != Decimal::ZERO {
        return Err(ExchangeError::Rejected(format!(
            "{symbol} 的杠杆必须是整数: {leverage}"
        )));
    }
    Ok(num(leverage))
}

/// Binance 风格响应：成功时不带 `code`，或 `code` 为 200。
fn reject_binance_style(venue: &str, value: &Value) -> Result<(), ExchangeError> {
    if let Some(code) = value["code"].as_i64() {
        if code != 200 {
            return Err(ExchangeError::Rejected(format!(
                "{venue} 设置杠杆失败: {}",
                value["msg"].as_str().unwrap_or("未知错误")
            )));
        }
    }
    Ok(())
}


/// Binance USDT 本位永续下单，签名参数直接拼在 URL 上。
fn binance_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let params = format!(
        "symbol={}&side={}&type=LIMIT&timeInForce=GTC&quantity={}&price={}&reduceOnly={}&newClientOrderId={}&recvWindow=5000&timestamp={}",
        internal_symbol(request),
        if request.side == Side::Buy { "BUY" } else { "SELL" },
        num(request.quantity),
        num(request.limit_price),
        request.reduce_only,
        client_order_id,
        now_millis(),
    );
    let signature = hmac_hex(secret, &params)?;
    let url = format!("{base_url}/fapi/v1/order?{params}&signature={signature}");
    let value = http
        .post_with_headers(&url, &[("X-MBX-APIKEY", key)])
        .map_err(transport)?;

    if let Some(code) = value["code"].as_i64() {
        if code != 200 {
            return Err(ExchangeError::Rejected(format!(
                "Binance 拒单: {}",
                value["msg"].as_str().unwrap_or("未知错误")
            )));
        }
    }
    let order_id = value["orderId"]
        .as_i64()
        .ok_or_else(|| ExchangeError::Rejected(format!("Binance 响应缺少 orderId: {value}")))?;
    Ok(ack(order_id.to_string(), client_order_id))
}

/// Aster V3 下单：参数表单带上 user / signer / nonce / signature。
fn aster_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let user = credentials
        .wallet_address
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要主账户钱包地址".to_string()))?;
    let signer_key = credentials
        .signer_key
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Aster 需要 API 钱包私钥".to_string()))?;
    let signer = aster::AsterSigner::new(user, signer_key)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;

    let mut params = vec![
        ("symbol", internal_symbol(request)),
        (
            "side",
            if request.side == Side::Buy {
                "BUY".to_string()
            } else {
                "SELL".to_string()
            },
        ),
        ("type", "LIMIT".to_string()),
        ("timeInForce", "GTC".to_string()),
        ("quantity", num(request.quantity)),
        ("price", num(request.limit_price)),
        ("newClientOrderId", client_order_id.to_string()),
    ];
    if request.reduce_only {
        params.push(("reduceOnly", "true".to_string()));
    }

    let value = aster::request(http, "POST", base_url, "/fapi/v3/order", &signer, &params)
        .map_err(transport)?;
    if let Some(code) = value["code"].as_i64() {
        if code != 200 {
            return Err(ExchangeError::Rejected(format!(
                "Aster 拒单: {}",
                value["msg"].as_str().unwrap_or("未知错误")
            )));
        }
    }
    let order_id = value["orderId"]
        .as_i64()
        .ok_or_else(|| ExchangeError::Rejected(format!("Aster 响应缺少 orderId: {value}")))?;
    Ok(ack(order_id.to_string(), client_order_id))
}

/// 读取 OKX 账户的持仓模式：`net_mode`（单向）或 `long_short_mode`（双向）。
fn okx_position_mode(
    http: &HttpClient,
    base_url: &str,
    key: &str,
    secret: &str,
    passphrase: &str,
) -> Result<String, ExchangeError> {
    let path = "/api/v5/account/config";
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_base64(secret, &format!("{timestamp}GET{path}"))?;
    let headers = [
        ("OK-ACCESS-KEY", key),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}{path}"), &headers)
        .map_err(transport)?;
    Ok(value["data"][0]["posMode"]
        .as_str()
        .unwrap_or("net_mode")
        .to_string())
}

/// OKX 永续按张数下单，先用公开合约信息把币数量换算成张数。
fn okx_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("OKX 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let inst_id = format!("{}-USDT-SWAP", base_asset(request)?);

    let instrument = http
        .get_json(&format!(
            "{base_url}/api/v5/public/instruments?instType=SWAP&instId={inst_id}"
        ))
        .map_err(transport)?;
    let contract_value = decimal(&instrument["data"][0]["ctVal"])
        .ok_or_else(|| ExchangeError::Rejected(format!("OKX 未返回 {inst_id} 的合约面值")))?;
    if contract_value <= Decimal::ZERO {
        return Err(ExchangeError::Rejected(format!(
            "OKX {inst_id} 合约面值异常: {contract_value}"
        )));
    }
    // OKX 的数量单位是「张」：minSz / lotSz 都以张计，BTC-USDT-SWAP 为 0.01 张
    // （即 0.0001 BTC），并非整数张。按步长向下取整后再校验最小张数。
    let min_contracts = decimal(&instrument["data"][0]["minSz"])
        .filter(|value| *value > Decimal::ZERO)
        .unwrap_or(Decimal::ONE);
    let lot_size = decimal(&instrument["data"][0]["lotSz"])
        .filter(|value| *value > Decimal::ZERO)
        .unwrap_or(Decimal::ONE);
    // 开仓向下取整避免超量；平仓按最接近的整手数取整：持仓数量由名义价值折算而来，
    // 直接向下取整会不足最小张数（如 0.009986 张）。
    let lots = (request.quantity / contract_value) / lot_size;
    let contracts = if request.reduce_only {
        lots.round() * lot_size
    } else {
        lots.floor() * lot_size
    };
    if contracts < min_contracts {
        return Err(ExchangeError::Rejected(format!(
            "OKX {inst_id} 数量不足 {min_contracts} 张（每张 {contract_value}，步长 {lot_size} 张）"
        )));
    }

    // 双向持仓（long_short_mode）必须显式指定 posSide，且 reduceOnly 仅适用于
    // 单向持仓（net_mode）；单向持仓则不能带 posSide。
    let mut body = json!({
        "instId": inst_id,
        "tdMode": "cross",
        "side": if request.side == Side::Buy { "buy" } else { "sell" },
        "ordType": "limit",
        "px": num(request.limit_price),
        "sz": num(contracts),
        "clOrdId": client_order_id,
    });
    if okx_position_mode(http, base_url, key, secret, passphrase)? == "long_short_mode" {
        let pos_side = if request.reduce_only {
            if request.side == Side::Buy { "short" } else { "long" }
        } else if request.side == Side::Buy {
            "long"
        } else {
            "short"
        };
        body["posSide"] = json!(pos_side);
    } else {
        body["reduceOnly"] = json!(request.reduce_only);
    }
    let body = body.to_string();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let signature = hmac_base64(secret, &format!("{timestamp}POST/api/v5/trade/order{body}"))?;
    let headers = [
        ("OK-ACCESS-KEY", key),
        ("OK-ACCESS-SIGN", signature.as_str()),
        ("OK-ACCESS-TIMESTAMP", timestamp.as_str()),
        ("OK-ACCESS-PASSPHRASE", passphrase),
    ];
    let value = http
        .post_body_with_headers(&format!("{base_url}/api/v5/trade/order"), &headers, &body)
        .map_err(transport)?;

    let data = &value["data"][0];
    if data["sCode"].as_str().unwrap_or("0") != "0" {
        return Err(ExchangeError::Rejected(format!(
            "OKX 拒单: {}",
            data["sMsg"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = data["ordId"]
        .as_str()
        .ok_or_else(|| ExchangeError::Rejected(format!("OKX 响应缺少 ordId: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// Bybit v5 线性合约下单，签名覆盖时间戳、密钥与请求体。
fn bybit_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let body = json!({
        "category": "linear",
        "symbol": internal_symbol(request),
        "side": if request.side == Side::Buy { "Buy" } else { "Sell" },
        "orderType": "Limit",
        "qty": num(request.quantity),
        "price": num(request.limit_price),
        "reduceOnly": request.reduce_only,
        "orderLinkId": client_order_id,
    })
    .to_string();
    let timestamp = now_millis().to_string();
    let recv_window = "5000";
    let signature = hmac_hex(secret, &format!("{timestamp}{key}{recv_window}{body}"))?;
    let headers = [
        ("X-BAPI-API-KEY", key),
        ("X-BAPI-TIMESTAMP", timestamp.as_str()),
        ("X-BAPI-SIGN", signature.as_str()),
        ("X-BAPI-RECV-WINDOW", recv_window),
    ];
    let value = http
        .post_body_with_headers(&format!("{base_url}/v5/order/create"), &headers, &body)
        .map_err(transport)?;

    if value["retCode"].as_i64().unwrap_or(0) != 0 {
        return Err(ExchangeError::Rejected(format!(
            "Bybit 拒单: {}",
            value["retMsg"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = value["result"]["orderId"]
        .as_str()
        .ok_or_else(|| ExchangeError::Rejected(format!("Bybit 响应缺少 orderId: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// 读取 Bitget 合约账户的持仓模式：`one_way_mode`（单向）或 `hedge_mode`（双向）。
fn bitget_position_mode(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    symbol: &str,
) -> Result<String, ExchangeError> {
    let path = format!(
        "/api/v2/mix/account/account?symbol={symbol}&productType=USDT-FUTURES&marginCoin=USDT"
    );
    let timestamp = now_millis().to_string();
    let signature = hmac_base64(
        &credentials.api_secret,
        &format!("{timestamp}GET{path}"),
    )?;
    let headers = [
        ("ACCESS-KEY", credentials.api_key.as_str()),
        ("ACCESS-SIGN", signature.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", credentials.passphrase.as_deref().unwrap_or("")),
        ("locale", "zh-CN"),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}{path}"), &headers)
        .map_err(transport)?;
    Ok(value["data"]["posMode"]
        .as_str()
        .unwrap_or("one_way_mode")
        .to_string())
}

/// Bitget 合约下单，数量单位与下单币种一致。
fn bitget_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let passphrase = credentials
        .passphrase
        .as_deref()
        .ok_or_else(|| ExchangeError::Credentials("Bitget 需要 Passphrase".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let symbol = internal_symbol(request);
    // 双向持仓必须用 tradeSide + holdSide 表达开平与方向，单向持仓则只能用
    // reduceOnly；混用会被拒为 40774。
    let mut body = json!({
        "symbol": symbol.clone(),
        "productType": "USDT-FUTURES",
        "marginMode": "crossed",
        "marginCoin": "USDT",
        "size": num(request.quantity),
        "price": num(request.limit_price),
        "side": if request.side == Side::Buy { "buy" } else { "sell" },
        "orderType": "limit",
        "clientOid": client_order_id,
    });
    if bitget_position_mode(http, base_url, credentials, &symbol)? == "hedge_mode" {
        // 双向持仓下 `side` 表示持仓方向（buy=多、sell=空），开平由 tradeSide 表达；
        // 平多要发 side=buy + tradeSide=close，否则会报 22002「暂无仓位可平」。
        let hold_side = if request.reduce_only {
            if request.side == Side::Buy { "short" } else { "long" }
        } else if request.side == Side::Buy {
            "long"
        } else {
            "short"
        };
        body["side"] = json!(if hold_side == "long" { "buy" } else { "sell" });
        body["tradeSide"] = json!(if request.reduce_only { "close" } else { "open" });
        body["holdSide"] = json!(hold_side);
    } else {
        body["reduceOnly"] = json!(if request.reduce_only { "YES" } else { "NO" });
    }
    let body = body.to_string();
    let timestamp = now_millis().to_string();
    let signature = hmac_base64(
        secret,
        &format!("{timestamp}POST/api/v2/mix/order/place-order{body}"),
    )?;
    let headers = [
        ("ACCESS-KEY", key),
        ("ACCESS-SIGN", signature.as_str()),
        ("ACCESS-TIMESTAMP", timestamp.as_str()),
        ("ACCESS-PASSPHRASE", passphrase),
        ("locale", "zh-CN"),
    ];
    let value = http
        .post_body_with_headers(
            &format!("{base_url}/api/v2/mix/order/place-order"),
            &headers,
            &body,
        )
        .map_err(transport)?;

    if value["code"].as_str().unwrap_or("00000") != "00000" {
        return Err(ExchangeError::Rejected(format!(
            "Bitget 拒单: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = value["data"]["orderId"]
        .as_str()
        .ok_or_else(|| ExchangeError::Rejected(format!("Bitget 响应缺少 orderId: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// Gate 永续按合约张数下单，多头为正、空头为负。
fn gate_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let contract = format!("{}_USDT", base_asset(request)?);
    let multiplier = gate_multiplier(http, base_url, credentials, &contract)?;
    let contracts = (request.quantity / multiplier)
        .floor()
        .to_i64()
        .ok_or_else(|| ExchangeError::Rejected(format!("Gate {contract} 下单数量异常")))?;
    if contracts <= 0 {
        return Err(ExchangeError::Rejected(format!(
            "Gate {contract} 数量不足 1 张（每张 {multiplier}）"
        )));
    }
    let size = if request.side == Side::Buy {
        contracts
    } else {
        -contracts
    };
    let path = "/api/v4/futures/usdt/orders";
    // Gate 要求 `text` 以 `t-` 开头，且自定义部分不超过 28 字节；client_id 可能长达 32 位。
    let text: String = format!("t-{client_order_id}").chars().take(30).collect();
    let body = json!({
        "contract": contract,
        "size": size,
        "price": num(request.limit_price),
        "tif": "gtc",
        "reduce_only": request.reduce_only,
        "text": text,
    })
    .to_string();
    let timestamp = Utc::now().timestamp().to_string();
    let signature = gate_sign(secret, "POST", path, "", &body, &timestamp)?;
    let headers = [
        ("KEY", key),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    let value = http
        .post_body_with_headers(&format!("{base_url}{path}"), &headers, &body)
        .map_err(transport)?;

    let order_id = value["id"]
        .as_i64()
        .map(|id| id.to_string())
        .or_else(|| value["id"].as_str().map(str::to_string))
        .ok_or_else(|| ExchangeError::Rejected(format!("Gate 响应缺少订单号: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// 读取 Gate 合约的每张面值（`quanto_multiplier`）。
fn gate_multiplier(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    contract: &str,
) -> Result<Decimal, ExchangeError> {
    let path = format!("/api/v4/futures/usdt/contracts/{contract}");
    let timestamp = Utc::now().timestamp().to_string();
    let signature = gate_sign(&credentials.api_secret, "GET", &path, "", "", &timestamp)?;
    let headers = [
        ("KEY", credentials.api_key.as_str()),
        ("SIGN", signature.as_str()),
        ("TIMESTAMP", timestamp.as_str()),
    ];
    let value = http
        .get_json_with_headers(&format!("{base_url}{path}"), &headers)
        .map_err(transport)?;
    decimal(&value["quanto_multiplier"])
        .filter(|value| *value > Decimal::ZERO)
        .ok_or_else(|| ExchangeError::Rejected(format!("Gate 未返回 {contract} 的合约面值")))
}

/// HTX U 本位永续下单，数量单位是合约张数。
fn htx_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let contract = format!("{}-USDT", base_asset(request)?);
    let info = htx_request(
        http,
        base_url,
        "GET",
        "/linear-swap-api/v1/swap_contract_info",
        &[("contract_code", contract.as_str())],
        credentials,
    )?;
    let contract_size = decimal(&info["data"][0]["contract_size"])
        .filter(|value| *value > Decimal::ZERO)
        .ok_or_else(|| ExchangeError::Rejected(format!("HTX 未返回 {contract} 的合约面值")))?;
    let volume = (request.quantity / contract_size)
        .floor()
        .to_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            ExchangeError::Rejected(format!(
                "HTX {contract} 数量不足 1 张（每张 {contract_size}）"
            ))
        })?;

    let volume = volume.to_string();
    let price = num(request.limit_price);
    let reduce_only = if request.reduce_only { "1" } else { "0" };
    // HTX 的杠杆随下单请求提交；未指定时沿用原有的 20 倍默认值。
    let lever_rate = match request.leverage {
        Some(value) => value
            .round_dp(0)
            .to_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| ExchangeError::Rejected(format!("HTX {contract} 杠杆不合法")))?
            .to_string(),
        None => "20".to_string(),
    };
    let value = htx_request(
        http,
        base_url,
        "POST",
        "/linear-swap-api/v1/swap_cross_order",
        &[
            ("contract_code", contract.as_str()),
            ("volume", volume.as_str()),
            ("price", price.as_str()),
            (
                "direction",
                if request.side == Side::Buy {
                    "buy"
                } else {
                    "sell"
                },
            ),
            ("offset", if request.reduce_only { "close" } else { "open" }),
            ("order_price_type", "limit"),
            ("lever_rate", lever_rate.as_str()),
            ("reduce_only", reduce_only),
            ("client_order_id", client_order_id),
        ],
        credentials,
    )?;

    if value["status"].as_str() != Some("ok") {
        return Err(ExchangeError::Rejected(format!(
            "HTX 拒单: {}",
            value["err-msg"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = value["data"]["order_id_str"]
        .as_str()
        .map(str::to_string)
        .or_else(|| value["data"]["order_id"].as_i64().map(|id| id.to_string()))
        .ok_or_else(|| ExchangeError::Rejected(format!("HTX 响应缺少订单号: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// HTX 私有接口：参数排序后参与签名，POST 时同时放进 JSON 请求体。
fn htx_request(
    http: &HttpClient,
    base_url: &str,
    method: &str,
    path: &str,
    params: &[(&str, &str)],
    credentials: &AccountCredentials,
) -> Result<Value, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let host = base_url.split("://").nth(1).unwrap_or(base_url);
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    let mut all: Vec<(&str, &str)> = vec![
        ("AccessKeyId", key),
        ("SignatureMethod", "HmacSHA256"),
        ("SignatureVersion", "2"),
        ("Timestamp", timestamp.as_str()),
    ];
    all.extend_from_slice(params);
    all.sort_by(|left, right| left.0.cmp(right.0));

    let query = all
        .iter()
        .map(|(name, value)| format!("{}={}", pct(name), pct(value)))
        .collect::<Vec<_>>()
        .join("&");
    let signature = hmac_base64(secret, &format!("{method}\n{host}\n{path}\n{query}"))?;
    let url = format!("{base_url}{path}?{query}&Signature={}", pct(&signature));

    let value = if method == "POST" {
        let mut payload = serde_json::Map::new();
        for (name, value) in &all {
            payload.insert((*name).to_string(), json!(value));
        }
        http.post_body_with_headers(&url, &[], &Value::Object(payload).to_string())
            .map_err(transport)?
    } else {
        http.get_json(&url).map_err(transport)?
    };
    Ok(value)
}

/// MEXC 合约下单，`vol` 为合约张数，方向用 1/2/3/4 表示开平仓。
fn mexc_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let base_url = base_url.trim_end_matches('/');
    let symbol = format!("{}_USDT", base_asset(request)?);
    let detail = http
        .get_json(&format!(
            "{base_url}/api/v1/contract/detail?symbol={symbol}"
        ))
        .map_err(transport)?;
    let contract_size = decimal(&detail["data"]["contractSize"])
        .filter(|value| *value > Decimal::ZERO)
        .ok_or_else(|| ExchangeError::Rejected(format!("MEXC 未返回 {symbol} 的合约面值")))?;
    let volume = (request.quantity / contract_size)
        .floor()
        .to_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            ExchangeError::Rejected(format!(
                "MEXC {symbol} 数量不足 1 张（每张 {contract_size}）"
            ))
        })?;

    // 1 开多、2 平空、3 开空、4 平多。
    let side = match (request.side, request.reduce_only) {
        (Side::Buy, false) => 1,
        (Side::Sell, true) => 2,
        (Side::Sell, false) => 3,
        (Side::Buy, true) => 4,
    };
    // MEXC 的杠杆随下单请求提交；未指定时沿用原有的 20 倍默认值。
    let leverage = match request.leverage {
        Some(value) => value
            .round_dp(0)
            .to_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| ExchangeError::Rejected(format!("MEXC {symbol} 杠杆不合法")))?,
        None => 20,
    };
    let body = json!({
        "symbol": symbol,
        "price": num(request.limit_price),
        "vol": volume,
        "leverage": leverage,
        "side": side,
        "type": 1,
        "openType": 2,
        "externalOid": client_order_id,
    })
    .to_string();
    let timestamp = now_millis().to_string();
    let signature = hmac_hex(
        credentials.api_secret.as_str(),
        &format!("{}{}{}", credentials.api_key, timestamp, body),
    )?;
    let headers = [
        ("ApiKey", credentials.api_key.as_str()),
        ("Request-Time", timestamp.as_str()),
        ("Signature", signature.as_str()),
        ("Recv-Window", "10"),
    ];
    let value = http
        .post_body_with_headers(
            &format!("{base_url}/api/v1/private/order/submit"),
            &headers,
            &body,
        )
        .map_err(transport)?;

    if value["success"].as_bool() != Some(true) {
        return Err(ExchangeError::Rejected(format!(
            "MEXC 拒单: {}",
            value["message"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = value["data"]
        .as_str()
        .map(str::to_string)
        .or_else(|| value["data"].as_i64().map(|id| id.to_string()))
        .ok_or_else(|| ExchangeError::Rejected(format!("MEXC 响应缺少订单号: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// BingX 永续下单：签名串是按键排序后的 query，订单参数全部放在 URL 上。
fn bingx_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let (key, secret) = key_pair(credentials)?;
    let base_url = base_url.trim_end_matches('/');
    let symbol = format!("{}-USDT", base_asset(request)?);
    let quantity = num(request.quantity);
    let price = num(request.limit_price);
    let timestamp = now_millis().to_string();
    // 平仓时 positionSide 指向被平掉的那一腿。
    let position_side = match (request.side, request.reduce_only) {
        (Side::Buy, false) | (Side::Sell, true) => "LONG",
        _ => "SHORT",
    };
    let reduce_only = if request.reduce_only { "true" } else { "false" };

    let mut params = [
        ("positionSide", position_side),
        ("price", price.as_str()),
        ("quantity", quantity.as_str()),
        ("recvWindow", "5000"),
        ("reduceOnly", reduce_only),
        (
            "side",
            if request.side == Side::Buy {
                "BUY"
            } else {
                "SELL"
            },
        ),
        ("symbol", symbol.as_str()),
        ("timeInForce", "GTC"),
        ("timestamp", timestamp.as_str()),
        ("type", "LIMIT"),
    ];
    params.sort_by(|left, right| left.0.cmp(right.0));
    let query = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let signature = hmac_hex(secret, &query)?;
    let value = http
        .post_with_headers(
            &format!("{base_url}/openApi/swap/v2/trade/order?{query}&signature={signature}"),
            &[("X-BX-APIKEY", key)],
        )
        .map_err(transport)?;

    if value["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(ExchangeError::Rejected(format!(
            "BingX 拒单: {}",
            value["msg"].as_str().unwrap_or("未知错误")
        )));
    }
    let order_id = value["data"]["order"]["orderId"]
        .as_i64()
        .map(|id| id.to_string())
        .or_else(|| {
            value["data"]["order"]["orderId"]
                .as_str()
                .map(str::to_string)
        })
        .ok_or_else(|| ExchangeError::Rejected(format!("BingX 响应缺少订单号: {value}")))?;
    Ok(ack(order_id, client_order_id))
}

/// Hyperliquid 永续限价单：签名完全在本地完成，提交的只有已签名 action。
fn hyperliquid_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let private_key = credentials.signer_key.as_deref().ok_or_else(|| {
        ExchangeError::Credentials(
            "Hyperliquid 下单需要 API Wallet（Agent）私钥，仅填写钱包地址只能查询余额".to_string(),
        )
    })?;
    let base_url = base_url.trim_end_matches('/');
    let coin = base_asset(request)?;

    // 开仓前先确认可用保证金：统一账户看现货余额，标准账户看永续余额。
    if !request.reduce_only {
        hyperliquid_ensure_perp_margin(http, base_url, credentials, request)?;
    }

    // `asset` 索引与 `szDecimals` 都在 meta.universe 的下标里，下单前必须查一次。
    let meta = http
        .post_json(&format!("{base_url}/info"), &json!({ "type": "meta" }))
        .map_err(transport)?;
    let universe = meta["universe"]
        .as_array()
        .ok_or_else(|| ExchangeError::Rejected(format!("Hyperliquid meta 响应异常: {meta}")))?;
    let entry = universe
        .iter()
        .enumerate()
        .find(|(_, entry)| entry["name"].as_str() == Some(coin.as_str()))
        .ok_or_else(|| ExchangeError::Rejected(format!("Hyperliquid 未上架该合约: {coin}")))?;
    let asset = entry.0 as u32;
    let size_decimals = entry.1["szDecimals"].as_u64().unwrap_or(0) as u32;

    let order = HyperliquidOrder {
        coin: &coin,
        asset,
        size_decimals,
        is_buy: request.side == Side::Buy,
        price: request.limit_price,
        size: request.quantity,
        reduce_only: request.reduce_only,
        cloid: Some(hyperliquid_cloid(client_order_id)),
    };
    let action = order.action().map_err(ExchangeError::Rejected)?;
    let action_value = order.action_value().map_err(ExchangeError::Rejected)?;
    let nonce = now_millis() as u64;
    let mainnet = !base_url.contains("testnet");
    let signature = sign_hyperliquid_action(&action, nonce, private_key, mainnet)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;

    let body = json!({
        "action": action_value,
        "nonce": nonce,
        "signature": {
            "r": signature.r,
            "s": signature.s,
            "v": signature.v,
        },
        "vaultAddress": Value::Null,
    })
    .to_string();
    let value = http
        .post_body_with_headers(&format!("{base_url}/exchange"), &[], &body)
        .map_err(transport)?;

    if value["status"].as_str() != Some("ok") {
        let detail = value["response"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string());
        return Err(ExchangeError::Rejected(format!(
            "Hyperliquid 拒单: {detail}"
        )));
    }
    let status = &value["response"]["data"]["statuses"][0];
    if let Some(error) = status["error"].as_str() {
        return Err(ExchangeError::Rejected(format!(
            "Hyperliquid 拒单: {error}"
        )));
    }
    let order_id = status["resting"]["oid"]
        .as_i64()
        .or_else(|| status["filled"]["oid"].as_i64())
        .ok_or_else(|| ExchangeError::Rejected(format!("Hyperliquid 响应缺少 oid: {value}")))?;
    Ok(ack(order_id.to_string(), client_order_id))
}

/// Hyperliquid 现货与永续的账本关系由「账户抽象模式」决定。
///
/// - `unifiedAccount` / `portfolioMargin`：每个币种只有一个余额，现货 USDC 直接
///   作为永续保证金；官方文档说明此时 API 返回的永续 `clearinghouseState`
///   对用户没有参考意义，余额只看现货清算状态。
/// - `standard` / `dexAbstraction`：现货与永续独立，现货 USDC 不能直接当保证金。
fn hyperliquid_ensure_perp_margin(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
) -> Result<(), ExchangeError> {
    let address = credentials.wallet_address.as_deref().ok_or_else(|| {
        ExchangeError::Credentials("Hyperliquid 划转需要主账户钱包地址".to_string())
    })?;
    // Hyperliquid 要求签名前把地址统一转成小写。
    let address = address.trim().to_ascii_lowercase();

    let notional = request.quantity * request.limit_price;
    let leverage = request
        .leverage
        .filter(|value| *value > Decimal::ZERO)
        .unwrap_or(Decimal::ONE);
    // 开仓所需保证金，向上取整到分。
    let required =
        (notional / leverage).round_dp_with_strategy(2, RoundingStrategy::ToPositiveInfinity);

    // 账户抽象模式决定现货与永续是否共用保证金，读不到时跳过预检交给交易所判定，
    // 避免把统一账户误判成标准账户而拦掉正常下单。
    let Some(mode) = hyperliquid_abstraction_mode(http, base_url, &address) else {
        return Ok(());
    };

    if matches!(mode.as_str(), "unifiedAccount" | "portfolioMargin") {
        // 统一账户的永续状态对 API 用户没有意义，余额只看现货清算状态。
        let spot_available = hyperliquid_spot_usdc(http, base_url, &address)?;
        if spot_available >= required {
            return Ok(());
        }
        return Err(ExchangeError::Rejected(format!(
            "Hyperliquid 可用保证金 {spot_available} USDC，本次下单需要 {required} USDC\
             （账户模式 {mode}）。统一账户的现货余额就是永续保证金，请确认这笔 USDC 是否被挂单占用"
        )));
    }

    let perp = http
        .post_json(
            &format!("{base_url}/info"),
            &json!({ "type": "clearinghouseState", "user": address }),
        )
        .map_err(transport)?;
    let account_value = decimal(&perp["crossMarginSummary"]["accountValue"]).unwrap_or_default();
    let margin_used = decimal(&perp["crossMarginSummary"]["totalMarginUsed"]).unwrap_or_default();
    let perp_available = (account_value - margin_used).max(Decimal::ZERO);
    if perp_available >= required {
        return Ok(());
    }

    // 标准账户的现货与永续是独立账本且只有主钱包能划转；API Wallet（Agent）签名的
    // 划转会被服务端当成 Agent 自己的账户而报 "Must deposit before performing actions"。
    let spot_available = hyperliquid_spot_usdc(http, base_url, &address)?;
    Err(ExchangeError::Rejected(format!(
        "Hyperliquid 永续可用保证金 {perp_available} USDC，本次下单需要 {required} USDC\
         （现货可用 {spot_available} USDC，账户模式 {mode}）。\
         API Wallet 无法在现货与永续之间划转，请先在 Hyperliquid 网页端把 USDC 划转到永续账户"
    )))
}

/// 读取账户抽象模式；代理偶发 RST，重试几次，仍失败则返回 `None` 交给调用方跳过预检。
fn hyperliquid_abstraction_mode(
    http: &HttpClient,
    base_url: &str,
    address: &str,
) -> Option<String> {
    let body = json!({ "type": "userAbstraction", "user": address });
    for _ in 0..3 {
        if let Ok(value) = http.post_json(&format!("{base_url}/info"), &body) {
            return Some(value.as_str().unwrap_or("unknown").to_string());
        }
    }
    None
}

/// 现货清算状态里的 USDC 可用余额（总额减去挂单占用）。
fn hyperliquid_spot_usdc(
    http: &HttpClient,
    base_url: &str,
    address: &str,
) -> Result<Decimal, ExchangeError> {
    let spot = http
        .post_json(
            &format!("{base_url}/info"),
            &json!({ "type": "spotClearinghouseState", "user": address }),
        )
        .map_err(transport)?;
    Ok(spot["balances"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["coin"].as_str() == Some("USDC")))
        .map(|row| {
            let total = decimal(&row["total"]).unwrap_or_default();
            let hold = decimal(&row["hold"]).unwrap_or_default();
            (total - hold).max(Decimal::ZERO)
        })
        .unwrap_or_default())
}

/// Lighter 永续限价单：市场精度取自 `orderBookDetails`，签名后按表单提交。
fn lighter_place(
    http: &HttpClient,
    base_url: &str,
    credentials: &AccountCredentials,
    request: &PlaceOrderRequest,
    client_order_id: &str,
) -> Result<VenueOrderAck, ExchangeError> {
    let private_key = credentials.signer_key.as_deref().ok_or_else(|| {
        ExchangeError::Credentials(
            "Lighter 下单需要 API Key 私钥，仅填写钱包地址只能查询余额".to_string(),
        )
    })?;
    let account_index = credentials
        .account_index
        .ok_or_else(|| ExchangeError::Credentials("Lighter 账户缺少数字账户索引".to_string()))?;
    let base_url = base_url.trim_end_matches('/');
    let coin = base_asset(request)?;

    let details = http
        .get_json(&format!("{base_url}/api/v1/orderBookDetails"))
        .map_err(transport)?;
    let rows = details["order_book_details"]
        .as_array()
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter 合约列表响应异常: {details}")))?;
    let row = rows
        .iter()
        .find(|row| {
            row["symbol"].as_str() == Some(coin.as_str())
                && row["market_type"].as_str().unwrap_or("perp") == "perp"
        })
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter 未上架该合约: {coin}")))?;
    let market_index = row["market_id"]
        .as_i64()
        .and_then(|value| i16::try_from(value).ok())
        .ok_or_else(|| ExchangeError::Rejected("Lighter 合约索引超出范围".to_string()))?;
    let size_decimals = row["size_decimals"].as_u64().unwrap_or(0) as u32;
    let price_decimals = row["price_decimals"].as_u64().unwrap_or(0) as u32;

    // Lighter 对 GTC 挂单同时限制币本位数量与 USDC 名义额，低于任一阈值
    // 会被序列器以 21706（invalid order base or quote amount）拒单。
    let min_base = decimal_field(&row["min_base_amount"]).unwrap_or_default();
    let min_quote = decimal_field(&row["min_quote_amount"]).unwrap_or_default();
    let notional = request.quantity * request.limit_price;
    if (min_base > Decimal::ZERO && request.quantity < min_base)
        || (min_quote > Decimal::ZERO && notional < min_quote)
    {
        return Err(ExchangeError::Rejected(format!(
            "Lighter {coin} 最小下单量 {min_base} {coin} / {min_quote} USDC，当前 {} {coin} / {notional} USDC",
            request.quantity
        )));
    }

    let nonce = lighter_next_nonce(
        http,
        base_url,
        account_index,
        credentials.key_index.unwrap_or(0),
    )?;
    // 限价单挂单有效期 24 小时，避免旧签名长期有效。
    let expired_at = now_millis() + 24 * 60 * 60 * 1000;
    let order = LighterOrder {
        account_index,
        api_key_index: credentials.key_index.unwrap_or(0),
        market_index,
        size_decimals,
        price_decimals,
        is_ask: request.side == Side::Sell,
        price: request.limit_price,
        size: request.quantity,
        reduce_only: request.reduce_only,
        nonce,
        expired_at,
    };
    let chain_id = if base_url.contains("testnet") {
        300
    } else {
        LIGHTER_MAINNET_CHAIN_ID
    };
    let signed = sign_lighter_order(&order, private_key, chain_id)
        .map_err(|error| ExchangeError::Credentials(error.to_string()))?;

    let form = format!(
        "tx_type={}&tx_info={}",
        signed.tx_type,
        pct(&signed.tx_info)
    );
    let value = http
        .post_body_with_headers(
            &format!("{base_url}/api/v1/sendTx"),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            &form,
        )
        .map_err(transport)?;

    if value["code"].as_i64().unwrap_or(0) != 200 {
        return Err(ExchangeError::Rejected(format!(
            "Lighter 拒单: {}",
            value["message"].as_str().unwrap_or("未知错误")
        )));
    }
    let tx_hash = value["tx_hash"]
        .as_str()
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| ExchangeError::Rejected(format!("Lighter 响应缺少 tx_hash: {value}")))?;
    Ok(ack(tx_hash, client_order_id))
}

fn key_pair(credentials: &AccountCredentials) -> Result<(&str, &str), ExchangeError> {
    if credentials.api_key.is_empty() || credentials.api_secret.is_empty() {
        return Err(ExchangeError::Credentials(format!(
            "{} 账户缺少 API Key / Secret",
            credentials.venue
        )));
    }
    Ok((&credentials.api_key, &credentials.api_secret))
}

/// 交易所返回的金额字段可能是字符串或数字，统一解析成 `Decimal`。
fn decimal_field(value: &Value) -> Option<Decimal> {
    value
        .as_str()
        .and_then(|raw| raw.parse::<Decimal>().ok())
        .or_else(|| value.as_f64().and_then(|raw| Decimal::try_from(raw).ok()))
}

/// 内部交易对（如 `BTCUSDT`）的币种部分，非 USDT 本位返回错误。
fn base_asset(request: &PlaceOrderRequest) -> Result<String, ExchangeError> {
    internal_symbol(request)
        .strip_suffix("USDT")
        .map(str::to_string)
        .filter(|base| !base.is_empty())
        .ok_or_else(|| {
            ExchangeError::Unsupported(format!("仅支持 USDT 本位永续: {}", request.symbol))
        })
}

fn internal_symbol(request: &PlaceOrderRequest) -> String {
    request.symbol.trim().to_ascii_uppercase()
}

/// 交易所客户端订单号只允许字母数字，统一裁剪到 32 位。
fn client_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if cleaned.is_empty() {
        format!("arb{}", now_millis())
    } else {
        cleaned.chars().take(32).collect()
    }
}

fn ack(order_id: impl Into<String>, client_order_id: &str) -> VenueOrderAck {
    VenueOrderAck {
        venue_order_id: order_id.into(),
        client_order_id: client_order_id.to_string(),
        accepted_at: Utc::now(),
    }
}

fn num(value: Decimal) -> String {
    let text = value.to_string();
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

fn decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::String(text) => text.trim().parse().ok(),
        Value::Number(number) => number.to_string().parse().ok(),
        _ => None,
    }
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// RFC3986 百分号编码，HTX 签名要求空格、冒号、斜杠等字符统一编码。
fn pct(value: &str) -> String {
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

fn transport(error: anyhow::Error) -> ExchangeError {
    // 4xx 是交易所对请求本身作出的业务答复（如参数不合法、余额不足），
    // 与网络故障区分开，避免下单失败被显示成「传输失败」。
    if let Some(status) = error.downcast_ref::<HttpStatusError>() {
        if (400..500).contains(&status.status) {
            return ExchangeError::Rejected(format!(
                "交易所拒单（HTTP {}）: {}",
                status.status, status.body
            ));
        }
    }
    ExchangeError::Transport(error.to_string())
}

/// Gate v4 签名：`METHOD\nPATH\nQUERY\nSHA512_HEX(body)\nTIMESTAMP`，HMAC-SHA512 后转 hex。
fn gate_sign(
    secret: &str,
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    timestamp: &str,
) -> Result<String, ExchangeError> {
    let hashed_payload = hex::encode(Sha512::digest(body.as_bytes()));
    let message = format!("{method}\n{path}\n{query}\n{hashed_payload}\n{timestamp}");
    let mut mac = Hmac::<Sha512>::new_from_slice(secret.as_bytes())
        .map_err(|_| ExchangeError::Credentials("密钥长度非法".to_string()))?;
    mac.update(message.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn hmac_hex(secret: &str, payload: &str) -> Result<String, ExchangeError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| ExchangeError::Credentials("密钥长度非法".to_string()))?;
    mac.update(payload.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn hmac_base64(secret: &str, payload: &str) -> Result<String, ExchangeError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| ExchangeError::Credentials("密钥长度非法".to_string()))?;
    mac.update(payload.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}
