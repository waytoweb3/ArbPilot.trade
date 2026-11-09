use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::OrderIntent;

/// 执行器级风控限额。所有订单（桌面 / Telegram / 策略）共用同一份限制。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskLimits {
    /// 单笔订单总名义价值上限（双腿合计）。
    pub max_order_notional_usd: Decimal,
    /// 账户总敞口上限。
    pub max_total_notional_usd: Decimal,
    pub max_leverage: Decimal,
    pub max_slippage_bps: Decimal,
    /// 行情允许的最大延迟（秒）。
    pub max_quote_age_secs: i64,
    /// 空列表表示不限制。
    pub allowed_symbols: Vec<String>,
    pub allowed_venues: Vec<String>,
    pub max_legs: usize,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_notional_usd: Decimal::from(5_000),
            max_total_notional_usd: Decimal::from(25_000),
            max_leverage: Decimal::from(3),
            max_slippage_bps: Decimal::from(25),
            max_quote_age_secs: 5,
            allowed_symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            allowed_venues: vec![
                "binance".to_string(),
                "binance-spot".to_string(),
                "okx".to_string(),
                "okx-spot".to_string(),
                "bybit".to_string(),
                "bybit-spot".to_string(),
                "bitget".to_string(),
                "gate".to_string(),
                "aster".to_string(),
                "hyperliquid".to_string(),
                "lighter".to_string(),
                "htx".to_string(),
                "mexc".to_string(),
                "bingx".to_string(),
            ],
            max_legs: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskViolationCode {
    ExecutorPaused,
    LegCountUnsupported,
    DuplicateLeg,
    InvalidLeg,
    VenueNotAllowed,
    SymbolNotAllowed,
    NotionalTooLarge,
    TotalNotionalTooLarge,
    LeverageTooHigh,
    SlippageTooHigh,
    QuoteStale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskViolation {
    pub code: RiskViolationCode,
    pub message: String,
}

impl RiskViolation {
    fn new(code: RiskViolationCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RiskDecision {
    Approved { estimated_notional_usd: Decimal },
    Rejected { violations: Vec<RiskViolation> },
}

impl RiskDecision {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }
}

/// 风控检查所需的运行时快照，由执行器在每次下单前重新获取。
pub struct RiskContext {
    pub now: DateTime<Utc>,
    pub limits: RiskLimits,
    /// 当前账户已有敞口。
    pub open_notional_usd: Decimal,
    /// 执行器是否处于暂停开仓状态（如行情异常、连续失败、用户手动暂停）。
    pub paused: bool,
}

pub struct RiskEngine;

impl RiskEngine {
    /// 下单前风控。纯函数，便于单元测试与回放。
    pub fn preflight(intent: &OrderIntent, context: &RiskContext) -> RiskDecision {
        let limits = &context.limits;
        let mut violations = Vec::new();

        // 全部为只减仓的订单（平仓）只降低敞口：暂停、杠杆、名义限额与
        // 交易范围都不应阻止离场，否则用户可能无法退出已有风险。
        let closing = !intent.legs.is_empty() && intent.legs.iter().all(|leg| leg.reduce_only);

        if context.paused && !closing {
            violations.push(RiskViolation::new(
                RiskViolationCode::ExecutorPaused,
                "执行器已暂停新开仓",
            ));
        }

        if intent.legs.is_empty() || intent.legs.len() > limits.max_legs {
            violations.push(RiskViolation::new(
                RiskViolationCode::LegCountUnsupported,
                format!(
                    "订单腿数 {} 不在支持范围 1..={}",
                    intent.legs.len(),
                    limits.max_legs
                ),
            ));
        }

        let mut seen = std::collections::HashSet::new();
        for leg in &intent.legs {
            if !seen.insert((leg.venue.clone(), leg.symbol.clone(), leg.side)) {
                violations.push(RiskViolation::new(
                    RiskViolationCode::DuplicateLeg,
                    format!("重复的订单腿: {} {} {:?}", leg.venue, leg.symbol, leg.side),
                ));
            }
            if leg.quantity <= Decimal::ZERO || leg.limit_price <= Decimal::ZERO {
                violations.push(RiskViolation::new(
                    RiskViolationCode::InvalidLeg,
                    format!("订单腿数量或价格无效: {} {}", leg.venue, leg.symbol),
                ));
            }
            if !closing
                && !limits.allowed_venues.is_empty()
                && !limits.allowed_venues.contains(&leg.venue)
            {
                violations.push(RiskViolation::new(
                    RiskViolationCode::VenueNotAllowed,
                    format!("交易所 {} 不在允许列表内", leg.venue),
                ));
            }
            if !closing
                && !limits.allowed_symbols.is_empty()
                && !limits.allowed_symbols.contains(&leg.symbol)
            {
                violations.push(RiskViolation::new(
                    RiskViolationCode::SymbolNotAllowed,
                    format!("交易对 {} 不在允许列表内", leg.symbol),
                ));
            }
        }

        if !closing && intent.leverage > limits.max_leverage {
            violations.push(RiskViolation::new(
                RiskViolationCode::LeverageTooHigh,
                format!("杠杆 {} 超过上限 {}", intent.leverage, limits.max_leverage),
            ));
        }

        if intent.max_slippage_bps > limits.max_slippage_bps {
            violations.push(RiskViolation::new(
                RiskViolationCode::SlippageTooHigh,
                format!(
                    "最大滑点 {} bps 超过上限 {} bps",
                    intent.max_slippage_bps, limits.max_slippage_bps
                ),
            ));
        }

        let quote_age = context.now.signed_duration_since(intent.quote_observed_at);
        if quote_age.num_seconds() > limits.max_quote_age_secs || quote_age.num_seconds() < 0 {
            violations.push(RiskViolation::new(
                RiskViolationCode::QuoteStale,
                format!(
                    "行情已过期：观测延迟 {} 秒，上限 {} 秒",
                    quote_age.num_seconds(),
                    limits.max_quote_age_secs
                ),
            ));
        }

        let notional = intent.notional_usd();
        if !closing {
            if notional > limits.max_order_notional_usd {
                violations.push(RiskViolation::new(
                    RiskViolationCode::NotionalTooLarge,
                    format!(
                        "单笔名义价值 {} 超过上限 {}",
                        notional, limits.max_order_notional_usd
                    ),
                ));
            }
            if context.open_notional_usd + notional > limits.max_total_notional_usd {
                violations.push(RiskViolation::new(
                    RiskViolationCode::TotalNotionalTooLarge,
                    format!(
                        "总敞口 {} 超过上限 {}",
                        context.open_notional_usd + notional,
                        limits.max_total_notional_usd
                    ),
                ));
            }
        }

        if violations.is_empty() {
            RiskDecision::Approved {
                estimated_notional_usd: notional,
            }
        } else {
            RiskDecision::Rejected { violations }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderLegIntent, OrderOrigin, Side};
    use rust_decimal_macros::dec;

    fn intent(now: DateTime<Utc>) -> OrderIntent {
        OrderIntent {
            idempotency_key: "test-key".into(),
            origin: OrderOrigin::Desktop,
            strategy_id: None,
            legs: vec![
                OrderLegIntent {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    side: Side::Buy,
                    quantity: dec!(0.01),
                    limit_price: dec!(60000),
                    reduce_only: false,
                },
                OrderLegIntent {
                    venue: "okx".into(),
                    symbol: "BTCUSDT".into(),
                    side: Side::Sell,
                    quantity: dec!(0.01),
                    limit_price: dec!(60100),
                    reduce_only: false,
                },
            ],
            leverage: dec!(1),
            max_slippage_bps: dec!(10),
            quote_observed_at: now,
            created_at: now,
        }
    }

    fn context(now: DateTime<Utc>, paused: bool) -> RiskContext {
        RiskContext {
            now,
            limits: RiskLimits::default(),
            open_notional_usd: Decimal::ZERO,
            paused,
        }
    }

    #[test]
    fn approves_order_within_limits() {
        let now = Utc::now();
        let decision = RiskEngine::preflight(&intent(now), &context(now, false));
        assert!(decision.is_approved());
    }

    #[test]
    fn rejects_when_paused() {
        let now = Utc::now();
        let decision = RiskEngine::preflight(&intent(now), &context(now, true));
        assert!(!decision.is_approved());
    }

    #[test]
    fn rejects_stale_quote() {
        let now = Utc::now();
        let stale = now - chrono::Duration::seconds(30);
        let decision = RiskEngine::preflight(&intent(stale), &context(now, false));
        assert!(!decision.is_approved());
    }
}
