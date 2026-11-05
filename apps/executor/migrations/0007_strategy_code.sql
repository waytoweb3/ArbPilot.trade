-- 策略沙箱源码：每个策略保留最新一份，历史由 strategy_versions 承担。
-- 源码只描述受限意图输出，不含密钥、网络或下单能力。
create table if not exists strategy_code (
    strategy_id uuid primary key,
    payload jsonb not null,
    updated_at timestamptz not null default now()
);
