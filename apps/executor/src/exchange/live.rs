use std::sync::Arc;

use arb_core::{ExchangeError, PlaceOrderRequest, Side, VenueOrderAck};
use base64::Engine as _;
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    accounts::{AccountCredentials, Accounts},
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
        return Err(ExchangeError::Rejected("下单数量与限价必须大于 0".to_string()));
    }
    let client_order_id = client_id(&request.client_order_id);
    match venue {
        // Aster 的合约下单接口与 Binance 完全兼容。
        "binance" => binance_place(http, &endpoints.binance, credentials, request, &client_order_id),
        "aster" => binance_place(http, &endpoints.aster, credentials, request, &client_order_id),
        "okx" => okx_place(http, &endpoints.okx, credentials, request, &client_order_id),
        "bybit" => bybit_place(http, &endpoints.bybit, credentials, request, &client_order_id),
        "bitget" => bitget_place(http, &endpoints.bitget, credentials, request, &client_order_id),
        "gate" => gate_place(http, &endpoints.gate, credentials, request, &client_order_id),
        "htx" => htx_place(http, &endpoints.htx, credentials, request, &client_order_id),
        "mexc" => mexc_place(http, &endpoints.mexc, credentials, request, &client_order_id),
        "bingx" => bingx_place(http, &endpoints.bingx, credentials, request, &client_order_id),
        "hyperliquid" | "lighter" => Err(ExchangeError::Unsupported(format!(
            "{venue} 合约下单需要钱包私钥签名，当前仅支持钱包地址只读查询"
        ))),
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
    let contract_value = decimal(&instrument["data"][0]["ctVal"]).ok_or_else(|| {
        ExchangeError::Rejected(format!("OKX 未返回 {inst_id} 的合约面值"))
    })?;
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
    let signature = hmac_hex(
        secret,
        &format!("{timestamp}{key}{recv_window}{body}"),
    )?;
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
    let signature = hmac_hex(
        secret,
        &format!(
            "POST\n{path}\n\n{}\n{timestamp}",
            hex::encode(Sha256::digest(body.as_bytes()))
        ),
    )?;
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
    let signature = hmac_hex(
        &credentials.api_secret,
        &format!(
            "GET\n{path}\n\n{}\n{timestamp}",
            hex::encode(Sha256::digest(b""))
        ),
    )?;
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
            ("direction", if request.side == Side::Buy { "buy" } else { "sell" }),
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
        .get_json(&format!("{base_url}/api/v1/contract/detail?symbol={symbol}"))
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
        ("side", if request.side == Side::Buy { "BUY" } else { "SELL" }),
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
        .or_else(|| value["data"]["order"]["orderId"].as_str().map(str::to_string))
        .ok_or_else(|| ExchangeError::Rejected(format!("BingX 响应缺少订单号: {value}")))?;
    Ok(ack(order_id, client_order_id))
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
            ExchangeError::Unsupported(format!(
                "仅支持 USDT 本位永续: {}",
                request.symbol
            ))
        })
}

fn internal_symbol(request: &PlaceOrderRequest) -> String {
    request.symbol.trim().to_ascii_uppercase()
}

/// 交易所客户端订单号只允许字母数字，统一裁剪到 32 位。
fn client_id(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect();
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
