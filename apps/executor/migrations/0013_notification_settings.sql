-- 消息通知配置：新闻推送与资金费率套利机会提醒的开关、阈值与渠道。
-- 单行表，id 固定为 1，保证全局只有一份配置。

create table if not exists notification_settings (
    id integer primary key check (id = 1),
    news_enabled boolean not null default true,
    news_sources text[] not null default array['binance', 'upbit', 'okx', 'bybit', 'bitget'],
    funding_enabled boolean not null default true,
    funding_min_annualized_pct numeric(12, 4) not null default 10,
    funding_cooldown_secs integer not null default 21600,
    telegram_enabled boolean not null default true,
    desktop_enabled boolean not null default true,
    updated_at timestamptz not null default now()
);

insert into notification_settings (id) values (1) on conflict (id) do nothing;
