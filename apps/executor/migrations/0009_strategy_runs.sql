-- 策略运行实例：一条策略在某个交易所组合上的一次运行。
create table if not exists strategy_runs (
  id uuid primary key,
  strategy_id uuid not null references strategies(id) on delete cascade,
  strategy_version int not null,
  -- self_hosted：用户自己的执行器；platform：发布后由平台托管执行器运行。
  scope text not null,
  status text not null,
  long_venue text not null,
  short_venue text not null,
  symbols text[] not null default '{}',
  payload jsonb not null,
  started_at timestamptz not null default now(),
  stopped_at timestamptz,
  last_heartbeat_at timestamptz
);

create index if not exists strategy_runs_strategy_idx
  on strategy_runs (strategy_id, started_at desc);

-- 运行事件：评估、信号、下单与错误，按运行实例留存，便于回看策略当时发生了什么。
create table if not exists strategy_run_events (
  id bigserial primary key,
  run_id uuid not null references strategy_runs(id) on delete cascade,
  strategy_id uuid not null,
  level text not null,
  payload jsonb not null,
  created_at timestamptz not null default now()
);

create index if not exists strategy_run_events_run_idx
  on strategy_run_events (run_id, created_at desc);
