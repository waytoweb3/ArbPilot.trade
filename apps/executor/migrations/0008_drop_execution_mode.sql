-- 策略统一按真实交易执行，不再区分模拟/实盘。
alter table strategies drop column if exists execution_mode;
