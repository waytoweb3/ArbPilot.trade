use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{anyhow, bail};
use arb_core::{Opportunity, OpportunityKind, Quote};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    config::Config,
    events::ExecutorEvent,
    exchange::{
        BinanceMarketClient, BingxFundingClient, BitgetFundingClient, BookSnapshot,
        BybitFundingClient, Candle, ContractSpec, DeliveryContract, DepthSnapshot, FundingRate,
        GateFundingClient, HtxFundingClient, HyperliquidFundingClient, LighterFundingClient,
        MexcFundingClient, MinOrderSpec, OkxMarketClient, Segment, ASTER_VENUE, BINGX_VENUE,
        BITGET_VENUE, BYBIT_VENUE, GATE_VENUE, HTX_VENUE, HYPERLIQUID_VENUE, LIGHTER_VENUE,
        MEXC_VENUE, OKX_VENUE, VENUE,
    },
    funding::{annualize, FundingFeed},
    http_client::HttpClient,
    notifier::Notifier,
    realtime::RealtimeFeed,
    store::Store,
};

/// Binance 现货腿标识；Binance 永续沿用 `binance`。
pub const SPOT_VENUE: &str = "binance-spot";
/// OKX 现货腿标识；OKX 永续沿用 `okx`。
pub const OKX_SPOT_VENUE: &str = "okx-spot";
/// Bybit 现货腿标识；Bybit 永续沿用 `bybit`。
pub const BYBIT_SPOT_VENUE: &str = "bybit-spot";

/// 单腿手续费估算（bps）：现货与永续各 4 bps。
const LEG_FEE_BPS: i64 = 4;
/// 双腿滑点估算（bps）。
const SLIPPAGE_BPS: i64 = 1;
/// 仓位实时定价允许的 WebSocket 快照最大年龄；超过视为断线，回退交易所标记价。
const REALTIME_PRICE_MAX_AGE_SECS: i64 = 30;
/// 交割合约清单的刷新间隔：合约按季度挂牌，无需每轮行情都重新拉取。
const DELIVERY_REFRESH_SECS: i64 = 3_600;
/// 跨所比较以 `funding_annualized_pct`（年化）为准；bps 字段仍按 8 小时一期展示，
/// 未能确认结算间隔时也回退到该值。
const FUNDING_TARGET_INTERVAL_SECS: i64 = 8 * 3600;
const KLINE_INTERVALS: [&str; 9] = ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w", "10m"];
/// 30 秒 K 线由 WebSocket 盘口逐秒采样聚合，REST 只提供分钟级历史。
const TICK_BAR_SECONDS: i64 = 30;
/// 单个交易对保留的 30 秒 K 线上限（30s × 480 = 4 小时）。
const TICK_BAR_CAPACITY: usize = 480;

/// 秒级 K 线内存聚合器。
///
/// 交易所 REST 基本不提供 30 秒历史（Binance 永续最低 1 分钟），
/// 这里对 WebSocket 盘口中间价逐秒采样，跨周期边界时开新的一根。
#[derive(Default)]
struct TickBars {
    inner: RwLock<TickBarsState>,
}

#[derive(Default)]
struct TickBarsState {
    watched: HashSet<(String, String)>,
    bars: HashMap<(String, String), Vec<Candle>>,
}

impl TickBars {
    /// 标记需要采样的交易对；采样器会持续为它维护 30 秒 K 线。
    fn watch(&self, venue: &str, symbol: &str) {
        self.inner
            .write()
            .expect("秒级 K 线锁不可用")
            .watched
            .insert((venue.to_string(), symbol.to_string()));
    }

    fn watched(&self) -> Vec<(String, String)> {
        self.inner
            .read()
            .expect("秒级 K 线锁不可用")
            .watched
            .iter()
            .cloned()
            .collect()
    }

    /// 用一次盘口中间价采样更新当前 K 线。
    fn record(&self, venue: &str, symbol: &str, price: Decimal, at: DateTime<Utc>) {
        let bucket = at.timestamp() - at.timestamp().rem_euclid(TICK_BAR_SECONDS);
        let Some(open_time) = DateTime::from_timestamp(bucket, 0) else {
            return;
        };
        let key = (venue.to_string(), symbol.to_string());
        let mut inner = self.inner.write().expect("秒级 K 线锁不可用");
        let bars = inner.bars.entry(key).or_default();
        if let Some(last) = bars.last_mut() {
            let last_at = last.open_time.timestamp();
            if last_at == bucket {
                last.high = last.high.max(price);
                last.low = last.low.min(price);
                last.close = price;
                return;
            }
            // 采样时间回退（时钟漂移）时忽略这一笔。
            if last_at > bucket {
                return;
            }
        }
        bars.push(Candle {
            open_time,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: Decimal::ZERO,
        });
        if bars.len() > TICK_BAR_CAPACITY {
            let excess = bars.len() - TICK_BAR_CAPACITY;
            bars.drain(..excess);
        }
    }

    fn series(&self, venue: &str, symbol: &str, limit: usize) -> Vec<Candle> {
        let inner = self.inner.read().expect("秒级 K 线锁不可用");
        let Some(bars) = inner.bars.get(&(venue.to_string(), symbol.to_string())) else {
            return Vec::new();
        };
        let start = bars.len().saturating_sub(limit);
        bars[start..].to_vec()
    }
}

/// 盘口快照，同时保留买一/卖一档的名义价值用于估算可成交额度。
#[derive(Debug, Clone)]
struct Book {
    quote: Quote,
    bid_notional_usd: Decimal,
    ask_notional_usd: Decimal,
}

#[derive(Default)]
struct MarketState {
    books: HashMap<(String, String), Book>,
    opportunities: Vec<Opportunity>,
    /// 交割合约基差机会，单独一张表，不与资金费率机会混排。
    basis: Vec<Opportunity>,
}

/// 交割合约清单缓存：合约按季度挂牌，低频刷新即可。
#[derive(Default)]
struct DeliveryCache {
    contracts: Vec<DeliveryContract>,
    refreshed_at: Option<DateTime<Utc>>,
}

/// REST 兜底用的盘口客户端；按 venue 分派到对应交易所。
#[derive(Clone, Default)]
struct BookClients {
    binance: Option<Arc<BinanceMarketClient>>,
    okx: Option<Arc<OkxMarketClient>>,
    bybit: Option<Arc<BybitFundingClient>>,
    bitget: Option<Arc<BitgetFundingClient>>,
    gate: Option<Arc<GateFundingClient>>,
    /// Aster 的合约接口与 Binance 兼容，复用同一客户端实现。
    aster: Option<Arc<BinanceMarketClient>>,
    hyperliquid: Option<Arc<HyperliquidFundingClient>>,
    lighter: Option<Arc<LighterFundingClient>>,
    htx: Option<Arc<HtxFundingClient>>,
    mexc: Option<Arc<MexcFundingClient>>,
    bingx: Option<Arc<BingxFundingClient>>,
}

