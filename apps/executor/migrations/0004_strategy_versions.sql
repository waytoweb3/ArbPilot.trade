-- 策略版本化：完整定义、校验结果、回测与部署状态快照，便于复现与回滚。
alter table strategies add column if not exists payload jsonb;
alter table strategies add column if not exists source text not null default 'built_in';
alter table strategies add column if not exists execution_mode text not null default 'paper';

create index if not exists strategies_updated_at_idx on strategies (updated_at desc);

create table if not exists strategy_versions (
    strategy_id uuid not null,
    version integer not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    primary key (strategy_id, version)
);
