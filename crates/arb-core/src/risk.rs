use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::OrderIntent;

/// 单个交易所的可选覆盖。未填写的字段沿用全局默认。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VenueRiskLimits {
    /// 该交易所单腿名义价值上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_order_notional_usd: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_leverage: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slippage_bps: Option<Decimal>,
    /// 覆盖该交易所的交易对白名单；空列表表示不限制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_symbols: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_legs: Option<usize>,
}

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
    /// 按交易所覆盖；键为小写交易所标识。未设置的字段沿用全局默认，
    /// 数值型覆盖只会进一步收紧上限。
    #[serde(default)]
    pub venue_overrides: BTreeMap<String, VenueRiskLimits>,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_notional_usd: Decimal::from(5_000),
            max_total_notional_usd: Decimal::from(25_000),
            max_leverage: Decimal::from(10),
            max_slippage_bps: Decimal::from(25),
            max_quote_age_secs: 5,
            // 默认不限制交易对，由各交易所实际支持的合约决定。
            allowed_symbols: Vec::new(),
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
            ],
            max_legs: 2,
            venue_overrides: BTreeMap::new(),
        }
    }
}

impl RiskLimits {
    /// 按小写交易所标识查找覆盖配置。
    pub fn venue(&self, venue: &str) -> Option<&VenueRiskLimits> {
        self.venue_overrides.get(&venue.to_lowercase())
    }
}

