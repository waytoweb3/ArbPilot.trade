-- 内置策略模板：海龟突破、均线趋势、网格。
-- 源码与默认定义都放在 payload 里，执行器启动时把源码编译成沙箱模块。

create table if not exists strategy_templates (
    key text primary key,
    name text not null,
    payload jsonb not null,
    updated_at timestamptz not null default now()
);
