-- 补齐 Bybit 现货：交易所白名单已允许 bybit-spot，策略可选市场也需要对应条目。

insert into exchange_venues (venue, segment, display_name, sort_order) values
    ('bybit-spot', 'spot', 'Bybit Spot', 30)
on conflict (venue, segment) do nothing;
