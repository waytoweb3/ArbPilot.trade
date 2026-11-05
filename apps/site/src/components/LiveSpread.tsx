import { formatBps, formatPrice, useSpreadDemo } from "../lib/demo";
import { useI18n } from "../lib/i18n";

type LadderRow = {
  label: string;
  price: number;
  size: string;
  side: "bid" | "ask";
};

function rowsFor(base: number, isAsk: boolean, seed: number): LadderRow[] {
  return Array.from({ length: 2 }, (_, index) => {
    const step = (index + 1) * 6.4;
    const size = (0.038 + index * 0.047 + seed * 0.011).toFixed(3);
    return {
      label: `L${index + 1}`,
      price: isAsk ? base + step : base - step,
      size,
      side: isAsk ? "ask" : "bid",
    };
  });
}

function VenueLadder({
  venue,
  caption,
  rows,
}: {
  venue: string;
  caption: string;
  rows: LadderRow[];
}) {
  return (
    <div className="rounded-xl border border-line bg-panel-soft p-3.5">
      <div className="flex items-baseline justify-between">
        <span className="font-mono text-[10px] tracking-[0.18em] text-body">{venue}</span>
        <span className="text-[10px] text-subtle">{caption}</span>
      </div>
      <div className="mt-3 space-y-1.5">
        {rows.map((row) => (
          <div
            key={row.label}
            className="grid grid-cols-[1fr_auto_auto] items-baseline gap-2 font-mono text-[11px]"
          >
            <span className="text-subtle">{row.label}</span>
            <span className={row.side === "ask" ? "text-danger" : "text-accent"}>
              {formatPrice(row.price)}
            </span>
            <span className="w-10 text-right text-muted">{row.size}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function LiveSpread() {
  const snapshot = useSpreadDemo();
  const { t } = useI18n();
  const activeStep = Math.floor(Date.now() / 1600) % t.live.steps.length;

  return (
    <div className="card relative overflow-hidden p-5 shadow-[0_30px_80px_-40px_rgba(16,185,129,0.35)]">
      <div className="pointer-events-none absolute -right-16 -top-16 h-40 w-40 rounded-full bg-accent/10 blur-3xl" />

      <div className="flex items-center justify-between">
        <span className="font-mono text-[10px] uppercase tracking-[0.22em] text-muted">
          {t.live.header}
        </span>
        <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-muted">
          {t.live.demo}
        </span>
      </div>

      <div className="mt-4 flex items-end justify-between gap-4">
        <div>
          <p className="text-[13px] text-muted">{t.live.pair}</p>
          <p className="mt-1 text-2xl font-semibold tracking-tight text-fg">
            {formatPrice(snapshot.binanceBid)}
            <span className="ml-2 font-mono text-[11px] font-normal text-muted">
              {snapshot.updatedAt}
            </span>
          </p>
        </div>
        <div className="text-right">
          <p className="text-[10px] uppercase tracking-[0.16em] text-muted">{t.live.gross}</p>
          <p className="font-mono text-xl text-accent">{formatBps(snapshot.grossBps)} bps</p>
        </div>
      </div>

      <div className="mt-4 grid grid-cols-2 gap-3">
        <VenueLadder
          venue="BINANCE"
          caption={t.live.buyLeg}
          rows={rowsFor(snapshot.binanceAsk, true, 1)}
        />
        <VenueLadder
          venue="BYBIT"
          caption={t.live.sellLeg}
          rows={rowsFor(snapshot.bybitBid, false, 2)}
        />
      </div>

      <div className="mt-4 flex items-center justify-between rounded-xl border border-line bg-panel-soft px-3.5 py-3">
        <div className="flex gap-5 font-mono text-[11px]">
          <span className="text-muted">
            {t.live.fee} <span className="text-body">8 bps</span>
          </span>
          <span className="text-muted">
            {t.live.slippage} <span className="text-body">2 bps</span>
          </span>
        </div>
        <div className="text-right">
          <span className="text-[10px] uppercase tracking-[0.16em] text-muted">{t.live.net}</span>
          <span className="ml-2 font-mono text-sm text-accent">{formatBps(snapshot.netBps)} bps</span>
        </div>
      </div>

      <div className="mt-4 grid grid-cols-4 gap-1.5">
        {t.live.steps.map((step, index) => (
          <div key={step} className="space-y-1.5">
            <div
              className={`h-[3px] rounded-full transition-colors ${
                index <= activeStep ? "bg-accent-solid/70" : "bg-line-strong"
              }`}
            />
            <p className={`text-[10px] ${index === activeStep ? "text-accent" : "text-subtle"}`}>
              {String(index + 1).padStart(2, "0")} {step}
            </p>
          </div>
        ))}
      </div>

      <p className="mt-4 text-[10px] leading-relaxed text-subtle">{t.live.note}</p>
    </div>
  );
}
