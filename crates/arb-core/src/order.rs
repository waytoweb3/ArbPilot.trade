use crate::types::{LegStatus, Order, OrderLeg, OrderStatus};

/// 异常状态下执行器应采取的动作。禁止静默重试，必须先与交易所对账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compensation {
    None,
    /// 撤销尚未成交的腿。
    CancelRemainingLegs,
    /// 已成交的腿需要反向平掉，索引对应订单腿下标。
    UnwindFilledLegs(Vec<usize>),
    /// 无法自动处置，暂停开仓并告警，等待人工确认。
    HaltAndAlert,
}

impl OrderStatus {
    /// 合法状态迁移表。任何未列出的迁移都应被视为程序错误。
    pub fn can_transition_to(self, next: OrderStatus) -> bool {
        use OrderStatus::*;
        matches!(
            (self, next),
            (PendingRisk, Rejected)
                | (PendingRisk, Submitting)
                | (Submitting, PartiallyFilled)
                | (Submitting, Filled)
                | (Submitting, Cancelled)
                | (Submitting, Failed)
                | (PartiallyFilled, Filled)
                | (PartiallyFilled, Unwinding)
                | (PartiallyFilled, Cancelled)
                | (PartiallyFilled, Failed)
                | (Unwinding, Cancelled)
                | (Unwinding, Failed)
        )
    }

    /// 根据当前状态与各腿成交情况给出补偿动作。
    pub fn compensation(self, legs: &[OrderLeg]) -> Compensation {
        if legs.is_empty() {
            return Compensation::None;
        }

        let filled: Vec<usize> = legs
            .iter()
            .enumerate()
            .filter(|(_, leg)| leg.is_filled())
            .map(|(index, _)| index)
            .collect();
        let pending = legs.iter().any(|leg| !leg.is_filled());
        // 只有还能撤销的腿才算可撤单：已拒绝/已撤销的腿没有剩余动作，
        // 单腿订单失败时不应再给出「撤销剩余腿」的补偿。
        let cancellable = legs.iter().any(|leg| {
            matches!(
                leg.status,
                LegStatus::Pending | LegStatus::Submitted | LegStatus::PartiallyFilled
            )
        });

        match self {
            OrderStatus::Rejected | OrderStatus::Filled | OrderStatus::Cancelled => {
                Compensation::None
            }
            OrderStatus::PendingRisk | OrderStatus::Submitting => {
                if filled.is_empty() {
                    Compensation::None
                } else if pending {
                    Compensation::UnwindFilledLegs(filled)
                } else {
                    Compensation::HaltAndAlert
                }
            }
            OrderStatus::PartiallyFilled | OrderStatus::Unwinding | OrderStatus::Failed => {
                if filled.is_empty() {
                    if cancellable {
                        Compensation::CancelRemainingLegs
                    } else {
                        Compensation::None
                    }
                } else {
                    Compensation::UnwindFilledLegs(filled)
                }
            }
        }
    }
}

impl Order {
    /// 迁移状态并记录原因；非法迁移返回错误而不是静默覆盖。
    pub fn transition(&mut self, next: OrderStatus, note: Option<String>) -> Result<(), String> {
        if !self.status.can_transition_to(next) {
            return Err(format!(
                "非法的订单状态迁移: {:?} -> {:?}",
                self.status, next
            ));
        }
        self.status = next;
        if note.is_some() {
            self.note = note;
        }
        self.updated_at = chrono::Utc::now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderLegIntent, Side};
    use rust_decimal::Decimal;

    fn leg(status: crate::types::LegStatus) -> OrderLeg {
        let mut leg = OrderLeg::pending(OrderLegIntent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            quantity: Decimal::ONE,
            limit_price: Decimal::ONE,
            reduce_only: false,
        });
        leg.status = status;
        leg
    }

    #[test]
    fn filled_legs_require_unwind() {
        let legs = vec![
            leg(crate::types::LegStatus::Filled),
            leg(crate::types::LegStatus::Pending),
        ];
        assert_eq!(
            OrderStatus::PartiallyFilled.compensation(&legs),
            Compensation::UnwindFilledLegs(vec![0])
        );
    }

    #[test]
    fn rejected_single_leg_needs_no_compensation() {
        let legs = vec![leg(LegStatus::Rejected)];
        assert_eq!(
            OrderStatus::PartiallyFilled.compensation(&legs),
            Compensation::None
        );
    }

    #[test]
    fn rejects_invalid_transition() {
        let mut order = Order::from_intent(
            crate::types::OrderIntent {
                idempotency_key: "k".into(),
                origin: crate::types::OrderOrigin::Desktop,
                strategy_id: None,
                legs: vec![],
                leverage: Decimal::ONE,
                max_slippage_bps: Decimal::ONE,
                quote_observed_at: chrono::Utc::now(),
                created_at: chrono::Utc::now(),
            },
            OrderStatus::Filled,
        );
        assert!(order.transition(OrderStatus::Submitting, None).is_err());
    }
}
