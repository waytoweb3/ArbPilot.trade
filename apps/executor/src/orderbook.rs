//! 增量 L2 盘口与逐笔成交的本地维护逻辑。
//!
//! 各交易所的增量盘口协议细节不同（序列号字段、快照获取方式），这里只负责
//! 与交易所无关的部分：档位合并、序列连续性判断、滑点 / 冲击成本估算，
//! 以及基于盘口与手续费的 maker-first 可行性判断。

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::exchange::DepthLevel;

/// 每个交易对保留的最大档位数（买卖各一半由各所推送决定）。
pub const L2_MAX_LEVELS: usize = 500;
/// 逐笔成交的内存上限。
pub const TRADE_BUFFER_CAPACITY: usize = 500;

/// 主动方向：买方吃单还是卖方吃单。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// 一笔逐笔成交。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTick {
    pub price: Decimal,
    pub quantity: Decimal,
    /// 主动方向（taker 方向）。
    pub side: Side,
    pub at: DateTime<Utc>,
}

impl TradeTick {
    pub fn notional_usd(&self) -> Decimal {
        self.price * self.quantity
    }
}

/// 某交易所某交易对的 L2 盘口快照。
#[derive(Debug, Clone)]
pub struct L2Book {
    pub venue: String,
    pub symbol: String,
    /// 买盘，价格由高到低。
    pub bids: Vec<DepthLevel>,
    /// 卖盘，价格由低到高。
    pub asks: Vec<DepthLevel>,
    /// 最近一次增量更新的序列号（各所语义不同，仅用于连续性判断）。
    pub last_update_id: Option<u64>,
    pub observed_at: DateTime<Utc>,
}

impl L2Book {
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first().map(|level| level.price)
    }

    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first().map(|level| level.price)
    }

    pub fn mid(&self) -> Option<Decimal> {
        Some((self.best_bid()? + self.best_ask()?) / Decimal::from(2))
    }

    /// 买卖价差（bps），以中间价为基准。
    pub fn spread_bps(&self) -> Option<Decimal> {
        let mid = self.mid()?;
        if mid.is_zero() {
            return None;
        }
        Some((self.best_ask()? - self.best_bid()?) / mid * Decimal::from(10_000))
    }
}

/// 按价格聚合的本地盘口状态，用于应用增量更新。
#[derive(Debug, Clone, Default)]
pub struct LevelBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    pub last_update_id: Option<u64>,
}

impl LevelBook {
    /// 用一份全量快照初始化盘口。
    pub fn from_snapshot(
        bids: impl IntoIterator<Item = (Decimal, Decimal)>,
        asks: impl IntoIterator<Item = (Decimal, Decimal)>,
    ) -> Self {
        let mut book = Self::default();
        book.replace(bids, asks);
        book
    }

    /// 用全量快照覆盖盘口。
    pub fn replace(
        &mut self,
        bids: impl IntoIterator<Item = (Decimal, Decimal)>,
        asks: impl IntoIterator<Item = (Decimal, Decimal)>,
    ) {
        self.bids.clear();
        self.asks.clear();
        for (price, quantity) in bids {
            if quantity > Decimal::ZERO {
                self.bids.insert(price, quantity);
            }
        }
        for (price, quantity) in asks {
            if quantity > Decimal::ZERO {
                self.asks.insert(price, quantity);
            }
        }
    }

    /// 应用一次增量：数量为 0 表示删除该档，其余为绝对数量。
    pub fn apply(
        &mut self,
        bids: impl IntoIterator<Item = (Decimal, Decimal)>,
        asks: impl IntoIterator<Item = (Decimal, Decimal)>,
    ) {
        for (price, quantity) in bids {
            if quantity.is_zero() {
                self.bids.remove(&price);
            } else {
                self.bids.insert(price, quantity);
            }
        }
        for (price, quantity) in asks {
            if quantity.is_zero() {
                self.asks.remove(&price);
            } else {
                self.asks.insert(price, quantity);
            }
        }
    }

    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.keys().next().copied()
    }

    /// 导出前 `n` 档：买盘由高到低，卖盘由低到高。
    pub fn top(&self, n: usize) -> (Vec<DepthLevel>, Vec<DepthLevel>) {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(n)
            .map(|(price, quantity)| DepthLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();
        let asks = self
            .asks
            .iter()
            .take(n)
            .map(|(price, quantity)| DepthLevel {
                price: *price,
                quantity: *quantity,
            })
            .collect();
        (bids, asks)
    }
}

