-- 早期版本在数据库为空时会自动写入内置策略，导致用户没有保存也能在列表里看到。
-- 策略列表现在完全以用户保存的数据为准，这里清理掉那些自动生成的记录。
-- 正在运行或已发布到市场的策略保留，避免中断已有任务。

with removable as (
    select s.id
    from strategies s
    where s.source = 'built_in'
      and s.status <> 'running'
      and not exists (
          select 1 from strategy_publications p where p.strategy_id = s.id
      )
)
delete from strategy_versions where strategy_id in (select id from removable);

with removable as (
    select s.id
    from strategies s
    where s.source = 'built_in'
      and s.status <> 'running'
      and not exists (
          select 1 from strategy_publications p where p.strategy_id = s.id
      )
)
delete from strategy_code where strategy_id in (select id from removable);

with removable as (
    select s.id
    from strategies s
    where s.source = 'built_in'
      and s.status <> 'running'
      and not exists (
          select 1 from strategy_publications p where p.strategy_id = s.id
      )
)
delete from strategies where id in (select id from removable);
