-- 策略可选市场配置：交易所与交易对统一保存在数据库，桌面端只读取这里的数据。
-- 现货腿用 `-spot` 后缀区分，同一交易所的现货与永续共享一份交易对清单。

create table if not exists exchange_venues (
    venue text not null,
    segment text not null check (segment in ('spot', 'perp')),
    display_name text not null default '',
    enabled boolean not null default true,
    sort_order integer not null default 0,
    updated_at timestamptz not null default now(),
    primary key (venue, segment)
);

create table if not exists tradable_symbols (
    venue text not null,
    symbol text not null,
    enabled boolean not null default true,
    sort_order integer not null default 0,
    updated_at timestamptz not null default now(),
    primary key (venue, symbol)
);

-- 初始交易所清单；后续可直接改库或通过接口维护。
insert into exchange_venues (venue, segment, display_name, sort_order) values
    ('binance-spot', 'spot', 'Binance Spot', 10),
    ('okx-spot', 'spot', 'OKX Spot', 20),
    ('binance', 'perp', 'Binance Perp', 10),
    ('okx', 'perp', 'OKX Swap', 20),
    ('bybit', 'perp', 'Bybit Perp', 30),
    ('bitget', 'perp', 'Bitget Perp', 40),
    ('gate', 'perp', 'Gate Perp', 50),
    ('aster', 'perp', 'Aster Perp', 60),
    ('hyperliquid', 'perp', 'Hyperliquid Perp', 70),
    ('lighter', 'perp', 'Lighter Perp', 80),
    ('htx', 'perp', 'HTX Perp', 90),
    ('mexc', 'perp', 'MEXC Perp', 100),
    ('bingx', 'perp', 'BingX Perp', 110)
on conflict (venue, segment) do nothing;

-- 初始交易对清单，资金费率采集器发现新合约后会自动补充更多交易对。
insert into tradable_symbols (venue, symbol, sort_order)
select venue, symbol, sort_order
from (
    values
        ('binance'), ('okx'), ('bybit'), ('bitget'), ('gate'),
        ('aster'), ('hyperliquid'), ('lighter'), ('htx'), ('mexc'), ('bingx')
) as venues(venue)
cross join (
    values
        ('BTCUSDT', 10),
        ('ETHUSDT', 20),
        ('SOLUSDT', 30),
        ('XRPUSDT', 40),
        ('BNBUSDT', 50),
        ('DOGEUSDT', 60),
        ('ADAUSDT', 70),
        ('LINKUSDT', 80),
        ('AVAXUSDT', 90),
        ('TONUSDT', 100)
) as symbols(symbol, sort_order)
on conflict (venue, symbol) do nothing;