/// 交易所硬性最小下单名义额（美元）。低于该值时交易所会直接拒单。
///
/// Lighter 与 Hyperliquid 的永续 GTC 挂单都要求单腿名义额不低于 10 USDC，
/// 与 `min_base_amount` 取较大者；这里只前置拦截名义额部分。
pub fn venue_min_notional_usd(venue: &str) -> Option<Decimal> {
    match venue.to_ascii_lowercase().as_str() {
        "lighter" | "hyperliquid" => Some(Decimal::from(10)),
        _ => None,
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
    NotionalTooSmall,
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

        // 数值型交易所覆盖只会收紧上限，取全局与各腿覆盖中的最严格值。
        let mut max_legs = limits.max_legs;
        let mut max_leverage = limits.max_leverage;
        let mut max_slippage_bps = limits.max_slippage_bps;
        for leg in &intent.legs {
            let Some(override_limits) = limits.venue(&leg.venue) else {
                continue;
            };
            if let Some(value) = override_limits.max_legs {
                max_legs = max_legs.min(value);
            }
            if let Some(value) = override_limits.max_leverage {
                max_leverage = max_leverage.min(value);
            }
            if let Some(value) = override_limits.max_slippage_bps {
                max_slippage_bps = max_slippage_bps.min(value);
            }
        }

        if context.paused && !closing {
            violations.push(RiskViolation::new(
                RiskViolationCode::ExecutorPaused,
                "执行器已暂停新开仓",
            ));
        }

        if intent.legs.is_empty() || intent.legs.len() > max_legs {
            violations.push(RiskViolation::new(
                RiskViolationCode::LegCountUnsupported,
                format!(
                    "订单腿数 {} 不在支持范围 1..={}",
                    intent.legs.len(),
                    max_legs
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
            let allowed_symbols = limits
                .venue(&leg.venue)
                .and_then(|override_limits| override_limits.allowed_symbols.as_deref())
                .unwrap_or(limits.allowed_symbols.as_slice());
            if !closing && !allowed_symbols.is_empty() && !allowed_symbols.contains(&leg.symbol) {
                violations.push(RiskViolation::new(
                    RiskViolationCode::SymbolNotAllowed,
                    format!("交易对 {} 不在允许列表内", leg.symbol),
                ));
            }
            if !closing {
                let leg_notional = leg.quantity * leg.limit_price;
                if let Some(minimum) = venue_min_notional_usd(&leg.venue) {
                    if leg_notional < minimum {
                        violations.push(RiskViolation::new(
                            RiskViolationCode::NotionalTooSmall,
                            format!(
                                "{} 单腿名义价值 {} 低于交易所最小下单额 {}",
                                leg.venue, leg_notional, minimum
                            ),
                        ));
                    }
                }
                if let Some(cap) = limits
                    .venue(&leg.venue)
                    .and_then(|override_limits| override_limits.max_order_notional_usd)
                {
                    if leg_notional > cap {
                        violations.push(RiskViolation::new(
                            RiskViolationCode::NotionalTooLarge,
                            format!(
                                "{} 单腿名义价值 {} 超过该交易所上限 {}",
                                leg.venue, leg_notional, cap
                            ),
                        ));
                    }
                }
            }
        }

        if !closing && intent.leverage > max_leverage {
            violations.push(RiskViolation::new(
                RiskViolationCode::LeverageTooHigh,
                format!("杠杆 {} 超过上限 {}", intent.leverage, max_leverage),
            ));
        }

        if intent.max_slippage_bps > max_slippage_bps {
            violations.push(RiskViolation::new(
                RiskViolationCode::SlippageTooHigh,
                format!(
                    "最大滑点 {} bps 超过上限 {} bps",
                    intent.max_slippage_bps, max_slippage_bps
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

    #[test]
    fn venue_override_restricts_symbols() {
        let now = Utc::now();
        let mut limits = RiskLimits::default();
        limits.venue_overrides.insert(
            "binance".into(),
            VenueRiskLimits {
                allowed_symbols: Some(vec!["ETHUSDT".into()]),
                ..Default::default()
            },
        );
        let context = RiskContext {
            now,
            limits,
            open_notional_usd: Decimal::ZERO,
            paused: false,
        };
        assert!(!RiskEngine::preflight(&intent(now), &context).is_approved());
    }

    #[test]
    fn venue_override_caps_leg_notional() {
        let now = Utc::now();
        let mut limits = RiskLimits::default();
        limits.venue_overrides.insert(
            "binance".into(),
            VenueRiskLimits {
                max_order_notional_usd: Some(dec!(100)),
                ..Default::default()
            },
        );
        let context = RiskContext {
            now,
            limits,
            open_notional_usd: Decimal::ZERO,
            paused: false,
        };
        assert!(!RiskEngine::preflight(&intent(now), &context).is_approved());
    }

    #[test]
    fn venue_override_tightens_leverage() {
        let now = Utc::now();
        let mut limits = RiskLimits::default();
        limits.venue_overrides.insert(
            "okx".into(),
            VenueRiskLimits {
                max_leverage: Some(dec!(0.5)),
                ..Default::default()
            },
        );
        let context = RiskContext {
            now,
            limits,
            open_notional_usd: Decimal::ZERO,
            paused: false,
        };
        assert!(!RiskEngine::preflight(&intent(now), &context).is_approved());
    }

    #[test]
    fn rejects_leg_below_venue_minimum() {
        let now = Utc::now();
        let mut intent = intent(now);
        intent.legs[0] = OrderLegIntent {
            venue: "lighter".into(),
            symbol: "ETHUSDT".into(),
            side: Side::Buy,
            // 0.0033 * 3000 = 9.9 USDC，低于 Lighter 的 10 USDC 下限。
            quantity: dec!(0.0033),
            limit_price: dec!(3000),
            reduce_only: false,
        };
        intent.legs[1] = OrderLegIntent {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            side: Side::Sell,
            quantity: dec!(0.0033),
            limit_price: dec!(3000),
            reduce_only: false,
        };
        match RiskEngine::preflight(&intent, &context(now, false)) {
            RiskDecision::Rejected { violations } => assert!(violations
                .iter()
                .any(|violation| violation.code == RiskViolationCode::NotionalTooSmall)),
            RiskDecision::Approved { .. } => panic!("低于最小下单额应被拒绝"),
        }
    }

    #[test]
    fn closing_order_ignores_venue_minimum() {
        let now = Utc::now();
        let mut intent = intent(now);
        intent.legs.iter_mut().for_each(|leg| leg.reduce_only = true);
        intent.legs[0].venue = "lighter".into();
        intent.legs[0].quantity = dec!(0.001);
        intent.legs[0].limit_price = dec!(3000);
        assert!(RiskEngine::preflight(&intent, &context(now, false)).is_approved());
    }
}
