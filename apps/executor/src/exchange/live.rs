use std::sync::Arc;

use arb_core::{ExchangeError, PlaceOrderRequest, Side, VenueOrderAck};
use base64::Engine as _;
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};

use crate::{
    accounts::{AccountCredentials, Accounts},
    exchange::{
        aster,
        dex::{
            hyperliquid_cloid, sign_hyperliquid_action, sign_lighter_order, HyperliquidOrder,
            LighterOrder, LIGHTER_MAINNET_CHAIN_ID,
        },
    },
    http_client::HttpClient,
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
    let contracts = (request.quantity / contract_value).floor();
    if contracts < Decimal::ONE {
        return Err(ExchangeError::Rejected(format!(
            "OKX {inst_id} 数量不足 1 张（每张 {contract_value}）"
        )));
    }

    let body = json!({
        "instId": inst_id,
        "tdMode": "cross",
        "side": if request.side == Side::Buy { "buy" } else { "sell" },
        "ordType": "limit",
        "px": num(request.limit_price),
        "sz": num(contracts),
        "clOrdId": client_order_id,
        "reduceOnly": request.reduce_only,
    })
    .to_string();
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
    let body = json!({
        "symbol": internal_symbol(request),
        "productType": "USDT-FUTURES",
        "marginMode": "crossed",
        "marginCoin": "USDT",
        "size": num(request.quantity),
        "price": num(request.limit_price),
        "side": if request.side == Side::Buy { "buy" } else { "sell" },
        "orderType": "limit",
        "clientOid": client_order_id,
        "reduceOnly": if request.reduce_only { "YES" } else { "NO" },
    })
    .to_string();
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
    let body = json!({
        "contract": contract,
        "size": size,
        "price": num(request.limit_price),
        "tif": "gtc",
        "reduce_only": request.reduce_only,
        "text": format!("t-{client_order_id}"),
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
            ("lever_rate", "20"),
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
    let body = json!({
        "symbol": symbol,
        "price": num(request.limit_price),
        "vol": volume,
        "leverage": 20,
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

    let nonce = now_millis();
    // 限价单挂单有效期 24 小时，避免旧签名长期有效。
    let expired_at = nonce + 24 * 60 * 60 * 1000;
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