impl BookClients {
    fn book(&self, venue: &str, symbol: &str, segment: Segment) -> anyhow::Result<BookSnapshot> {
        match venue {
            SPOT_VENUE | VENUE => self
                .binance
                .as_ref()
                .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?
                .book(symbol, segment),
            OKX_SPOT_VENUE | OKX_VENUE => self
                .okx
                .as_ref()
                .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?
                .book(symbol, segment),
            BYBIT_SPOT_VENUE => self
                .bybit
                .as_ref()
                .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?
                .spot_book(symbol),
            BYBIT_VENUE => self
                .bybit
                .as_ref()
                .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?
                .book(symbol),
            BITGET_VENUE => self
                .bitget
                .as_ref()
                .ok_or_else(|| anyhow!("Bitget 行情客户端未启用"))?
                .book(symbol),
            GATE_VENUE => self
                .gate
                .as_ref()
                .ok_or_else(|| anyhow!("Gate 行情客户端未启用"))?
                .book(symbol),
            ASTER_VENUE => self
                .aster
                .as_ref()
                .ok_or_else(|| anyhow!("Aster 行情客户端未启用"))?
                .book(symbol, Segment::Swap),
            HYPERLIQUID_VENUE => self
                .hyperliquid
                .as_ref()
                .ok_or_else(|| anyhow!("Hyperliquid 行情客户端未启用"))?
                .book(symbol),
            LIGHTER_VENUE => self
                .lighter
                .as_ref()
                .ok_or_else(|| anyhow!("Lighter 行情客户端未启用"))?
                .book(symbol),
            HTX_VENUE => self
                .htx
                .as_ref()
                .ok_or_else(|| anyhow!("HTX 行情客户端未启用"))?
                .book(symbol),
            MEXC_VENUE => self
                .mexc
                .as_ref()
                .ok_or_else(|| anyhow!("MEXC 行情客户端未启用"))?
                .book(symbol),
            BINGX_VENUE => self
                .bingx
                .as_ref()
                .ok_or_else(|| anyhow!("BingX 行情客户端未启用"))?
                .book(symbol),
            other => bail!("不支持的行情来源: {other}"),
        }
    }

    /// 多档盘口；按 venue 分派到对应交易所的 depth 端点。
    fn depth(
        &self,
        venue: &str,
        symbol: &str,
        segment: Segment,
        limit: u32,
    ) -> anyhow::Result<DepthSnapshot> {
        match venue {
            SPOT_VENUE | VENUE => self
                .binance
                .as_ref()
                .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?
                .depth(symbol, segment, limit),
            OKX_SPOT_VENUE | OKX_VENUE => self
                .okx
                .as_ref()
                .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?
                .depth(symbol, segment, limit),
            BYBIT_SPOT_VENUE => self
                .bybit
                .as_ref()
                .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?
                .depth(symbol, true, limit),
            BYBIT_VENUE => self
                .bybit
                .as_ref()
                .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?
                .depth(symbol, false, limit),
            BITGET_VENUE => self
                .bitget
                .as_ref()
                .ok_or_else(|| anyhow!("Bitget 行情客户端未启用"))?
                .depth(symbol, limit),
            GATE_VENUE => self
                .gate
                .as_ref()
                .ok_or_else(|| anyhow!("Gate 行情客户端未启用"))?
                .depth(symbol, limit),
            ASTER_VENUE => self
                .aster
                .as_ref()
                .ok_or_else(|| anyhow!("Aster 行情客户端未启用"))?
                .depth(symbol, Segment::Swap, limit),
            HYPERLIQUID_VENUE => self
                .hyperliquid
                .as_ref()
                .ok_or_else(|| anyhow!("Hyperliquid 行情客户端未启用"))?
                .depth(symbol, limit),
            LIGHTER_VENUE => self
                .lighter
                .as_ref()
                .ok_or_else(|| anyhow!("Lighter 行情客户端未启用"))?
                .depth(symbol, limit),
            HTX_VENUE => self
                .htx
                .as_ref()
                .ok_or_else(|| anyhow!("HTX 行情客户端未启用"))?
                .depth(symbol),
            MEXC_VENUE => self
                .mexc
                .as_ref()
                .ok_or_else(|| anyhow!("MEXC 行情客户端未启用"))?
                .depth(symbol, limit),
            BINGX_VENUE => self
                .bingx
                .as_ref()
                .ok_or_else(|| anyhow!("BingX 行情客户端未启用"))?
                .depth(symbol, limit),
            other => bail!("不支持的行情来源: {other}"),
        }
    }

    /// 最小下单限制；按 venue 分派到对应交易所的合约规格接口。
    fn min_order(
        &self,
        venue: &str,
        symbol: &str,
        segment: Segment,
    ) -> anyhow::Result<Option<MinOrderSpec>> {
        match venue {
            SPOT_VENUE | VENUE => self
                .binance
                .as_ref()
                .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?
                .min_order(venue, symbol, segment),
            OKX_SPOT_VENUE | OKX_VENUE => self
                .okx
                .as_ref()
                .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?
                .min_order(symbol),
            BYBIT_SPOT_VENUE | BYBIT_VENUE => self
                .bybit
                .as_ref()
                .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?
                .min_order(symbol),
            BITGET_VENUE => self
                .bitget
                .as_ref()
                .ok_or_else(|| anyhow!("Bitget 行情客户端未启用"))?
                .min_order(symbol),
            GATE_VENUE => self
                .gate
                .as_ref()
                .ok_or_else(|| anyhow!("Gate 行情客户端未启用"))?
                .min_order(symbol),
            ASTER_VENUE => self
                .aster
                .as_ref()
                .ok_or_else(|| anyhow!("Aster 行情客户端未启用"))?
                .min_order(venue, symbol, Segment::Swap),
            HYPERLIQUID_VENUE => self
                .hyperliquid
                .as_ref()
                .ok_or_else(|| anyhow!("Hyperliquid 行情客户端未启用"))?
                .min_order(symbol),
            LIGHTER_VENUE => self
                .lighter
                .as_ref()
                .ok_or_else(|| anyhow!("Lighter 行情客户端未启用"))?
                .min_order(symbol),
            HTX_VENUE => self
                .htx
                .as_ref()
                .ok_or_else(|| anyhow!("HTX 行情客户端未启用"))?
                .min_order(symbol),
            MEXC_VENUE => self
                .mexc
                .as_ref()
                .ok_or_else(|| anyhow!("MEXC 行情客户端未启用"))?
                .min_order(symbol),
            BINGX_VENUE => self
                .bingx
                .as_ref()
                .ok_or_else(|| anyhow!("BingX 行情客户端未启用"))?
                .min_order(symbol),
            other => bail!("不支持的行情来源: {other}"),
        }
    }
}

/// 自检用：单次拉取某交易所某交易对的买一/卖一，不启动后台行情采集。
pub fn fetch_book_quote(
    config: &Config,
    http: Arc<HttpClient>,
    venue: &str,
    symbol: &str,
) -> anyhow::Result<BookSnapshot> {
    let venue = venue.trim().to_ascii_lowercase();
    let book = match venue.as_str() {
        VENUE => BinanceMarketClient::new(&config.binance_spot_url, &config.binance_fapi_url, http)
            .book(symbol, Segment::Swap)?,
        OKX_VENUE => OkxMarketClient::new(&config.okx_base_url, http).book(symbol, Segment::Swap)?,
        BYBIT_VENUE => BybitFundingClient::new(&config.bybit_base_url, http).book(symbol)?,
        BITGET_VENUE => BitgetFundingClient::new(&config.bitget_base_url, http).book(symbol)?,
        GATE_VENUE => GateFundingClient::new(&config.gate_base_url, http).book(symbol)?,
        ASTER_VENUE => {
            BinanceMarketClient::new(&config.aster_base_url, &config.aster_base_url, http)
                .book(symbol, Segment::Swap)?
        }
        HYPERLIQUID_VENUE => {
            HyperliquidFundingClient::new(&config.hyperliquid_base_url, http).book(symbol)?
        }
        LIGHTER_VENUE => LighterFundingClient::new(&config.lighter_base_url, http).book(symbol)?,
        other => bail!("自检暂不支持该交易所: {other}"),
    };
    Ok(book)
}

