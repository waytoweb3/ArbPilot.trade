-- 技能策略（SKILL.md 参数化）：每个策略实例一份设置，以 JSON 载荷整份保存。
create table if not exists skill_settings (
    id int primary key,
    payload jsonb not null,
    updated_at timestamptz not null default now()
);
