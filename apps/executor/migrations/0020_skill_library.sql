-- 技能库：独立 Skill 与不可变版本，整份 JSON 载荷保存。
create table if not exists skills (
    id uuid primary key,
    author_id uuid not null,
    payload jsonb not null,
    updated_at timestamptz not null default now()
);

create index if not exists skills_author_idx on skills (author_id, updated_at desc);

create table if not exists skill_versions (
    skill_id uuid not null,
    version integer not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    primary key (skill_id, version)
);