/// 增量序列检查结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceCheck {
    /// 可以应用到当前盘口。
    Apply,
    /// 已经处理过，直接丢弃。
    Duplicate,
    /// 中间漏包，需要重新拉取快照。
    Gap,
}

/// Binance 风格的序列校验：`first` 是首条更新号，`last` 是末条更新号。
///
/// - `last <= prev`：重复消息；
/// - `first <= prev + 1 <= last`：可接续；
/// - 其它：漏包。
pub fn check_range(prev: Option<u64>, first: u64, last: u64) -> SequenceCheck {
    match prev {
        None => SequenceCheck::Gap,
        Some(prev) if last <= prev => SequenceCheck::Duplicate,
        Some(prev) if first <= prev.saturating_add(1) && prev.saturating_add(1) <= last => {
            SequenceCheck::Apply
        }
        Some(_) => SequenceCheck::Gap,
    }
}

/// `prev` 风格的序列校验：下一条序列号必须恰好是 `prev + 1`。
pub fn check_next(prev: Option<u64>, next: u64) -> SequenceCheck {
    match prev {
        None => SequenceCheck::Gap,
        Some(prev) if next == prev.saturating_add(1) => SequenceCheck::Apply,
        Some(prev) if next <= prev => SequenceCheck::Duplicate,
        Some(_) => SequenceCheck::Gap,
    }
}

/// 逐笔成交环形缓冲。
#[derive(Debug, Default)]
pub struct TradeBuffer {
    trades: VecDeque<TradeTick>,
}

impl TradeBuffer {
    pub fn push(&mut self, trade: TradeTick) {
        self.trades.push_back(trade);
        while self.trades.len() > TRADE_BUFFER_CAPACITY {
            self.trades.pop_front();
        }
    }

    pub fn latest(&self, limit: usize) -> Vec<TradeTick> {
        let start = self.trades.len().saturating_sub(limit);
        self.trades.iter().skip(start).cloned().collect()
    }

    /// 最近一段时间的买卖主动成交额（USDT）。
    pub fn flow_usd(&self, window: chrono::Duration) -> (Decimal, Decimal) {
        let cutoff = Utc::now() - window;
        let mut buy = Decimal::ZERO;
        let mut sell = Decimal::ZERO;
        for trade in self.trades.iter().rev() {
            if trade.at < cutoff {
                break;
            }
            match trade.side {
                Side::Buy => buy += trade.notional_usd(),
                Side::Sell => sell += trade.notional_usd(),
            }
        }
        (buy, sell)
    }
}

/// 按目标名义额吃单的估算结果。
#[derive(Debug, Clone, Serialize)]
pub struct SlippageEstimate {
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    /// 目标成交名义额（USDT）。
    pub target_notional_usd: Decimal,
    /// 盘口实际可成交名义额（USDT），深度不足时小于目标。
    pub filled_notional_usd: Decimal,
    /// 成交量（基础币）。
    pub filled_quantity: Decimal,
    /// 加权平均成交价。
    pub average_price: Decimal,
    /// 最差一档成交价。
    pub worst_price: Decimal,
    /// 消耗的档位数。
    pub levels_consumed: usize,
    /// 是否吃满了目标名义额。
    pub filled: bool,
    /// 相对对手方最优价的滑点（bps）。
    pub slippage_bps: Decimal,
    /// 相对中间价的冲击成本（bps）。
    pub impact_bps: Decimal,
}

