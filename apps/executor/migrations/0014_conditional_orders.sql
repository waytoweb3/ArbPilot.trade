-- 条件单：等待资金费率或价格达到阈值后自动提交双腿订单。
-- 触发条件、有效期与执行状态都放在 payload 里，状态流转由执行器后台轮询驱动。

create table if not exists conditional_orders (
    id uuid primary key,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists conditional_orders_created_at_idx
    on conditional_orders (created_at desc);
