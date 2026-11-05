-- 交易账户的美元权益快照。仅存账户 UUID 与金额，密钥始终留在执行器本机文件。

create table if not exists account_equity_points (
    id bigserial primary key,
    account_id uuid not null,
    venue text not null,
    equity_usd numeric(24, 8) not null,
    -- 采集到该快照的时间
    observed_at timestamptz not null
);

create index if not exists account_equity_points_account_idx
    on account_equity_points (account_id, observed_at desc);

create index if not exists account_equity_points_observed_at_idx
    on account_equity_points (observed_at desc);