/// 按目标名义额估算吃单滑点与冲击成本。
///
/// 买入遍历卖盘、卖出遍历买盘；深度不足时返回部分成交的估算，`filled = false`。
pub fn estimate_slippage(
    book: &L2Book,
    side: Side,
    target_notional_usd: Decimal,
) -> Option<SlippageEstimate> {
    if target_notional_usd <= Decimal::ZERO {
        return None;
    }
    let levels = match side {
        // 买入吃卖盘，价格由低到高。
        Side::Buy => &book.asks,
        // 卖出吃买盘，价格由高到低。
        Side::Sell => &book.bids,
    };
    let reference = match side {
        Side::Buy => book.best_ask()?,
        Side::Sell => book.best_bid()?,
    };
    if reference.is_zero() {
        return None;
    }

    let mut remaining = target_notional_usd;
    let mut filled_notional = Decimal::ZERO;
    let mut filled_quantity = Decimal::ZERO;
    let mut worst_price = reference;
    let mut levels_consumed = 0usize;
    for level in levels {
        if remaining <= Decimal::ZERO {
            break;
        }
        if level.price <= Decimal::ZERO || level.quantity <= Decimal::ZERO {
            continue;
        }
        let level_notional = level.price * level.quantity;
        let take_notional = level_notional.min(remaining);
        let take_quantity = take_notional / level.price;
        filled_notional += take_notional;
        filled_quantity += take_quantity;
        worst_price = level.price;
        levels_consumed += 1;
        remaining -= take_notional;
    }
    if filled_quantity.is_zero() {
        return None;
    }
    let average_price = filled_notional / filled_quantity;
    let slippage_bps = (average_price - reference) / reference * Decimal::from(10_000);
    let impact_bps = match book.mid() {
        Some(mid) if !mid.is_zero() => {
            let signed = average_price - mid;
            let signed = match side {
                Side::Buy => signed,
                Side::Sell => -signed,
            };
            signed / mid * Decimal::from(10_000)
        }
        _ => slippage_bps,
    };
    Some(SlippageEstimate {
        venue: book.venue.clone(),
        symbol: book.symbol.clone(),
        side,
        target_notional_usd,
        filled_notional_usd: filled_notional,
        filled_quantity,
        average_price,
        worst_price,
        levels_consumed,
        filled: remaining <= Decimal::ZERO,
        slippage_bps,
        impact_bps,
    })
}

/// maker-first 可行性判断。
#[derive(Debug, Clone, Serialize)]
pub struct MakerFirstAssessment {
    pub venue: String,
    pub symbol: String,
    pub spread_bps: Decimal,
    /// 以中间价挂单相对吃单节省的成本（bps）：价差一半 + 手续费差。
    pub maker_edge_bps: Decimal,
    /// 买一/卖一档的名义额，代表排队深度。
    pub queue_ahead_usd: Decimal,
    /// 是否值得优先挂单（等待成交）而不是直接吃单。
    pub viable: bool,
    pub reason: &'static str,
}

/// 判断是否值得 maker-first：只有挂单相对吃单的净收益为正、且盘口不是空的才可行。
///
/// `taker_fee_bps` / `maker_fee_bps` 为正值表示手续费成本，负值表示返佣；
/// `adverse_buffer_bps` 是预留的逆向选择缓冲（成交前价格恶化）。
pub fn assess_maker_first(
    book: &L2Book,
    taker_fee_bps: Decimal,
    maker_fee_bps: Decimal,
    adverse_buffer_bps: Decimal,
) -> Option<MakerFirstAssessment> {
    let spread_bps = book.spread_bps()?;
    let best_bid_level = book.bids.first()?;
    let best_ask_level = book.asks.first()?;
    let queue_ahead_usd = (best_bid_level.price * best_bid_level.quantity)
        .min(best_ask_level.price * best_ask_level.quantity);
    // 挂单赚取一半价差，并省下 taker 与 maker 的手续费差。
    let maker_edge_bps = spread_bps / Decimal::from(2) + taker_fee_bps - maker_fee_bps
        - adverse_buffer_bps;
    let viable = maker_edge_bps > Decimal::ZERO && queue_ahead_usd > Decimal::ZERO;
    Some(MakerFirstAssessment {
        venue: book.venue.clone(),
        symbol: book.symbol.clone(),
        spread_bps,
        maker_edge_bps,
        queue_ahead_usd,
        viable,
        reason: if viable {
            "挂单净收益为正"
        } else if queue_ahead_usd <= Decimal::ZERO {
            "盘口无有效挂单量"
        } else {
            "价差不足以覆盖手续费与逆向选择"
        },
    })
}

/// 计算一组成交的主动买卖失衡度，范围 `[-1, 1]`。
pub fn trade_imbalance(trades: &[TradeTick]) -> Option<Decimal> {
    if trades.is_empty() {
        return None;
    }
    let mut buy = Decimal::ZERO;
    let mut sell = Decimal::ZERO;
    for trade in trades {
        match trade.side {
            Side::Buy => buy += trade.notional_usd(),
            Side::Sell => sell += trade.notional_usd(),
        }
    }
    let total = buy + sell;
    if total.is_zero() {
        return None;
    }
    Some((buy - sell) / total)
}
