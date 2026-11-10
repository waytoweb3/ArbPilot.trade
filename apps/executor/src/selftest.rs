//! 交易所下单 / 平仓临时自检。
//!
//! 通过环境变量 `ARBPILOT_SELFTEST=1` 触发：逐家交易所用最小可成交数量开一张
//! 限价单，确认成交后立即用 reduce-only 反向单平掉，并把结果打印成表格。
//! 不启动 HTTP 服务，也不写入业务库，测完可以直接删除本文件与 `main.rs` 里的入口。

use std::{sync::Arc, time::Duration};

use arb_core::{PlaceOrderRequest, Position, Side};
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::time::sleep;

use crate::{
    accounts::{AccountEndpoints, Accounts},
    config::Config,
    exchange::{LiveEndpoints, LiveExchange},
    http_client::HttpClient,
    market,
    store::Store,
};

/// 单家交易所的自检结果。
struct Report {
    venue: String,
    equity: String,
    available: String,
    open: String,
    close: String,
    note: String,
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let symbol = env_string("ARBPILOT_SELFTEST_SYMBOL").unwrap_or_else(|| "BTCUSDT".to_string());
    let leverage = env_decimal("ARBPILOT_SELFTEST_LEVERAGE").unwrap_or_else(|| Decimal::from(10));
    let venues: Vec<String> = env_string("ARBPILOT_SELFTEST_VENUES")
        .unwrap_or_else(|| "binance,okx,bybit,bitget,gate,aster,hyperliquid,lighter".to_string())
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();

    println!("自检开始: symbol={symbol} leverage={leverage} venues={venues:?}");

    let http = Arc::new(HttpClient::new(
        Duration::from_secs(15),
        config.market_proxy.clone(),
    )?);
    let store = Arc::new(Store::connect_if_configured(None, None).await);
    let accounts = Accounts::start(
        Some(http.clone()),
        AccountEndpoints {
            binance_spot: config.binance_spot_url.clone(),
            binance_fapi: config.binance_fapi_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            gate: config.gate_base_url.clone(),
            aster: config.aster_base_url.clone(),
            htx: config.htx_base_url.clone(),
            mexc: config.mexc_base_url.clone(),
            bingx: config.bingx_base_url.clone(),
            hyperliquid: config.hyperliquid_base_url.clone(),
            lighter: config.lighter_base_url.clone(),
        },
        config.accounts_file.clone(),
        3600,
        store,
    );
    let exchange = LiveExchange::new(
        Some(http.clone()),
        LiveEndpoints {
            binance: config.binance_fapi_url.clone(),
            okx: config.okx_base_url.clone(),
            bybit: config.bybit_base_url.clone(),
            bitget: config.bitget_base_url.clone(),
            gate: config.gate_base_url.clone(),
            aster: config.aster_base_url.clone(),
            htx: config.htx_base_url.clone(),
            mexc: config.mexc_base_url.clone(),
            bingx: config.bingx_base_url.clone(),
            hyperliquid: config.hyperliquid_base_url.clone(),
            lighter: config.lighter_base_url.clone(),
        },
        accounts.clone(),
    );

    let mut reports = Vec::new();
    let close_only = env_string("ARBPILOT_SELFTEST_CLOSE_ONLY").is_some();
    for venue in &venues {
        let report = if close_only {
            close_only_venue(
                &config, &http, &exchange, &accounts, venue, &symbol, leverage,
            )
            .await
        } else {
            test_venue(
                &config, &http, &exchange, &accounts, venue, &symbol, leverage,
            )
            .await
        };
        reports.push(report);
    }

    println!();
    println!(
        "{:<12} | {:<10} | {:<10} | {:<34} | {:<34} | {}",
        "venue", "equity", "avail", "开仓", "平仓", "备注"
    );
    println!("{}", "-".repeat(140));
    for report in &reports {
        println!(
            "{:<12} | {:<10} | {:<10} | {:<34} | {:<34} | {}",
            report.venue, report.equity, report.available, report.open, report.close, report.note
        );
    }

    Ok(())
}

