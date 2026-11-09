-- 仓位组：双腿开仓成交时记录成员，重启后据此把交易所同步回来的仓位归回同一组。
-- members 为 [{venue, symbol, side}]，执行器启动/同步时读取并映射 group_id。

create table if not exists position_groups (
    group_id uuid primary key,
    members jsonb not null,
    created_at timestamptz not null default now()
);