/// 真实行情源：WebSocket 推送优先，REST 只在推送缺失或过期时兜底。
///
/// 永续盘口来自各交易所，并结合已入库的资金费率生成「永续-永续」资金费率差机会；
/// 现货盘口只用于交割基差与 K 线。
pub struct MarketFeed {
    symbols: Vec<String>,
    poll: Duration,
    /// WebSocket 快照的最长可用时长，超过即回退到 REST。
    ws_max_age: Duration,
    binance: Option<Arc<BinanceMarketClient>>,
    okx: Option<Arc<OkxMarketClient>>,
    bybit: Option<Arc<BybitFundingClient>>,
    bitget: Option<Arc<BitgetFundingClient>>,
    gate: Option<Arc<GateFundingClient>>,
    /// Aster 的合约接口与 Binance 兼容，复用同一客户端实现。
    aster: Option<Arc<BinanceMarketClient>>,
    hyperliquid: Option<Arc<HyperliquidFundingClient>>,
    lighter: Option<Arc<LighterFundingClient>>,
    htx: Option<Arc<HtxFundingClient>>,
    mexc: Option<Arc<MexcFundingClient>>,
    bingx: Option<Arc<BingxFundingClient>>,
    /// 参与永续-永续资金费率差的交易所（只含已启用且已建客户端的）。
    perp_venues: Vec<&'static str>,
    realtime: Option<Arc<RealtimeFeed>>,
    store: Arc<Store>,
    state: RwLock<MarketState>,
    /// 30 秒 K 线由 WebSocket 盘口采样聚合。
    tick_bars: TickBars,
    /// 交割合约清单缓存（Binance / OKX / Bybit）。
    delivery: RwLock<DeliveryCache>,
    events: broadcast::Sender<ExecutorEvent>,
    /// 资金费率机会按通知设置推送；未挂载时只入库不推送。
    notifier: RwLock<Option<Arc<Notifier>>>,
    /// 资金费率采集器，用于把不同结算间隔的费率归一到 8 小时。
    funding: RwLock<Option<Arc<FundingFeed>>>,
}