async fn test_venue(
    config: &Config,
    http: &Arc<HttpClient>,
    exchange: &LiveExchange,
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
    leverage: Decimal,
) -> Report {
    let mut report = Report {
        venue: venue.to_string(),
        equity: "-".to_string(),
        available: "-".to_string(),
        open: "未执行".to_string(),
        close: "未执行".to_string(),
        note: String::new(),
    };

    let Some(credentials) = accounts.credentials_for(venue) else {
        report.note = "未导入账户".to_string();
        return report;
    };

    // 预检：账户快照 + 是否已有同交易对持仓（避免干扰真实仓位）。
    match accounts.refresh(credentials.id).await {
        Ok(view) => {
            if let Some(snapshot) = view.snapshot {
                report.equity = fmt(snapshot.equity_usd);
                report.available = snapshot
                    .available_margin_usd
                    .map(fmt)
                    .unwrap_or_else(|| "-".to_string());
                if let Some(error) = snapshot.error {
                    push_note(&mut report.note, &format!("预检错误: {error}"));
                }
            }
        }
        Err(error) => push_note(&mut report.note, &format!("预检失败: {error}")),
    }

    match accounts.open_positions_for(venue).await {
        Ok(positions)
            if positions
                .iter()
                .any(|p| p.symbol.eq_ignore_ascii_case(symbol)) =>
        {
            report.note = format!("已有 {symbol} 持仓，跳过以免干扰");
            return report;
        }
        Ok(_) => {}
        Err(error) => push_note(&mut report.note, &format!("查询持仓失败: {error}")),
    }

    // 取对手价作为限价（与执行器实盘市价腿一致），数量按各所最小可成交。
    let book = match fetch_book(config, http, venue, symbol).await {
        Ok(book) => book,
        Err(error) => {
            report.note = format!("获取行情失败: {error}");
            return report;
        }
    };
    let quantity = min_quantity(venue, book.ask);
    let open_price = nudge_up(book.ask);
    report.note = format!(
        "计划 {} @ {} ≈ ${}",
        quantity,
        open_price,
        quantity * open_price
    );

    if let Err(error) = exchange.set_leverage(venue, symbol, leverage).await {
        push_note(&mut report.note, &format!("设置杠杆失败: {error}"));
    }

    let request = PlaceOrderRequest {
        symbol: symbol.to_string(),
        side: Side::Buy,
        quantity,
        limit_price: open_price,
        reduce_only: false,
        client_order_id: format!("selftest-open-{}", Utc::now().timestamp_millis()),
        leverage: Some(leverage),
    };
    match exchange.place_order(venue, request).await {
        Ok(ack) => report.open = format!("已受理 {}", ack.venue_order_id),
        Err(error) => {
            report.open = "失败".to_string();
            push_note(&mut report.note, &format!("开仓: {error}"));
            return report;
        }
    }

    let close_quantity = match wait_position(accounts, venue, symbol, Duration::from_secs(15)).await
    {
        Ok(Some(position)) => {
            report.open = format!("成交 {} 张/币", position.quantity);
            position.quantity
        }
        Ok(None) => {
            report.open = "已受理但未确认成交".to_string();
            push_note(
                &mut report.note,
                "15 秒内未看到持仓，仍尝试 reduce-only 平仓以确保安全",
            );
            quantity
        }
        Err(error) => {
            push_note(
                &mut report.note,
                &format!("确认持仓失败: {error}，仍尝试平仓"),
            );
            quantity
        }
    };
    close_position(
        config,
        http,
        exchange,
        accounts,
        venue,
        symbol,
        close_quantity,
        leverage,
        &mut report,
    )
    .await;
    report
}

/// 只平不开：把该交易对已有持仓用 reduce-only 单平掉，供开仓自检后单独补平。
async fn close_only_venue(
    config: &Config,
    http: &Arc<HttpClient>,
    exchange: &LiveExchange,
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
    leverage: Decimal,
) -> Report {
    let mut report = Report {
        venue: venue.to_string(),
        equity: "-".to_string(),
        available: "-".to_string(),
        open: "未执行".to_string(),
        close: "未执行".to_string(),
        note: String::new(),
    };

    let Some(credentials) = accounts.credentials_for(venue) else {
        report.note = "未导入账户".to_string();
        return report;
    };

    match accounts.refresh(credentials.id).await {
        Ok(view) => {
            if let Some(snapshot) = view.snapshot {
                report.equity = fmt(snapshot.equity_usd);
                report.available = snapshot
                    .available_margin_usd
                    .map(fmt)
                    .unwrap_or_else(|| "-".to_string());
                if let Some(error) = snapshot.error {
                    push_note(&mut report.note, &format!("预检错误: {error}"));
                }
            }
        }
        Err(error) => push_note(&mut report.note, &format!("预检失败: {error}")),
    }

    let position = match positions_with_retry(accounts, venue, symbol).await {
        Ok(positions) => positions.into_iter().next(),
        Err(error) => {
            push_note(&mut report.note, &format!("查询持仓失败: {error}"));
            return report;
        }
    };
    let Some(position) = position else {
        report.open = "无持仓".to_string();
        report.close = "跳过".to_string();
        return report;
    };
    report.open = format!("已有 {} 张/币", position.quantity);

    close_position(
        config,
        http,
        exchange,
        accounts,
        venue,
        symbol,
        position.quantity,
        leverage,
        &mut report,
    )
    .await;
    report
}

