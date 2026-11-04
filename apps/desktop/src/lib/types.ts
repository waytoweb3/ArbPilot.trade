/** 与 Rust 端 rust_decimal 序列化保持一致：可能是数字或字符串。 */
export type Decimal = number | string;

export type TradingMode = "paper" | "live";

export type OpportunityKind = "funding_rate" | "cex_cex_spread" | "cex_dex_spread";

export type VenueKind = "cex" | "dex";

export type Side = "buy" | "sell";

export type OrderOrigin = "desktop" | "telegram" | "strategy";

export type LegStatus = "pending" | "submitted" | "partially_filled" | "filled" | "cancelled" | "rejected";

export type OrderStatus =
  | "pending_risk"
  | "rejected"
  | "submitting"
  | "partially_filled"
  | "filled"
  | "unwinding"
  | "cancelled"
  | "failed";

export type StrategyStatus = "draft" | "running" | "paused" | "stopped";

export interface HealthResponse {
  status: "ok" | "degraded";
  version: string;
  mode: TradingMode;
  paused: boolean;
  database_connected: boolean;
  uptime_secs: number;
  server_time: string;
}

export interface Opportunity {
  id: string;
  kind: OpportunityKind;
  symbol: string;
  long_venue: string;
  short_venue: string;
  gross_spread_bps: Decimal;
  estimated_fee_bps: Decimal;
  estimated_slippage_bps: Decimal;
  net_edge_bps: Decimal;
  max_notional_usd: Decimal;
  funding_rate: Decimal | null;
  observed_at: string;
  expires_at: string;
}

export interface OrderLegIntent {
  venue: string;
  symbol: string;
  side: Side;
  quantity: Decimal;
  limit_price: Decimal;
  reduce_only: boolean;
}

export interface OrderLeg {
  intent: OrderLegIntent;
  status: LegStatus;
  venue_order_id: string | null;
  filled_quantity: Decimal;
  average_price: Decimal | null;
}

export interface OrderIntent {
  idempotency_key: string;
  origin: OrderOrigin;
  strategy_id: string | null;
  legs: OrderLegIntent[];
  leverage: Decimal;
  max_slippage_bps: Decimal;
  quote_observed_at: string;
  created_at: string;
}

export interface Order {
  id: string;
  status: OrderStatus;
  intent: OrderIntent;
  legs: OrderLeg[];
  note: string | null;
  created_at: string;
  updated_at: string;
}

export interface Position {
  id: string;
  venue: string;
  symbol: string;
  side: Side;
  quantity: Decimal;
  entry_price: Decimal;
  mark_price: Decimal;
  leverage: Decimal;
  unrealized_pnl: Decimal;
  updated_at: string;
}

export interface Balance {
  asset: string;
  free: Decimal;
  locked: Decimal;
}

export interface AccountSnapshot {
  venue: string;
  balances: Balance[];
  equity_usd: Decimal;
  margin_used_usd: Decimal;
  available_margin_usd: Decimal;
  observed_at: string;
}

export interface StrategyRecord {
  id: string;
  name: string;
  kind: OpportunityKind;
  status: StrategyStatus;
  venues: string[];
  symbols: string[];
  last_heartbeat_at: string | null;
  updated_at: string;
}

export interface RiskLimits {
  max_order_notional_usd: Decimal;
  max_total_notional_usd: Decimal;
  max_leverage: Decimal;
  max_slippage_bps: Decimal;
  max_quote_age_secs: number;
  allowed_symbols: string[];
  allowed_venues: string[];
  max_legs: number;
}

export interface RiskViolation {
  code: string;
  message: string;
}

export type RiskDecision =
  | { decision: "approved"; estimated_notional_usd: Decimal }
  | { decision: "rejected"; violations: RiskViolation[] };

export interface CreateOrderRequest {
  symbol: string;
  long_venue: string;
  short_venue: string;
  notional_usd: number;
  leverage: number;
  max_slippage_bps?: number;
  origin?: OrderOrigin;
}

export interface CreateOrderResponse {
  order: Order;
  decision: RiskDecision | null;
  replayed: boolean;
}

export interface ControlResponse {
  paused: boolean;
  reason: string | null;
}

export interface ExecutorEvent {
  type: "opportunity_updated" | "order_updated" | "executor_status" | "alert";
  [key: string]: unknown;
}
