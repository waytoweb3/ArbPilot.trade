-- 上币单腿交易：设置（单行）与交易记录，均以 JSON 载荷保存，便于随业务字段演进。
create table if not exists listing_settings (
    id int primary key,
    payload jsonb not null,
    updated_at timestamptz not null default now()
);

create table if not exists listing_trades (
    id uuid primary key,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
