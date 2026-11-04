-- ArbPilot 执行器业务库。仅存业务状态，不存交易所密钥或订单签名材料。

create table if not exists orders (
    id uuid primary key,
    status text not null,
    origin text not null,
    symbol text not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists orders_created_at_idx on orders (created_at desc);
create index if not exists orders_symbol_idx on orders (symbol, created_at desc);

create table if not exists order_events (
    id bigserial primary key,
    order_id uuid not null references orders (id) on delete cascade,
    event_type text not null,
    detail jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists order_events_order_idx on order_events (order_id, created_at);

create table if not exists risk_events (
    id bigserial primary key,
    level text not null,
    code text not null,
    message text not null,
    order_id uuid,
    created_at timestamptz not null default now()
);

create index if not exists risk_events_created_at_idx on risk_events (created_at desc);

create table if not exists opportunities (
    id uuid primary key,
    kind text not null,
    symbol text not null,
    long_venue text not null,
    short_venue text not null,
    net_edge_bps numeric(18, 4) not null,
    payload jsonb not null,
    observed_at timestamptz not null,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

create index if not exists opportunities_observed_at_idx on opportunities (observed_at desc);

create table if not exists strategies (
    id uuid primary key,
    name text not null,
    kind text not null,
    status text not null,
    config jsonb not null default '{}'::jsonb,
    version integer not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists telegram_bindings (
    id uuid primary key,
    telegram_user_id bigint not null unique,
    label text,
    permissions jsonb not null default '["read"]'::jsonb,
    paired_at timestamptz not null default now(),
    revoked_at timestamptz
);

create table if not exists audit_log (
    id bigserial primary key,
    actor text not null,
    action text not null,
    target text,
    detail jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists audit_log_created_at_idx on audit_log (created_at desc);
