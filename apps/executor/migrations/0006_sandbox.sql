-- WASM 策略沙箱模块。模块内容只存放在执行器本机，不包含任何密钥。
-- 绑定关系保存在 strategies.payload 的 sandbox 字段中。

create table if not exists sandbox_artifacts (
    id uuid primary key,
    name text not null,
    sha256 text not null,
    size_bytes bigint not null,
    abi_version integer,
    source text not null,
    payload jsonb not null,
    wasm bytea not null,
    created_at timestamptz not null default now(),
    unique (sha256)
);