/// 用 reduce-only 反向单平掉该交易对持仓，最多尝试 3 次，每次重新取买一。
async fn close_position(
    config: &Config,
    http: &Arc<HttpClient>,
    exchange: &LiveExchange,
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
    quantity: Decimal,
    leverage: Decimal,
    report: &mut Report,
) {
    for attempt in 1..=3 {
        let close_book = match fetch_book(config, http, venue, symbol).await {
            Ok(book) => book,
            Err(error) => {
                report.close = format!("获取行情失败: {error}");
                break;
            }
        };
        let request = PlaceOrderRequest {
            symbol: symbol.to_string(),
            side: Side::Sell,
            quantity,
            limit_price: nudge_down(close_book.bid),
            reduce_only: true,
            client_order_id: format!("selftest-close-{}-{attempt}", Utc::now().timestamp_millis()),
            leverage: Some(leverage),
        };
        match exchange.place_order(venue, request).await {
            Ok(ack) => report.close = format!("第 {attempt} 次已受理 {}", ack.venue_order_id),
            Err(error) => {
                report.close = format!("第 {attempt} 次失败: {error}");
                if attempt < 3 {
                    sleep(Duration::from_secs(2)).await;
                }
                continue;
            }
        }
        match wait_flat(accounts, venue, symbol, Duration::from_secs(20)).await {
            Ok(true) => {
                report.close = format!("已平仓（第 {attempt} 次）");
                break;
            }
            Ok(false) => {
                report.close = format!("第 {attempt} 次后仍有持仓");
                if attempt < 3 {
                    sleep(Duration::from_secs(2)).await;
                }
            }
            Err(error) => {
                report.close = format!("确认平仓失败: {error}");
                break;
            }
        }
    }
}

/// 自检使用的各交易所最小可成交数量（BTCUSDT）。
fn min_quantity(venue: &str, price: Decimal) -> Decimal {
    match venue {
        // Binance / Aster / Bybit 的 BTC 合约最小变动为 0.001 BTC。
        "binance" | "aster" | "bybit" => Decimal::new(1, 3),
        // Bitget 最小 0.0001 BTC。
        "bitget" => Decimal::new(1, 4),
        // OKX / Gate 内部按张数换算，0.0001 BTC 即 1 张。
        "okx" | "gate" => Decimal::new(1, 4),
        // Hyperliquid 最小名义额 10 美元，步长 0.00001 BTC。
        "hyperliquid" => ceil_to(Decimal::from(10) / price, Decimal::new(1, 5)),
        // Lighter 同时限制最小币量与 10 USDC 名义额。
        "lighter" => ceil_to(Decimal::from(10) / price, Decimal::new(1, 4)).max(Decimal::new(1, 4)),
        _ => Decimal::new(1, 4),
    }
}

async fn fetch_book(
    config: &Config,
    http: &Arc<HttpClient>,
    venue: &str,
    symbol: &str,
) -> anyhow::Result<crate::exchange::BookSnapshot> {
    let config = config.clone();
    let http = http.clone();
    let venue = venue.to_string();
    let symbol = symbol.to_string();
    tokio::task::spawn_blocking(move || market::fetch_book_quote(&config, http, &venue, &symbol))
        .await?
}

async fn positions_with_retry(
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
) -> anyhow::Result<Vec<Position>> {
    let mut last_error = None;
    for _ in 0..3 {
        match accounts.open_positions_for(venue).await {
            Ok(positions) => {
                return Ok(positions
                    .into_iter()
                    .filter(|position| position.symbol.eq_ignore_ascii_case(symbol))
                    .collect());
            }
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("查询持仓失败")))
}

async fn wait_position(
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
    timeout: Duration,
) -> anyhow::Result<Option<Position>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let positions = accounts.open_positions_for(venue).await?;
        if let Some(position) = positions
            .into_iter()
            .find(|position| position.symbol.eq_ignore_ascii_case(symbol))
        {
            return Ok(Some(position));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_flat(
    accounts: &Arc<Accounts>,
    venue: &str,
    symbol: &str,
    timeout: Duration,
) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let positions = accounts.open_positions_for(venue).await?;
        if !positions
            .iter()
            .any(|position| position.symbol.eq_ignore_ascii_case(symbol))
        {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_secs(2)).await;
    }
}

fn ceil_to(value: Decimal, step: Decimal) -> Decimal {
    (value / step).ceil() * step
}

/// 买入限价上浮 5bp 并取整到 0.1，抵消行情到撮合之间的漂移，确保立即成交。
fn nudge_up(price: Decimal) -> Decimal {
    ceil_to(price * Decimal::new(10005, 4), Decimal::new(1, 1))
}

/// 卖出限价下浮 5bp 并取整到 0.1，抵消行情到撮合之间的漂移，确保立即成交。
fn nudge_down(price: Decimal) -> Decimal {
    ((price * Decimal::new(9995, 4)) / Decimal::new(1, 1)).floor() * Decimal::new(1, 1)
}

fn fmt(value: Decimal) -> String {
    value.normalize().to_string()
}

fn push_note(note: &mut String, message: &str) {
    if !note.is_empty() {
        note.push_str("；");
    }
    note.push_str(message);
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_decimal(key: &str) -> Option<Decimal> {
    env_string(key).and_then(|value| value.parse().ok())
}
