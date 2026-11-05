-- Binance 资金费率历史（公开行情数据，不含任何密钥）。

create table if not exists funding_rates (
    id bigserial primary key,
    venue text not null,
    symbol text not null,
    -- 交易所的结算时间（Binance fundingRate 的 fundingTime）
    funding_time timestamptz not null,
    funding_rate numeric(24, 12) not null,
    mark_price numeric(24, 8),
    -- 本系统写入/更新该行的时间
    ingested_at timestamptz not null default now(),
    created_at timestamptz not null default now()
);

-- 同一交易对在同一结算时间只保留一条，重复采集走 update。
create unique index if not exists funding_rates_unique_idx
    on funding_rates (venue, symbol, funding_time);

create index if not exists funding_rates_symbol_funding_time_idx
    on funding_rates (symbol, funding_time desc);