impl MarketFeed {
    pub fn start(
        config: &Config,
        http: Option<Arc<HttpClient>>,
        realtime: Option<Arc<RealtimeFeed>>,
        store: Arc<Store>,
        events: broadcast::Sender<ExecutorEvent>,
    ) -> Arc<Self> {
        let (binance, okx, bybit, bitget, gate, aster, hyperliquid, lighter, htx, mexc, bingx) =
            match http {
                Some(http) => {
                    let binance = Some(Arc::new(BinanceMarketClient::new(
                        &config.binance_spot_url,
                        &config.binance_fapi_url,
                        http.clone(),
                    )));
                    let okx = config.okx_enabled.then(|| {
                        Arc::new(OkxMarketClient::new(&config.okx_base_url, http.clone()))
                    });
                    let bybit = config.bybit_enabled.then(|| {
                        Arc::new(BybitFundingClient::new(
                            &config.bybit_base_url,
                            http.clone(),
                        ))
                    });
                    let bitget = config.bitget_enabled.then(|| {
                        Arc::new(BitgetFundingClient::new(
                            &config.bitget_base_url,
                            http.clone(),
                        ))
                    });
                    let gate = config.gate_enabled.then(|| {
                        Arc::new(GateFundingClient::new(&config.gate_base_url, http.clone()))
                    });
                    // Aster 的合约接口与 Binance 完全兼容，现货地址用不到，填同一域名即可。
                    let aster = config.aster_enabled.then(|| {
                        Arc::new(BinanceMarketClient::new(
                            &config.aster_base_url,
                            &config.aster_base_url,
                            http.clone(),
                        ))
                    });
                    let hyperliquid = config.hyperliquid_enabled.then(|| {
                        Arc::new(HyperliquidFundingClient::new(
                            &config.hyperliquid_base_url,
                            http.clone(),
                        ))
                    });
                    let lighter = config.lighter_enabled.then(|| {
                        Arc::new(LighterFundingClient::new(
                            &config.lighter_base_url,
                            http.clone(),
                        ))
                    });
                    let htx = config.htx_enabled.then(|| {
                        Arc::new(HtxFundingClient::new(&config.htx_base_url, http.clone()))
                    });
                    let mexc = config.mexc_enabled.then(|| {
                        Arc::new(MexcFundingClient::new(&config.mexc_base_url, http.clone()))
                    });
                    let bingx = config.bingx_enabled.then(|| {
                        Arc::new(BingxFundingClient::new(
                            &config.bingx_base_url,
                            http.clone(),
                        ))
                    });
                    (
                        binance,
                        okx,
                        bybit,
                        bitget,
                        gate,
                        aster,
                        hyperliquid,
                        lighter,
                        htx,
                        mexc,
                        bingx,
                    )
                }
                None => {
                    tracing::error!("行情 HTTP 客户端不可用，盘口只能依赖 WebSocket 推送");
                    (
                        None, None, None, None, None, None, None, None, None, None, None,
                    )
                }
            };

        let mut perp_venues = Vec::new();
        for (enabled, venue) in [
            (binance.is_some(), VENUE),
            (okx.is_some(), OKX_VENUE),
            (bybit.is_some(), BYBIT_VENUE),
            (bitget.is_some(), BITGET_VENUE),
            (gate.is_some(), GATE_VENUE),
            (aster.is_some(), ASTER_VENUE),
            (hyperliquid.is_some(), HYPERLIQUID_VENUE),
            (lighter.is_some(), LIGHTER_VENUE),
            (htx.is_some(), HTX_VENUE),
            (mexc.is_some(), MEXC_VENUE),
            (bingx.is_some(), BINGX_VENUE),
        ] {
            if enabled {
                perp_venues.push(venue);
            }
        }

        let poll = Duration::from_secs(config.quote_poll_secs.max(1));
        let feed = Arc::new(Self {
            symbols: config.market_symbols.clone(),
            poll,
            ws_max_age: poll * 2,
            binance,
            okx,
            bybit,
            bitget,
            gate,
            aster,
            hyperliquid,
            lighter,
            htx,
            mexc,
            bingx,
            perp_venues,
            realtime,
            store,
            state: RwLock::new(MarketState::default()),
            tick_bars: TickBars::default(),
            delivery: RwLock::new(DeliveryCache::default()),
            events,
            notifier: RwLock::new(None),
            funding: RwLock::new(None),
        });

        if feed.symbols.is_empty() || (feed.perp_venues.is_empty() && feed.realtime.is_none()) {
            tracing::info!("盘口采集已停用（检查 ARBPILOT_BINANCE_* / ARBPILOT_OKX_* 与 ARBPILOT_MARKET_SYMBOLS）");
            return feed;
        }

        tracing::info!(
            binance_spot = %config.binance_spot_url,
            binance_futures = %config.binance_fapi_url,
            okx_enabled = feed.okx.is_some(),
            okx_base_url = %config.okx_base_url,
            bybit_enabled = feed.bybit.is_some(),
            bitget_enabled = feed.bitget.is_some(),
            gate_enabled = feed.gate.is_some(),
            aster_enabled = feed.aster.is_some(),
            hyperliquid_enabled = feed.hyperliquid.is_some(),
            lighter_enabled = feed.lighter.is_some(),
            htx_enabled = feed.htx.is_some(),
            mexc_enabled = feed.mexc.is_some(),
            bingx_enabled = feed.bingx.is_some(),
            perp_venues = ?feed.perp_venues,
            websocket = feed.realtime.is_some(),
            symbols = ?feed.symbols,
            poll_secs = feed.poll.as_secs(),
            ws_max_age_secs = feed.ws_max_age.as_secs(),
            proxy = ?config.market_proxy,
            "盘口采集已启动"
        );

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(worker.poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                worker.refresh().await;
            }
        });

        // 秒级 K 线采样：每秒把已订阅交易对的盘口中间价写进 30s 聚合器。
        let sampler = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                for (venue, symbol) in sampler.tick_bars.watched() {
                    let Some((price, at)) = sampler.realtime_quote(&venue, &symbol) else {
                        continue;
                    };
                    sampler.tick_bars.record(&venue, &symbol, price, at);
                }
            }
        });

        feed
    }

    pub fn opportunities(&self) -> Vec<Opportunity> {
        self.state
            .read()
            .expect("行情锁不可用")
            .opportunities
            .clone()
    }

    /// 交割合约基差机会（现货多 + 交割空），按年化基差排序。
    pub fn basis_opportunities(&self) -> Vec<Opportunity> {
        self.state.read().expect("行情锁不可用").basis.clone()
    }

    /// 绑定通知分发器后，资金费率机会会按通知设置推送。
    pub fn attach_notifier(&self, notifier: Arc<Notifier>) {
        *self.notifier.write().expect("行情通知锁不可用") = Some(notifier);
    }

    /// 绑定资金费率采集器，用于读取各合约的结算间隔并归一费率。
    pub fn attach_funding(&self, funding: Arc<FundingFeed>) {
        *self.funding.write().expect("行情资金费率锁不可用") = Some(funding);
    }

    pub fn quote(&self, venue: &str, symbol: &str) -> Option<Quote> {
        if let Some(quote) = self
            .state
            .read()
            .expect("行情锁不可用")
            .books
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|book| book.quote.clone())
        {
            return Some(quote);
        }
        // 扫描清单之外的交易对（例如账户同步回来的持仓）只订阅了 WebSocket，
        // 这里用推送盘口兜底，否则平仓会误报“没有可用行情”。
        let update = self.realtime.as_ref()?.book(venue, symbol)?;
        let age_ms = Utc::now()
            .signed_duration_since(update.observed_at)
            .num_milliseconds();
        if age_ms < 0 || age_ms > REALTIME_PRICE_MAX_AGE_SECS * 1_000 {
            return None;
        }
        let (bid, ask) = (update.snapshot.bid, update.snapshot.ask);
        if bid <= Decimal::ZERO || ask <= Decimal::ZERO {
            return None;
        }
        Some(Quote {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            bid,
            ask,
            observed_at: update.observed_at,
        })
    }

    /// 读取 WebSocket 推送的最新盘口中间价，不受 REST 轮询节奏影响。
    ///
    /// 仓位与浮动盈亏用它实时定价；推送过期（断线）时返回 `None`，由调用方
    /// 保留交易所返回的标记价。
    pub fn realtime_price(&self, venue: &str, symbol: &str) -> Option<Decimal> {
        self.realtime_quote(venue, symbol).map(|(mid, _)| mid)
    }

    /// WebSocket 盘口的最新中间价与推送时间，供实时 K 线与前端价格推送使用。
    pub fn realtime_quote(&self, venue: &str, symbol: &str) -> Option<(Decimal, DateTime<Utc>)> {
        let update = self.realtime.as_ref()?.book(venue, symbol)?;
        let age_ms = Utc::now()
            .signed_duration_since(update.observed_at)
            .num_milliseconds();
        if age_ms < 0 || age_ms > REALTIME_PRICE_MAX_AGE_SECS * 1_000 {
            return None;
        }
        let mid = (update.snapshot.bid + update.snapshot.ask) / Decimal::from(2);
        (mid > Decimal::ZERO).then_some((mid, update.observed_at))
    }

    /// 把交易对加入 WebSocket 订阅，价格变化即可实时反映到仓位与图表。
    pub fn watch_symbols(&self, venue: &str, symbols: &[String]) {
        if let Some(feed) = &self.realtime {
            feed.book_symbols().extend_venue(venue, symbols.to_vec());
        }
    }

    /// 订阅某交易对的实时价格，并让采样器维护它的 30 秒 K 线。
    pub fn watch_tick_bars(&self, venue: &str, symbol: &str) {
        let Ok(symbol) = normalize_symbol(symbol) else {
            return;
        };
        let venue = venue.trim().to_ascii_lowercase();
        self.tick_bars.watch(&venue, &symbol);
        self.watch_symbols(&venue, &[symbol]);
    }

    /// 把持仓涉及的交易对加入 WebSocket 订阅，价格变化即可实时反映到仓位。
    pub fn watch_position_symbols(&self, venue: &str, symbols: &[String]) {
        self.watch_symbols(venue, symbols);
    }

    /// K 线，供桌面端图表与策略指标使用。
    ///
    /// `venue` 为 `binance-spot` / `okx-spot` 等标识；以 `-spot` 结尾走现货，
    /// 其余走永续合约。`30s` 由 WebSocket 盘口采样聚合，`10m` 由 5m 聚合。
    pub async fn candles(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let symbol = normalize_symbol(symbol)?;
        let interval = interval.trim().to_ascii_lowercase();
        let limit = limit.clamp(1, 1_000);
        let lower = venue.trim().to_ascii_lowercase();
        let segment = if lower.ends_with("-spot") {
            Segment::Spot
        } else {
            Segment::Swap
        };

        // 30 秒没有可用的 REST 历史，只能读取 WebSocket 采样聚合的结果。
        if interval == "30s" {
            self.watch_tick_bars(&lower, &symbol);
            return Ok(self.tick_bars.series(&lower, &symbol, limit as usize));
        }
        if !KLINE_INTERVALS.contains(&interval.as_str()) {
            bail!("不支持的 K 线周期: {interval}");
        }
        // 交易所普遍没有 10 分钟周期，用 5 分钟聚合。
        let (native, bucket_secs, aggregate) = if interval == "10m" {
            ("5m".to_string(), 600, true)
        } else {
            (interval.clone(), interval_seconds(&interval)?, false)
        };
        let fetch_limit = if aggregate {
            ((limit as i64) * bucket_secs / interval_seconds(&native)?).clamp(1, 1_000) as u32
        } else {
            limit
        };

        let raw = match lower.as_str() {
            SPOT_VENUE | VENUE => {
                let client = self
                    .binance
                    .clone()
                    .ok_or_else(|| anyhow!("Binance 行情客户端未初始化"))?;
                tokio::task::spawn_blocking(move || {
                    client.candles(&symbol, &native, fetch_limit, segment)
                })
                .await
                .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            OKX_VENUE | OKX_SPOT_VENUE => {
                let client = self
                    .okx
                    .clone()
                    .ok_or_else(|| anyhow!("OKX 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || {
                    client.candles(&symbol, &native, fetch_limit, segment)
                })
                .await
                .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            ASTER_VENUE => {
                let client = self
                    .aster
                    .clone()
                    .ok_or_else(|| anyhow!("Aster 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || {
                    client.candles(&symbol, &native, fetch_limit, Segment::Swap)
                })
                .await
                .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            BYBIT_VENUE => {
                let client = self
                    .bybit
                    .clone()
                    .ok_or_else(|| anyhow!("Bybit 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &native, fetch_limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            BITGET_VENUE => {
                let client = self
                    .bitget
                    .clone()
                    .ok_or_else(|| anyhow!("Bitget 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &native, fetch_limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            GATE_VENUE => {
                let client = self
                    .gate
                    .clone()
                    .ok_or_else(|| anyhow!("Gate 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &native, fetch_limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            HYPERLIQUID_VENUE => {
                let client = self
                    .hyperliquid
                    .clone()
                    .ok_or_else(|| anyhow!("Hyperliquid 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &native, fetch_limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            LIGHTER_VENUE => {
                let client = self
                    .lighter
                    .clone()
                    .ok_or_else(|| anyhow!("Lighter 行情客户端未启用"))?;
                tokio::task::spawn_blocking(move || client.candles(&symbol, &native, fetch_limit))
                    .await
                    .map_err(|error| anyhow!("K 线任务异常退出: {error}"))?
            }
            other => bail!("{other} 暂不支持 K 线"),
        }?;

        if aggregate {
            Ok(aggregate_candles(raw, bucket_secs, limit as usize))
        } else {
            Ok(raw)
        }
    }

    /// 多档盘口，供桌面端展示盘口深度。
    ///
    /// `venue` 规则与 [`MarketFeed::candles`] 一致：`-spot` 结尾走现货，其余走永续。
    pub async fn depth(
        &self,
        venue: &str,
        symbol: &str,
        limit: u32,
    ) -> anyhow::Result<DepthSnapshot> {
        let symbol = normalize_symbol(symbol)?;
        let limit = limit.clamp(1, 200);
        let requested = venue.trim().to_ascii_lowercase();
        let segment = if requested.ends_with("-spot") {
            Segment::Spot
        } else {
            Segment::Swap
        };
        let clients = BookClients {
            binance: self.binance.clone(),
            okx: self.okx.clone(),
            bybit: self.bybit.clone(),
            bitget: self.bitget.clone(),
            gate: self.gate.clone(),
            aster: self.aster.clone(),
            hyperliquid: self.hyperliquid.clone(),
            lighter: self.lighter.clone(),
            htx: self.htx.clone(),
            mexc: self.mexc.clone(),
            bingx: self.bingx.clone(),
        };
        let target = requested.clone();
        let mut snapshot = tokio::task::spawn_blocking(move || {
            clients.depth(&target, &symbol, segment, limit)
        })
        .await
        .map_err(|error| anyhow!("盘口深度任务异常退出: {error}"))??;
        // Aster 复用 Binance 客户端，这里统一改用请求方传入的 venue。
        snapshot.venue = requested;
        snapshot.bids.truncate(limit as usize);
        snapshot.asks.truncate(limit as usize);
        Ok(snapshot)
    }

    /// 某交易所某交易对的最小下单限制，供桌面端展示与校验。
    ///
    /// `venue` 规则与 [`MarketFeed::depth`] 一致：`-spot` 结尾走现货，其余走永续。
    pub async fn min_order(
        &self,
        venue: &str,
        symbol: &str,
    ) -> anyhow::Result<Option<MinOrderSpec>> {
        let symbol = normalize_symbol(symbol)?;
        let requested = venue.trim().to_ascii_lowercase();
        let segment = if requested.ends_with("-spot") {
            Segment::Spot
        } else {
            Segment::Swap
        };
        let clients = BookClients {
            binance: self.binance.clone(),
            okx: self.okx.clone(),
            bybit: self.bybit.clone(),
            bitget: self.bitget.clone(),
            gate: self.gate.clone(),
            aster: self.aster.clone(),
            hyperliquid: self.hyperliquid.clone(),
            lighter: self.lighter.clone(),
            htx: self.htx.clone(),
            mexc: self.mexc.clone(),
            bingx: self.bingx.clone(),
        };
        let target = requested.clone();
        tokio::task::spawn_blocking(move || clients.min_order(&target, &symbol, segment))
            .await
            .map_err(|error| anyhow!("最小下单限制任务异常退出: {error}"))?
    }

    /// 下单前向交易所 REST 取一次最新盘口。
    ///
    /// [`MarketFeed::quote`] 读取的是轮询缓存：采集间隔、WebSocket 推送稀疏
    /// （只在价格变化时推送）或采集清单之外的交易对，都会让缓存报价超过风控
    /// 允许的行情延迟。下单时用这里的实时报价做校验，避免把有效行情误判为过期。
    pub async fn refresh_quote(&self, venue: &str, symbol: &str) -> anyhow::Result<Quote> {
        let requested = venue.trim().to_ascii_lowercase();
        let symbol = symbol.trim().to_ascii_uppercase();
        let segment = if requested.ends_with("-spot") {
            Segment::Spot
        } else {
            Segment::Swap
        };
        let clients = BookClients {
            binance: self.binance.clone(),
            okx: self.okx.clone(),
            bybit: self.bybit.clone(),
            bitget: self.bitget.clone(),
            gate: self.gate.clone(),
            aster: self.aster.clone(),
            hyperliquid: self.hyperliquid.clone(),
            lighter: self.lighter.clone(),
            htx: self.htx.clone(),
            mexc: self.mexc.clone(),
            bingx: self.bingx.clone(),
        };
        let target = requested.clone();
        let symbol_for_client = symbol.clone();
        let (snapshot, observed_at) = tokio::task::spawn_blocking(move || {
            let snapshot = clients.book(&target, &symbol_for_client, segment)?;
            Ok::<_, anyhow::Error>((snapshot, Utc::now()))
        })
        .await
        .map_err(|error| anyhow!("盘口刷新任务异常退出: {error}"))??;
        if snapshot.bid <= Decimal::ZERO || snapshot.ask <= Decimal::ZERO {
            bail!("{symbol} 在 {requested} 的最新盘口无效");
        }
        Ok(Quote {
            venue: requested,
            symbol,
            bid: snapshot.bid,
            ask: snapshot.ask,
            observed_at,
        })
    }

    /// OKX 永续的合约规格（每张面值、最小张数、张数步长）。
    ///
    /// 其他交易所按基础币数量下单，返回 `None`；查询失败时同样返回 `None`，
    /// 由交易所下单接口做最终校验，避免因合约信息接口抖动阻断正常下单。
    pub async fn swap_contract_spec(&self, venue: &str, symbol: &str) -> Option<ContractSpec> {
        if !venue.trim().eq_ignore_ascii_case(OKX_VENUE) {
            return None;
        }
        let client = self.okx.clone()?;
        let symbol = symbol.trim().to_ascii_uppercase();
        let target = symbol.clone();
        match tokio::task::spawn_blocking(move || client.swap_contract_spec(&target)).await {
            Ok(Ok(spec)) => Some(spec),
            Ok(Err(error)) => {
                tracing::warn!(symbol = %symbol, %error, "查询 OKX 合约规格失败，跳过数量粒度校验");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "OKX 合约规格任务异常退出，跳过数量粒度校验");
                None
            }
        }
    }

    /// 下单数量与价格的步长约束。
    ///
    /// Binance / Aster / Bybit 永续要求数量是步长的整数倍、价格是价格步长的整数倍，
    /// 否则会以「精度超限」或「Qty invalid」拒单；Lighter 与 Hyperliquid 的签名接口
    /// 也会按各自精度取整，取整后可能掉到最小下单额以下。其他交易所返回 `None` 且
    /// 不下发请求。查询失败同样返回 `None`，由交易所下单接口做最终校验，避免因合约
    /// 信息接口抖动阻断正常下单。
    pub async fn order_step_spec(&self, venue: &str, symbol: &str) -> Option<MinOrderSpec> {
        let target = venue.trim().to_ascii_lowercase();
        if target != VENUE
            && target != ASTER_VENUE
            && target != BYBIT_VENUE
            && target != LIGHTER_VENUE
            && target != HYPERLIQUID_VENUE
        {
            return None;
        }
        match self.min_order(&target, symbol).await {
            Ok(spec) => spec,
            Err(error) => {
                tracing::warn!(
                    venue = %target,
                    symbol = %symbol,
                    %error,
                    "查询下单步长失败，跳过数量/价格精度对齐"
                );
                None
            }
        }
    }

    /// 交割合约清单：缓存一小时内有效，避免每轮行情都拉取各所合约信息。
    async fn delivery_contracts(&self, now: DateTime<Utc>) -> Vec<DeliveryContract> {
        let cached = {
            let cache = self.delivery.read().expect("交割合约锁不可用");
            cache
                .refreshed_at
                .filter(|at| now.signed_duration_since(*at).num_seconds() < DELIVERY_REFRESH_SECS)
                .map(|_| cache.contracts.clone())
        };
        if let Some(contracts) = cached {
            return contracts;
        }

        let symbols = self.symbols.clone();
        let binance = self.binance.clone();
        let okx = self.okx.clone();
        let bybit = self.bybit.clone();
        let fetched = tokio::task::spawn_blocking(move || {
            let mut contracts = Vec::new();
            if let Some(client) = binance {
                match client.delivery_contracts(&symbols) {
                    Ok(rows) => contracts.extend(rows),
                    Err(error) => tracing::warn!(%error, venue = "binance", "拉取交割合约清单失败"),
                }
            }
            if let Some(client) = okx {
                match client.delivery_contracts(&symbols) {
                    Ok(rows) => contracts.extend(rows),
                    Err(error) => tracing::warn!(%error, venue = "okx", "拉取交割合约清单失败"),
                }
            }
            if let Some(client) = bybit {
                match client.delivery_contracts(&symbols) {
                    Ok(rows) => contracts.extend(rows),
                    Err(error) => tracing::warn!(%error, venue = "bybit", "拉取交割合约清单失败"),
                }
            }
            contracts
        })
        .await;

        let contracts = match fetched {
            Ok(contracts) => contracts,
            Err(error) => {
                tracing::error!(%error, "交割合约清单任务异常退出");
                return Vec::new();
            }
        };

        let mut cache = self.delivery.write().expect("交割合约锁不可用");
        cache.contracts = contracts.clone();
        cache.refreshed_at = Some(now);
        contracts
    }

    async fn refresh(self: &Arc<Self>) {
        if self.perp_venues.is_empty() && self.realtime.is_none() {
            return;
        }

        let now = Utc::now();
        let pushed = self
            .realtime
            .as_ref()
            .map(|feed| feed.books())
            .unwrap_or_default();

        // 先取 WebSocket 快照，缺失或过期（例如刚启动、断线）的腿才走 REST。
        let mut latest: HashMap<(String, String), Book> = HashMap::new();
        let mut stale: Vec<(String, String, Segment)> = Vec::new();
        for symbol in &self.symbols {
            let mut legs: Vec<(&str, Segment)> = Vec::new();
            if self.binance.is_some() {
                legs.push((SPOT_VENUE, Segment::Spot));
            }
            if self.okx.is_some() {
                legs.push((OKX_SPOT_VENUE, Segment::Spot));
            }
            if self.bybit.is_some() {
                legs.push((BYBIT_SPOT_VENUE, Segment::Spot));
            }
            for &venue in &self.perp_venues {
                legs.push((venue, Segment::Swap));
            }

            for (venue, segment) in legs {
                let key = (venue.to_string(), symbol.clone());
                match pushed.get(&key) {
                    Some(update) if is_fresh(update.observed_at, now, self.ws_max_age) => {
                        latest.insert(
                            key,
                            Book {
                                quote: Quote {
                                    venue: venue.to_string(),
                                    symbol: symbol.clone(),
                                    bid: update.snapshot.bid,
                                    ask: update.snapshot.ask,
                                    observed_at: update.observed_at,
                                },
                                bid_notional_usd: update.snapshot.bid_notional_usd,
                                ask_notional_usd: update.snapshot.ask_notional_usd,
                            },
                        );
                    }
                    _ => stale.push((venue.to_string(), symbol.clone(), segment)),
                }
            }
        }

        // 交割合约清单低频刷新；盘口本身每轮都只能用 REST 拉取。
        let delivery = self.delivery_contracts(now).await;
        for contract in &delivery {
            stale.push((
                contract.venue.clone(),
                contract.contract.clone(),
                Segment::Delivery,
            ));
        }

        if !stale.is_empty() {
            let clients = BookClients {
                binance: self.binance.clone(),
                okx: self.okx.clone(),
                bybit: self.bybit.clone(),
                bitget: self.bitget.clone(),
                gate: self.gate.clone(),
                aster: self.aster.clone(),
                hyperliquid: self.hyperliquid.clone(),
                lighter: self.lighter.clone(),
                htx: self.htx.clone(),
                mexc: self.mexc.clone(),
                bingx: self.bingx.clone(),
            };
            let fetched = tokio::task::spawn_blocking(move || {
                let mut books = Vec::new();
                let mut errors = Vec::new();
                for (venue, symbol, segment) in stale {
                    match clients.book(&venue, &symbol, segment) {
                        Ok(snapshot) => books.push(Book {
                            quote: Quote {
                                venue,
                                symbol,
                                bid: snapshot.bid,
                                ask: snapshot.ask,
                                observed_at: Utc::now(),
                            },
                            bid_notional_usd: snapshot.bid_notional_usd,
                            ask_notional_usd: snapshot.ask_notional_usd,
                        }),
                        Err(error) => errors.push(format!("{venue} {symbol}: {error}")),
                    }
                }
                (books, errors)
            })
            .await;

            match fetched {
                Ok((books, errors)) => {
                    if !errors.is_empty() {
                        let message = errors.join("; ");
                        tracing::warn!(%message, "盘口 REST 兜底部分失败");
                    }
                    for book in books {
                        latest.insert((book.quote.venue.clone(), book.quote.symbol.clone()), book);
                    }
                }
                Err(error) => tracing::error!(%error, "盘口 REST 兜底任务异常退出"),
            }
        }

        let mut funding = Vec::new();
        for &venue in &self.perp_venues {
            match self.store.funding_latest(venue, &self.symbols).await {
                Ok(rows) => funding.extend(rows),
                Err(error) => tracing::warn!(%error, venue, "读取最新资金费率失败"),
            }
        }
        let intervals = self
            .funding
            .read()
            .expect("行情资金费率锁不可用")
            .as_ref()
            .map(|feed| feed.intervals())
            .unwrap_or_default();

        let mut opportunities = perp_perp_opportunities(
            &latest,
            &funding,
            &intervals,
            now,
            self.poll.as_secs(),
        );
        // 各所结算间隔不同（Hyperliquid 1h、多数 CEX 8h），只按年化收益排序。
        opportunities.sort_by(|a, b| {
            b.funding_annualized_pct
                .unwrap_or_default()
                .cmp(&a.funding_annualized_pct.unwrap_or_default())
        });
        // 交割基差单独成表，按年化基差从高到低排序。
        let basis = basis_opportunities(&latest, &delivery, now, self.poll.as_secs());

        // 先按通知设置推送资金费率机会，再把本轮结果写入行情快照。
        let notifier = self.notifier.read().expect("行情通知锁不可用").clone();
        if let Some(notifier) = notifier {
            // 推送走后台任务，避免 Telegram 请求拖慢行情刷新。
            let pending = opportunities.clone();
            tokio::spawn(async move { notifier.notify_funding(&pending).await });
        }

        {
            let mut state = self.state.write().expect("行情锁不可用");
            state.books = latest;
            state.opportunities = opportunities;
            state.basis = basis;
        }

        let count = self.opportunities().len();
        let _ = self.events.send(ExecutorEvent::OpportunityUpdated {
            count,
            observed_at: now,
        });
    }
}

/// WebSocket 快照是否仍在可用窗口内。
fn is_fresh(observed_at: DateTime<Utc>, now: DateTime<Utc>, max_age: Duration) -> bool {
    let age_ms = now.signed_duration_since(observed_at).num_milliseconds();
    age_ms >= 0 && age_ms <= max_age.as_millis() as i64
}

/// 把单期费率折算成 8 小时一期，仅用于 bps 字段展示；跨所比较请用 [`annualize`]。
fn normalize_funding(rate: Decimal, interval_secs: i64) -> Decimal {
    if interval_secs <= 0 || interval_secs == FUNDING_TARGET_INTERVAL_SECS {
        return rate;
    }
    rate * Decimal::from(FUNDING_TARGET_INTERVAL_SECS) / Decimal::from(interval_secs)
}

/// 现货多 + 交割空的基差机会：买入现货、卖出交割合约，持有到到期收敛。
///
/// 基差 = (交割买一 - 现货卖一) / 现货卖一，只在正向市场（交割升水）成立；
/// 年化 = 基差 / 剩余天数 × 365，便于与资金费率年化横向比较。
fn basis_opportunities(
    books: &HashMap<(String, String), Book>,
    contracts: &[DeliveryContract],
    now: DateTime<Utc>,
    poll_secs: u64,
) -> Vec<Opportunity> {
    let expires_at = now + ChronoDuration::seconds((poll_secs as i64 * 2).max(10));
    let fee_bps = Decimal::from(LEG_FEE_BPS * 2);
    let slippage_bps = Decimal::from(SLIPPAGE_BPS);
    let seconds_per_day = 86_400;
    let year_days = Decimal::from(365);

    let mut opportunities = Vec::with_capacity(contracts.len());
    for contract in contracts {
        let remaining_secs = contract.expiry.signed_duration_since(now).num_seconds();
        // 剩余不足一天的合约年化会失真，直接跳过。
        if remaining_secs <= seconds_per_day {
            continue;
        }
        let venue = spot_venue(&contract.venue);
        let Some(spot) = books.get(&(venue.clone(), contract.symbol.clone())) else {
            continue;
        };
        let Some(delivery) = books.get(&(contract.venue.clone(), contract.contract.clone())) else {
            continue;
        };
        if spot.quote.ask <= Decimal::ZERO {
            continue;
        }

        let basis_ratio = (delivery.quote.bid - spot.quote.ask) / spot.quote.ask;
        // 只有交割升水才对应「现货多 + 交割空」；贴水方向现货无法做空。
        if basis_ratio <= Decimal::ZERO {
            continue;
        }
        let gross_bps = (basis_ratio * Decimal::from(10_000)).round_dp(2);
        let days = Decimal::from(remaining_secs) / Decimal::from(seconds_per_day);
        let annualized_pct = (basis_ratio * year_days / days * Decimal::from(100)).round_dp(4);

        opportunities.push(Opportunity {
            id: Uuid::new_v4(),
            kind: OpportunityKind::DeliveryBasis,
            symbol: contract.symbol.clone(),
            long_venue: venue,
            short_venue: contract.venue.clone(),
            gross_spread_bps: gross_bps,
            estimated_fee_bps: fee_bps,
            estimated_slippage_bps: slippage_bps,
            net_edge_bps: gross_bps - fee_bps - slippage_bps,
            max_notional_usd: spot
                .ask_notional_usd
                .min(delivery.bid_notional_usd)
                .round_dp(0),
            funding_rate: None,
            funding_annualized_pct: None,
            delivery_contract: Some(contract.contract.clone()),
            expiry: Some(contract.expiry),
            annualized_basis_pct: Some(annualized_pct),
            observed_at: now,
            expires_at,
        });
    }

    opportunities.sort_by(|a, b| {
        b.annualized_basis_pct
            .unwrap_or_default()
            .cmp(&a.annualized_basis_pct.unwrap_or_default())
    });
    opportunities
}

/// 跨交易所「永续多 + 永续空」的资金费率差机会。
///
/// 同一交易对在两家交易所的费率不同时，做多低费率腿、做空高费率腿：
/// 空头收取的费率高于多头支付的费率，差额即为收益，且不需要现货库存。
///
/// 各所结算间隔可能不同（Hyperliquid 1 小时、多数 CEX 8 小时），
/// 因此每条腿先按自己的间隔折成年化，再按年化差排序与判断。
fn perp_perp_opportunities(
    books: &HashMap<(String, String), Book>,
    funding: &[FundingRate],
    intervals: &HashMap<(String, String), i64>,
    now: DateTime<Utc>,
    poll_secs: u64,
) -> Vec<Opportunity> {
    let expires_at = now + ChronoDuration::seconds((poll_secs as i64 * 2).max(10));
    let fee_bps = Decimal::from(LEG_FEE_BPS * 2);
    let slippage_bps = Decimal::from(SLIPPAGE_BPS);

    // 同一交易对在各交易所的当期费率与结算间隔。
    let mut by_symbol: HashMap<&str, Vec<(&str, Decimal, i64)>> = HashMap::new();
    for row in funding {
        let interval = intervals
            .get(&(row.venue.clone(), row.symbol.clone()))
            .copied()
            .unwrap_or(FUNDING_TARGET_INTERVAL_SECS);
        by_symbol.entry(row.symbol.as_str()).or_default().push((
            row.venue.as_str(),
            row.funding_rate,
            interval,
        ));
    }

    let mut opportunities = Vec::new();
    for (symbol, rates) in by_symbol {
        // 只有盘口在手的腿才能估算可成交额度。
        let mut quoted: Vec<(&str, Decimal, i64)> = rates
            .into_iter()
            .filter(|(venue, _, _)| books.contains_key(&((*venue).to_string(), symbol.to_string())))
            .collect();
        if quoted.len() < 2 {
            continue;
        }
        // 按年化排序：直接比单期费率会把 8 小时的 CEX 误判成比 1 小时的 DEX 更贵。
        quoted.sort_by(|left, right| annualize(left.1, left.2).cmp(&annualize(right.1, right.2)));
        let (long_venue, long_rate, long_interval) = quoted[0];
        let (short_venue, short_rate, short_interval) = quoted[quoted.len() - 1];
        let annualized_pct = (annualize(short_rate, short_interval)
            - annualize(long_rate, long_interval))
        .round_dp(4);
        if long_venue == short_venue || annualized_pct <= Decimal::ZERO {
            continue;
        }
        let gross_bps = ((normalize_funding(short_rate, short_interval)
            - normalize_funding(long_rate, long_interval))
            * Decimal::from(10_000))
        .round_dp(2);

        let long_book = &books[&(long_venue.to_string(), symbol.to_string())];
        let short_book = &books[&(short_venue.to_string(), symbol.to_string())];
        opportunities.push(Opportunity {
            id: Uuid::new_v4(),
            kind: OpportunityKind::PerpPerpFunding,
            symbol: symbol.to_string(),
            long_venue: long_venue.to_string(),
            short_venue: short_venue.to_string(),
            gross_spread_bps: gross_bps,
            estimated_fee_bps: fee_bps,
            estimated_slippage_bps: slippage_bps,
            net_edge_bps: gross_bps - fee_bps - slippage_bps,
            max_notional_usd: long_book
                .ask_notional_usd
                .min(short_book.bid_notional_usd)
                .round_dp(0),
            // 两腿结算周期可能不同，单期费率无法直接相减；收益以年化字段为准。
            funding_rate: None,
            funding_annualized_pct: Some(annualized_pct),
            delivery_contract: None,
            expiry: None,
            annualized_basis_pct: None,
            observed_at: now,
            expires_at,
        });
    }

    opportunities
}

/// 永续 venue → 同交易所的现货 venue（`binance` → `binance-spot`）。
fn spot_venue(venue: &str) -> String {
    format!("{venue}-spot")
}

fn normalize_symbol(symbol: &str) -> anyhow::Result<String> {
    let symbol = symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() || !symbol.chars().all(|char| char.is_ascii_alphanumeric()) {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(symbol)
}

/// K 线周期对应的秒数，仅用于聚合计算。
fn interval_seconds(interval: &str) -> anyhow::Result<i64> {
    Ok(match interval {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1_800,
        "1h" => 3_600,
        "4h" => 14_400,
        "1d" => 86_400,
        "1w" => 604_800,
        other => bail!("不支持的 K 线周期: {other}"),
    })
}

/// 把更细粒度的 K 线按周期边界合并成目标周期（如 5m → 10m）。
fn aggregate_candles(candles: Vec<Candle>, bucket_secs: i64, limit: usize) -> Vec<Candle> {
    if bucket_secs <= 0 {
        return candles;
    }
    let mut out: Vec<Candle> = Vec::with_capacity(candles.len());
    for candle in candles {
        let ts = candle.open_time.timestamp();
        let bucket = ts - ts.rem_euclid(bucket_secs);
        match out.last_mut() {
            Some(last) if last.open_time.timestamp() == bucket => {
                last.high = last.high.max(candle.high);
                last.low = last.low.min(candle.low);
                last.close = candle.close;
                last.volume += candle.volume;
            }
            _ => {
                let Some(open_time) = DateTime::from_timestamp(bucket, 0) else {
                    continue;
                };
                out.push(Candle {
                    open_time,
                    ..candle
                });
            }
        }
    }
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn book(venue: &str, bid: Decimal, ask: Decimal) -> Book {
        Book {
            quote: Quote {
                venue: venue.to_string(),
                symbol: "BTCUSDT".to_string(),
                bid,
                ask,
                observed_at: Utc::now(),
            },
            bid_notional_usd: bid * dec!(2),
            ask_notional_usd: ask * dec!(3),
        }
    }

    fn funding(venue: &str, rate: Decimal) -> FundingRate {
        FundingRate {
            venue: venue.to_string(),
            symbol: "BTCUSDT".to_string(),
            funding_time: Utc::now(),
            funding_rate: rate,
            mark_price: None,
        }
    }

    #[test]
    fn rejects_invalid_symbols() {
        assert!(normalize_symbol("BTCUSDT").is_ok());
        assert!(normalize_symbol("btcusdt").is_ok());
        assert!(normalize_symbol("").is_err());
        assert!(normalize_symbol("BTC-USDT").is_err());
    }

    fn perp_book(venue: &str, bid: Decimal, ask: Decimal) -> ((String, String), Book) {
        (
            (venue.to_string(), "BTCUSDT".to_string()),
            book(venue, bid, ask),
        )
    }

    #[test]
    fn builds_perp_perp_opportunity_from_low_and_high_rate() {
        let books: HashMap<_, _> = [
            perp_book("binance", dec!(100), dec!(101)),
            perp_book("bybit", dec!(102), dec!(103)),
        ]
        .into_iter()
        .collect();

        let funding = [
            funding("binance", dec!(0.0001)),
            funding("bybit", dec!(0.0003)),
        ];
        let opportunities =
            perp_perp_opportunities(&books, &funding, &HashMap::new(), Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        let opportunity = &opportunities[0];
        assert_eq!(opportunity.kind, OpportunityKind::PerpPerpFunding);
        // 低费率腿做多，高费率腿做空。
        assert_eq!(opportunity.long_venue, "binance");
        assert_eq!(opportunity.short_venue, "bybit");
        assert_eq!(opportunity.gross_spread_bps, dec!(2));
        assert_eq!(opportunity.net_edge_bps, dec!(-7));
        // 多头卖一 101*3 与空头买一 102*2 取较小者。
        assert_eq!(opportunity.max_notional_usd, dec!(204));
        // 两腿结算周期不同，单期费率无法直接相减，收益只看年化（0.02% / 8h → 21.9%/年）。
        assert_eq!(opportunity.funding_rate, None);
        assert_eq!(opportunity.funding_annualized_pct, Some(dec!(21.9)));
    }

    #[test]
    fn normalizes_funding_to_eight_hours() {
        let books: HashMap<_, _> = [
            perp_book("binance", dec!(100), dec!(101)),
            perp_book("bybit", dec!(100), dec!(101)),
        ]
        .into_iter()
        .collect();
        // Bybit 每 4 小时结算 0.015%，折合 8 小时 0.03%。
        let funding = [
            funding("binance", dec!(0.0001)),
            funding("bybit", dec!(0.00015)),
        ];
        let intervals = HashMap::from([(("bybit".to_string(), "BTCUSDT".to_string()), 4 * 3600)]);

        let opportunities = perp_perp_opportunities(&books, &funding, &intervals, Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        assert_eq!(opportunities[0].gross_spread_bps, dec!(2));
        // 年化：10.95% 与 32.85%，差额 21.9%。
        assert_eq!(opportunities[0].funding_annualized_pct, Some(dec!(21.9)));
    }

    #[test]
    fn ranks_perp_funding_by_annualized_rate_across_intervals() {
        let books: HashMap<_, _> = [
            perp_book("binance", dec!(100), dec!(101)),
            perp_book("hyperliquid", dec!(100), dec!(101)),
        ]
        .into_iter()
        .collect();
        // 单期看 CEX 更高（0.002% / 8h > 0.00125% / 1h），
        // 年化后 Hyperliquid 更高（10.95% > 2.19%），因此应做空 Hyperliquid。
        let funding = [
            funding("binance", dec!(0.00002)),
            funding("hyperliquid", dec!(0.0000125)),
        ];
        let intervals = HashMap::from([
            (("binance".to_string(), "BTCUSDT".to_string()), 8 * 3600),
            (("hyperliquid".to_string(), "BTCUSDT".to_string()), 3600),
        ]);

        let opportunities = perp_perp_opportunities(&books, &funding, &intervals, Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        let opportunity = &opportunities[0];
        assert_eq!(opportunity.long_venue, "binance");
        assert_eq!(opportunity.short_venue, "hyperliquid");
        assert_eq!(opportunity.funding_annualized_pct, Some(dec!(8.76)));
    }

    #[test]
    fn perp_perp_requires_two_quoted_venues() {
        let books: HashMap<_, _> = [perp_book("binance", dec!(100), dec!(101))]
            .into_iter()
            .collect();
        let funding = [
            funding("binance", dec!(0.0001)),
            funding("bybit", dec!(0.0003)),
        ];

        assert!(
            perp_perp_opportunities(&books, &funding, &HashMap::new(), Utc::now(), 60).is_empty()
        );
    }

    #[test]
    fn perp_perp_skips_equal_rates() {
        let books: HashMap<_, _> = [
            perp_book("binance", dec!(100), dec!(101)),
            perp_book("bybit", dec!(100), dec!(101)),
        ]
        .into_iter()
        .collect();
        let funding = [
            funding("binance", dec!(0.0001)),
            funding("bybit", dec!(0.0001)),
        ];

        assert!(
            perp_perp_opportunities(&books, &funding, &HashMap::new(), Utc::now(), 60).is_empty()
        );
    }

    fn delivery_contract(expiry_days: i64) -> DeliveryContract {
        DeliveryContract {
            venue: VENUE.to_string(),
            symbol: "BTCUSDT".to_string(),
            contract: "BTCUSDT_251226".to_string(),
            expiry: Utc::now() + ChronoDuration::days(expiry_days),
        }
    }

    fn basis_books(spot_ask: Decimal, delivery_bid: Decimal) -> HashMap<(String, String), Book> {
        [
            (
                (SPOT_VENUE.to_string(), "BTCUSDT".to_string()),
                book(SPOT_VENUE, spot_ask - dec!(1), spot_ask),
            ),
            (
                (VENUE.to_string(), "BTCUSDT_251226".to_string()),
                book(VENUE, delivery_bid, delivery_bid + dec!(1)),
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn builds_delivery_basis_opportunity() {
        // 现货卖一 101，交割买一 103：基差 2/101 ≈ 198.02 bps，剩余 365 天 → 年化 1.98%。
        let books = basis_books(dec!(101), dec!(103));
        let opportunities = basis_opportunities(&books, &[delivery_contract(365)], Utc::now(), 60);

        assert_eq!(opportunities.len(), 1);
        let opportunity = &opportunities[0];
        assert_eq!(opportunity.kind, OpportunityKind::DeliveryBasis);
        assert_eq!(opportunity.symbol, "BTCUSDT");
        assert_eq!(opportunity.long_venue, SPOT_VENUE);
        assert_eq!(opportunity.short_venue, VENUE);
        assert_eq!(
            opportunity.delivery_contract.as_deref(),
            Some("BTCUSDT_251226")
        );
        assert_eq!(opportunity.gross_spread_bps, dec!(198.02));
        assert_eq!(opportunity.net_edge_bps, dec!(189.02));
        assert_eq!(opportunity.annualized_basis_pct, Some(dec!(1.9802)));
        // 现货卖一 101*3 与交割买一 103*2 取较小者。
        assert_eq!(opportunity.max_notional_usd, dec!(206));
    }

    #[test]
    fn skips_backwardated_delivery() {
        // 交割买一低于现货卖一，现货腿无法做空，不构成基差机会。
        let books = basis_books(dec!(101), dec!(100));
        assert!(basis_opportunities(&books, &[delivery_contract(365)], Utc::now(), 60).is_empty());
    }

    #[test]
    fn skips_delivery_near_expiry() {
        let books = basis_books(dec!(101), dec!(103));
        assert!(basis_opportunities(&books, &[delivery_contract(0)], Utc::now(), 60).is_empty());
    }
}
