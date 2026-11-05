-- 策略分享与付费市场：作者卡片、发布、表现记录、订阅与结算。
-- 只保存业务数据，不保存任何支付凭证、私钥或银行账号。

create table if not exists strategy_authors (
    author_id uuid primary key,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists strategy_publications (
    id uuid primary key,
    strategy_id uuid not null,
    author_id uuid not null,
    status text not null,
    strategy_version integer not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists strategy_publications_author_idx
    on strategy_publications (author_id);
create index if not exists strategy_publications_strategy_idx
    on strategy_publications (strategy_id);

create table if not exists strategy_performance_records (
    id uuid primary key,
    publication_id uuid not null,
    source text not null,
    payload jsonb not null,
    recorded_at timestamptz not null default now()
);

create index if not exists strategy_performance_publication_idx
    on strategy_performance_records (publication_id);

create table if not exists strategy_subscriptions (
    id uuid primary key,
    publication_id uuid not null,
    subscriber_id uuid not null,
    status text not null,
    payload jsonb not null,
    started_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists strategy_subscriptions_subscriber_idx
    on strategy_subscriptions (subscriber_id);
create index if not exists strategy_subscriptions_publication_idx
    on strategy_subscriptions (publication_id);

create table if not exists strategy_settlements (
    id uuid primary key,
    author_id uuid not null,
    period text not null,
    status text not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (author_id, period)
);
