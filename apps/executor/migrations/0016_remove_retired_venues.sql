-- 下线 HTX / MEXC / BingX：从可选交易所与交易对清单中移除。

delete from tradable_symbols where venue in ('htx', 'mexc', 'bingx');
delete from exchange_venues where venue in ('htx', 'mexc', 'bingx');
