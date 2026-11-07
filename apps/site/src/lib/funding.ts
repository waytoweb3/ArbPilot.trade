/** 官方只读数据接口；自建实例可用 VITE_ARBPILOT_DATA_URL 覆盖。 */
export const DATA_URL =
  (import.meta.env.VITE_ARBPILOT_DATA_URL as string | undefined) ?? "https://exec.arbpilot.trade";

type Decimal = number | string;

/** 单个交易对的资金费率快照，字段与执行器 /api/v1/funding/status 一致。 */
export type FundingSymbolStatus = {
  symbol: string;
  last_funding_time: string | null;
  last_funding_rate: Decimal | null;
  /** 该合约的实际结算间隔（秒）。 */
  funding_interval_secs: number;
  annualized_pct: Decimal | null;
  predicted_rate: Decimal | null;
  mark_price: Decimal | null;
  next_funding_at: string | null;
  observed_at: string | null;
};

export type FundingVenueStatus = {
  enabled: boolean;
  venue: string;
  base_url: string;
  poll_secs: number;
  stored_rows: number;
  last_attempt_at: string | null;
  last_success_at: string | null;
  last_error: string | null;
  symbols: FundingSymbolStatus[];
};

/** 读取官方只读实例的公共资金费率；只聚合已启用的交易所。 */
export async function fetchFundingStatus(signal?: AbortSignal): Promise<FundingVenueStatus[]> {
  const response = await fetch(`${DATA_URL}/api/v1/funding/status`, {
    signal,
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`funding status ${response.status}`);
  }
  return (await response.json()) as FundingVenueStatus[];
}
