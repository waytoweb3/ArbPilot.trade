//! WASM + Wasmtime 策略沙箱。
//!
//! 隔离边界：
//! - 不链接 WASI，模块无法读写文件、发起网络请求或读取环境变量；
//! - 只导入 `arbpilot` 命名空间下的 emit/log 与结构化行情读取、决策辅助函数；
//! - 通过燃料限制、epoch 超时、线性内存上限与输出大小上限约束资源；
//! - 输入只有确定性行情快照，输出只有开仓/平仓/保持意图；
//! - 本模块不持有交易所密钥，也没有下单能力；意图必须交给统一风控网关。

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
    time::{Duration, Instant},
};

use arb_core::{
    OpportunityKind, SandboxArtifact, SandboxDecision, SandboxInput, SandboxLimits,
    SandboxMarketSnapshot, SandboxOrderLimits, SandboxOutput, SandboxPositionSnapshot,
    SandboxRunReport, SandboxRunStatus, SANDBOX_ABI_VERSION, SANDBOX_CLOSE, SANDBOX_EMIT,
    SANDBOX_ENTRYPOINT, SANDBOX_HOLD, SANDBOX_IMPORT_NAMESPACE, SANDBOX_LOG, SANDBOX_MARKET,
    SANDBOX_MARKETS, SANDBOX_OPEN, SANDBOX_POSITION_HELD_SECS, SANDBOX_POSITION_NOTIONAL,
    SANDBOX_POSITION_PRESENT,
};
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use sha2::{Digest, Sha256};
use wasmtime::{
    Caller, Config, Engine, Instance, Linker, Memory, Module, OptLevel, Store, StoreLimits,
    StoreLimitsBuilder, Trap, TypedFunc,
};

use crate::state::AppState;

const EPOCH_TICK: Duration = Duration::from_millis(10);
const DEFAULT_SPOT_VENUE: &str = "binance-spot";
const DEFAULT_PERP_VENUE: &str = "binance";

struct Runtime {
    engine: Engine,
    modules: RwLock<HashMap<String, Module>>,
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.cranelift_opt_level(OptLevel::SpeedAndSize);
        let engine = Engine::new(&config).expect("初始化 Wasmtime 引擎失败");

        // 全局 epoch 递增线程：超时通过 Store 的 epoch deadline 中断执行。
        let ticker = engine.clone();
        std::thread::Builder::new()
            .name("arbpilot-sandbox-epoch".to_string())
            .spawn(move || loop {
                std::thread::sleep(EPOCH_TICK);
                ticker.increment_epoch();
            })
            .expect("启动沙箱 epoch 线程失败");

        Runtime {
            engine,
            modules: RwLock::new(HashMap::new()),
        }
    })
}

/// 模块通过辅助函数能读到的确定性输入快照。
///
/// 快照只包含行情与持仓的数值，不含账户、凭证或任何可写能力；静态检查阶段
/// （无输入）使用空快照，所有读取函数安全返回 0。
#[derive(Debug, Default)]
struct InputSnapshot {
    /// 每个市场按 `SANDBOX_FIELD_*` 顺序展开的字段。
    markets: Vec<[f64; 8]>,
    /// 持仓名义价值（USD）与已持有秒数。
    position: Option<(f64, i64)>,
}

impl InputSnapshot {
    fn from_input(input: &SandboxInput) -> Self {
        Self {
            markets: input
                .markets
                .iter()
                .map(|market| {
                    [
                        to_f64(market.spot_bid),
                        to_f64(market.spot_ask),
                        to_f64(market.perp_bid),
                        to_f64(market.perp_ask),
                        to_f64(market.funding_rate),
                        to_f64(market.gross_spread_bps),
                        to_f64(market.net_edge_bps),
                        to_f64(market.max_notional_usd),
                    ]
                })
                .collect(),
            position: input.position.as_ref().map(|position| {
                (
                    to_f64(position.notional_usd),
                    position.held_secs.min(i64::MAX as u64) as i64,
                )
            }),
        }
    }
}

fn to_f64(value: Decimal) -> f64 {
    value.to_f64().unwrap_or(0.0)
}

/// 主机侧状态：资源限制、输出缓冲与输入快照。
struct HostState {
    limits: StoreLimits,
    output: Vec<u8>,
    /// 通过 `emit` 自行序列化的输出。
    logs: Vec<String>,
    /// 通过 `open` / `close` / `hold` 辅助函数提交的决策。
    decisions: Vec<SandboxDecision>,
    snapshot: InputSnapshot,
    max_output_bytes: usize,
    max_decisions: usize,
    max_log_lines: usize,
    rejection: Option<String>,
}

/// 计算模块摘要，作为缓存键与版本绑定依据。
pub fn hash_wasm(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 编译并静态检查导入/导出。上传阶段调用，尽早拒绝不合规模块。
pub fn inspect(bytes: &[u8], limits: &SandboxLimits) -> Result<u32, String> {
    if bytes.is_empty() {
        return Err("wasm 文件为空".to_string());
    }
    if bytes.len() > limits.max_artifact_bytes {
        return Err(format!(
            "wasm 文件 {} 字节，超过上限 {} 字节",
            bytes.len(),
            limits.max_artifact_bytes
        ));
    }

    let module = compile(bytes)?;
    let (mut store, instance) = instantiate(&module, limits)?;
    let abi = instance
        .get_typed_func::<(), i32>(&mut store, "abi_version")
        .map_err(|_| "模块缺少导出函数 abi_version()".to_string())?
        .call(&mut store, ())
        .map_err(|error| format!("调用 abi_version() 失败: {error}"))?;
    if abi as u32 != SANDBOX_ABI_VERSION {
        return Err(format!(
            "模块 ABI 版本为 {abi}，当前执行器只接受 {SANDBOX_ABI_VERSION}"
        ));
    }
    Ok(abi as u32)
}

fn compile(bytes: &[u8]) -> Result<Module, String> {
    let runtime = runtime();
    let key = hash_wasm(bytes);
    if let Some(module) = runtime
        .modules
        .read()
        .expect("沙箱模块锁不可用")
        .get(&key)
        .cloned()
    {
        return Ok(module);
    }

    let module = Module::from_binary(&runtime.engine, bytes)
        .map_err(|error| format!("wasm 编译失败: {error}"))?;
    check_imports(&module)?;
    check_exports(&module)?;
    runtime
        .modules
        .write()
        .expect("沙箱模块锁不可用")
        .insert(key, module.clone());
    Ok(module)
}

fn check_imports(module: &Module) -> Result<(), String> {
    const ALLOWED: [&str; 10] = [
        SANDBOX_EMIT,
        SANDBOX_LOG,
        SANDBOX_MARKETS,
        SANDBOX_MARKET,
        SANDBOX_POSITION_PRESENT,
        SANDBOX_POSITION_NOTIONAL,
        SANDBOX_POSITION_HELD_SECS,
        SANDBOX_OPEN,
        SANDBOX_CLOSE,
        SANDBOX_HOLD,
    ];
    for import in module.imports() {
        let allowed = import.module() == SANDBOX_IMPORT_NAMESPACE
            && ALLOWED.contains(&import.name());
        if !allowed {
            return Err(format!(
                "模块导入了不允许的依赖 {}.{}；沙箱只提供 {SANDBOX_IMPORT_NAMESPACE} 下的行情读取与决策辅助函数",
                import.module(),
                import.name()
            ));
        }
    }
    Ok(())
}

fn check_exports(module: &Module) -> Result<(), String> {
    let mut has_memory = false;
    let mut has_evaluate = false;
    let mut has_alloc = false;
    let mut has_abi = false;
    for export in module.exports() {
        match export.name() {
            "memory" => has_memory = true,
            SANDBOX_ENTRYPOINT => has_evaluate = true,
            "alloc" => has_alloc = true,
            "abi_version" => has_abi = true,
            _ => {}
        }
    }
    if !has_memory {
        return Err("模块缺少导出 memory".to_string());
    }
    if !has_evaluate {
        return Err(format!("模块缺少导出函数 {SANDBOX_ENTRYPOINT}"));
    }
    if !has_alloc {
        return Err("模块缺少导出函数 alloc(len) -> ptr".to_string());
    }
    if !has_abi {
        return Err("模块缺少导出函数 abi_version()".to_string());
    }
    Ok(())
}

fn store_limits(limits: &SandboxLimits) -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes)
        .memories(1)
        .instances(1)
        .tables(2)
        .trap_on_grow_failure(true)
        .build()
}

fn instantiate(
    module: &Module,
    limits: &SandboxLimits,
) -> Result<(Store<HostState>, Instance), String> {
    instantiate_with_snapshot(module, limits, InputSnapshot::default())
}

fn instantiate_with_snapshot(
    module: &Module,
    limits: &SandboxLimits,
    snapshot: InputSnapshot,
) -> Result<(Store<HostState>, Instance), String> {
    let engine = runtime().engine.clone();
    let state = HostState {
        limits: store_limits(limits),
        output: Vec::new(),
        logs: Vec::new(),
        decisions: Vec::new(),
        snapshot,
        max_output_bytes: limits.max_output_bytes,
        max_decisions: limits.max_decisions,
        max_log_lines: limits.max_log_lines,
        rejection: None,
    };
    let mut store = Store::new(&engine, state);
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.max_fuel)
        .map_err(|error| format!("设置燃料失败: {error}"))?;
    let ticks = (limits.timeout_ms / EPOCH_TICK.as_millis() as u64).max(1);
    store.set_epoch_deadline(ticks);

    let mut linker: Linker<HostState> = Linker::new(&engine);
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_EMIT,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                if ptr < 0 || len < 0 {
                    return 1;
                }
                let Some(memory) = exported_memory(&mut caller) else {
                    return 1;
                };
                let start = ptr as usize;
                let end = start.saturating_add(len as usize);
                let Some(bytes) = memory
                    .data(&caller)
                    .get(start..end)
                    .map(<[u8]>::to_vec)
                else {
                    return 1;
                };
                let state = caller.data_mut();
                if state.output.len() + bytes.len() > state.max_output_bytes {
                    state.rejection = Some("模块输出超过沙箱上限".to_string());
                    return 1;
                }
                state.output.extend_from_slice(&bytes);
                0
            },
        )
        .map_err(|error| format!("注册 emit 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_LOG,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                if ptr < 0 || len < 0 {
                    return 1;
                }
                let Some(memory) = exported_memory(&mut caller) else {
                    return 1;
                };
                let start = ptr as usize;
                let end = start.saturating_add(len as usize);
                let Some(bytes) = memory
                    .data(&caller)
                    .get(start..end)
                    .map(<[u8]>::to_vec)
                else {
                    return 1;
                };
                let Ok(line) = String::from_utf8(bytes) else {
                    return 1;
                };
                let state = caller.data_mut();
                if state.logs.len() < state.max_log_lines {
                    state.logs.push(line);
                }
                0
            },
        )
        .map_err(|error| format!("注册 log 失败: {error}"))?;

    // 结构化行情读取：模块无需自己解析 JSON。
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_MARKETS,
            |caller: Caller<'_, HostState>| -> i32 {
                caller.data().snapshot.markets.len().min(i32::MAX as usize) as i32
            },
        )
        .map_err(|error| format!("注册 markets 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_MARKET,
            |caller: Caller<'_, HostState>, index: i32, field: i32| -> f64 {
                if index < 0 || field < 0 {
                    return 0.0;
                }
                caller
                    .data()
                    .snapshot
                    .markets
                    .get(index as usize)
                    .and_then(|market| market.get(field as usize))
                    .copied()
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 market 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_PRESENT,
            |caller: Caller<'_, HostState>| -> i32 {
                caller.data().snapshot.position.is_some() as i32
            },
        )
        .map_err(|error| format!("注册 position_present 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_NOTIONAL,
            |caller: Caller<'_, HostState>| -> f64 {
                caller.data().snapshot.position.map(|p| p.0).unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 position_notional 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_HELD_SECS,
            |caller: Caller<'_, HostState>| -> i64 {
                caller.data().snapshot.position.map(|p| p.1).unwrap_or(0)
            },
        )
        .map_err(|error| format!("注册 position_held_secs 失败: {error}"))?;

    // 决策辅助：直接提交意图，模块不必拼 JSON。
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_OPEN,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, notional: f64| -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                let Some(notional_usd) = Decimal::from_f64(notional) else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::Open {
                        symbol,
                        notional_usd,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 open 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_CLOSE,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::Close {
                        symbol,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 close 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_HOLD,
            |mut caller: Caller<'_, HostState>| -> i32 {
                push_decision(caller.data_mut(), SandboxDecision::Hold { reason: None })
            },
        )
        .map_err(|error| format!("注册 hold 失败: {error}"))?;

    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|error| format!("实例化 wasm 模块失败: {error}"))?;
    Ok((store, instance))
}

/// 从模块线性内存读取 UTF-8 字符串；过长的交易对名直接拒绝。
fn read_guest_string(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<String> {
    const MAX_SYMBOL_BYTES: usize = 64;
    if ptr < 0 || len < 0 {
        return None;
    }
    let len = len as usize;
    if len == 0 || len > MAX_SYMBOL_BYTES {
        return None;
    }
    let memory = exported_memory(caller)?;
    let start = ptr as usize;
    let end = start.checked_add(len)?;
    let bytes = memory.data(&*caller).get(start..end)?;
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

fn push_decision(state: &mut HostState, decision: SandboxDecision) -> i32 {
    if state.decisions.len() >= state.max_decisions {
        state.rejection = Some("模块提交的决策条数超过沙箱上限".to_string());
        return 1;
    }
    state.decisions.push(decision);
    0
}

fn exported_memory(caller: &mut Caller<'_, HostState>) -> Option<Memory> {
    caller.get_export("memory").and_then(|export| export.into_memory())
}

/// 同步执行一次评估，调用方应放在阻塞线程池中。
fn run_blocking(
    artifact: &SandboxArtifact,
    bytes: &[u8],
    input: &SandboxInput,
    limits: &SandboxLimits,
) -> SandboxRunReport {
    let started = Instant::now();
    let module = match compile(bytes) {
        Ok(module) => module,
        Err(error) => {
            return SandboxRunReport::failed(
                artifact,
                limits,
                SandboxRunStatus::Invalid,
                error,
                elapsed_ms(started),
            )
        }
    };

    let (mut store, instance) = match instantiate_with_snapshot(
        &module,
        limits,
        InputSnapshot::from_input(input),
    ) {
        Ok(pair) => pair,
        Err(error) => {
            return SandboxRunReport::failed(
                artifact,
                limits,
                SandboxRunStatus::Invalid,
                error,
                elapsed_ms(started),
            )
        }
    };

    let result = invoke(&mut store, &instance, input);
    let fuel_consumed = store
        .get_fuel()
        .map(|remaining| limits.max_fuel.saturating_sub(remaining))
        .unwrap_or(0);
    let memory_bytes = instance
        .get_memory(&mut store, "memory")
        .map(|memory| memory.data_size(&store))
        .unwrap_or(0);
    let output_bytes = store.data().output.len();
    let logs = store.data().logs.clone();
    let rejection = store.data().rejection.clone();
    let helper_decisions = store.data().decisions.clone();

    let (status, decisions, error) = match result {
        Ok(()) => {
            if let Some(message) = rejection {
                (SandboxRunStatus::Invalid, Vec::new(), Some(message))
            } else {
                match combine_output(&store.data().output, helper_decisions) {
                    Ok(output) => {
                        let (output, _issues) = output.sanitize(limits);
                        let mut invalid: Option<String> = None;
                        for decision in &output.decisions {
                            if let Some(issue) = decision.validate().into_iter().next() {
                                invalid = Some(format!("{}: {}", issue.path, issue.message));
                                break;
                            }
                        }
                        match invalid {
                            Some(message) => (SandboxRunStatus::Invalid, Vec::new(), Some(message)),
                            None => (SandboxRunStatus::Ok, output.decisions, None),
                        }
                    }
                    Err(message) => (SandboxRunStatus::Invalid, Vec::new(), Some(message)),
                }
            }
        }
        Err(error) => (classify(&error), Vec::new(), Some(format!("{error}"))),
    };

    SandboxRunReport {
        artifact_id: artifact.id,
        artifact_hash: artifact.sha256.clone(),
        abi_version: SANDBOX_ABI_VERSION,
        status,
        decisions,
        logs,
        fuel_limit: limits.max_fuel,
        fuel_consumed,
        max_memory_bytes: memory_bytes,
        duration_ms: elapsed_ms(started),
        output_bytes,
        error,
        ran_at: chrono::Utc::now(),
    }
}

/// 把实时资金费率机会转成确定性输入。模块看不到账户、密钥或任何凭证。
pub fn build_input(
    state: &AppState,
    record: Option<&arb_core::StrategyRecord>,
    symbol: Option<&str>,
    fallback_strategy_id: uuid::Uuid,
) -> SandboxInput {
    use rust_decimal::Decimal;

    let now = chrono::Utc::now();
    let (spot_venue, perp_venue, symbols, order_limits) = match record {
        Some(record) => (
            record
                .definition
                .spot_venue()
                .map(str::to_string)
                .unwrap_or_else(|| DEFAULT_SPOT_VENUE.to_string()),
            record
                .definition
                .perp_venue()
                .map(str::to_string)
                .unwrap_or_else(|| DEFAULT_PERP_VENUE.to_string()),
            record.definition.universe.symbols.clone(),
            SandboxOrderLimits {
                max_notional_usd: record
                    .definition
                    .risk
                    .max_notional_usd
                    .min(state.risk_limits().max_order_notional_usd),
                max_leverage: record.definition.execution.leverage,
                max_slippage_bps: record.definition.execution.max_slippage_bps,
                max_unhedged_secs: record.definition.risk.max_unhedged_seconds as u64,
            },
        ),
        None => (
            DEFAULT_SPOT_VENUE.to_string(),
            DEFAULT_PERP_VENUE.to_string(),
            Vec::new(),
            SandboxOrderLimits {
                max_notional_usd: state.risk_limits().max_order_notional_usd,
                max_leverage: state.risk_limits().max_leverage,
                max_slippage_bps: Decimal::from(10),
                max_unhedged_secs: 3,
            },
        ),
    };

    let markets: Vec<SandboxMarketSnapshot> = state
        .market
        .opportunities()
        .into_iter()
        .filter(|opportunity| {
            opportunity.kind == OpportunityKind::FundingRate
                && opportunity.is_fresh(now, chrono::Duration::seconds(30))
        })
        .filter(|opportunity| {
            symbol
                .map(|symbol| opportunity.symbol == symbol)
                .unwrap_or(true)
        })
        .filter(|opportunity| {
            symbols.is_empty()
                || (symbols.contains(&opportunity.symbol)
                    && opportunity.long_venue == spot_venue
                    && opportunity.short_venue == perp_venue)
        })
        .filter_map(|opportunity| {
            let spot = state
                .market
                .quote(&opportunity.long_venue, &opportunity.symbol)?;
            let perp = state
                .market
                .quote(&opportunity.short_venue, &opportunity.symbol)?;
            Some(SandboxMarketSnapshot {
                symbol: opportunity.symbol.clone(),
                spot_venue: opportunity.long_venue.clone(),
                perp_venue: opportunity.short_venue.clone(),
                spot_bid: spot.bid,
                spot_ask: spot.ask,
                perp_bid: perp.bid,
                perp_ask: perp.ask,
                funding_rate: opportunity.funding_rate.unwrap_or_default(),
                gross_spread_bps: opportunity.gross_spread_bps,
                net_edge_bps: opportunity.net_edge_bps,
                // 机会模型不携带下次结算时间，模块只能依赖费率与基差。
                next_funding_at: None,
                observed_at: opportunity.observed_at,
                max_notional_usd: opportunity.max_notional_usd,
            })
        })
        .collect();

    let position = record
        .and_then(|record| record.runtime.open.as_ref())
        .map(|open| SandboxPositionSnapshot {
            symbol: open.symbol.clone(),
            notional_usd: open.notional_usd,
            opened_at: open.opened_at,
            held_secs: now
                .signed_duration_since(open.opened_at)
                .num_seconds()
                .max(0) as u64,
            entry_net_edge_bps: open.entry_net_edge_bps,
        });

    SandboxInput {
        abi_version: SANDBOX_ABI_VERSION,
        strategy_id: record.map(|record| record.id).unwrap_or(fallback_strategy_id),
        strategy_version: record.map(|record| record.version).unwrap_or(0),
        now,
        limits: order_limits,
        markets,
        position,
    }
}

fn invoke(
    store: &mut Store<HostState>,
    instance: &Instance,
    input: &SandboxInput,
) -> Result<(), wasmtime::Error> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| wasmtime::Error::msg("模块缺少 memory"))?;
    let payload = serde_json::to_vec(input)
        .map_err(|error| wasmtime::Error::msg(format!("序列化沙箱输入失败: {error}")))?;
    let len = i32::try_from(payload.len())
        .map_err(|_| wasmtime::Error::msg("沙箱输入过大"))?;

    let alloc: TypedFunc<i32, i32> = instance
        .get_typed_func(&mut *store, "alloc")
        .map_err(|_| wasmtime::Error::msg("模块缺少导出函数 alloc(len) -> ptr"))?;
    let ptr = alloc.call(&mut *store, len)?;
    if ptr < 0 {
        return Err(wasmtime::Error::msg("alloc 返回非法指针"));
    }
    memory.write(&mut *store, ptr as usize, &payload)?;

    let evaluate: TypedFunc<(i32, i32), i32> = instance
        .get_typed_func(&mut *store, SANDBOX_ENTRYPOINT)
        .map_err(|_| wasmtime::Error::msg(format!("模块缺少导出函数 {SANDBOX_ENTRYPOINT}")))?;
    let code = evaluate.call(&mut *store, (ptr, len))?;
    if code != 0 {
        return Err(wasmtime::Error::msg(format!(
            "模块返回非零状态码 {code}"
        )));
    }
    Ok(())
}

/// 合并 `emit` 的 JSON 输出与辅助函数提交的决策。
fn combine_output(
    bytes: &[u8],
    mut helper_decisions: Vec<SandboxDecision>,
) -> Result<SandboxOutput, String> {
    if bytes.is_empty() {
        if helper_decisions.is_empty() {
            return Err("模块没有通过 emit 或辅助函数提交任何决策".to_string());
        }
        return Ok(SandboxOutput {
            decisions: helper_decisions,
            logs: Vec::new(),
        });
    }

    let mut output = parse_output(bytes)?;
    output.decisions.append(&mut helper_decisions);
    Ok(output)
}

/// 兼容三种输出格式：完整 SandboxOutput、单条决策、多行 JSON。
fn parse_output(bytes: &[u8]) -> Result<SandboxOutput, String> {
    if bytes.is_empty() {
        return Err("模块没有通过 emit 提交任何决策".to_string());
    }
    if let Ok(output) = serde_json::from_slice::<SandboxOutput>(bytes) {
        return Ok(output);
    }
    if let Ok(decision) = serde_json::from_slice::<SandboxDecision>(bytes) {
        return Ok(SandboxOutput {
            decisions: vec![decision],
            logs: Vec::new(),
        });
    }

    let text = std::str::from_utf8(bytes).map_err(|_| "模块输出不是合法 UTF-8".to_string())?;
    let mut merged = SandboxOutput::default();
    let mut parsed_any = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Ok(output) = serde_json::from_str::<SandboxOutput>(line) {
            merged.decisions.extend(output.decisions);
            merged.logs.extend(output.logs);
            parsed_any = true;
        } else if let Ok(decision) = serde_json::from_str::<SandboxDecision>(line) {
            merged.decisions.push(decision);
            parsed_any = true;
        }
    }
    if parsed_any {
        Ok(merged)
    } else {
        Err("模块输出不是合法的决策 JSON".to_string())
    }
}

fn classify(error: &wasmtime::Error) -> SandboxRunStatus {
    if let Some(trap) = error.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel | Trap::Interrupt => SandboxRunStatus::Timeout,
            _ => SandboxRunStatus::Error,
        };
    }
    let text = error.to_string();
    if text.contains("fuel") || text.contains("epoch") {
        SandboxRunStatus::Timeout
    } else {
        SandboxRunStatus::Error
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// 异步入口：编译与执行都放在阻塞线程池，避免卡住行情循环。
pub async fn evaluate(
    artifact: SandboxArtifact,
    wasm: Vec<u8>,
    input: SandboxInput,
    limits: SandboxLimits,
) -> SandboxRunReport {
    let fallback = artifact.clone();
    tokio::task::spawn_blocking(move || run_blocking(&artifact, &wasm, &input, &limits))
        .await
        .unwrap_or_else(|error| {
            SandboxRunReport::failed(
                &fallback,
                &limits,
                SandboxRunStatus::Error,
                format!("沙箱执行线程异常退出: {error}"),
                0,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::{
        SandboxMarketSnapshot, SandboxOrderLimits, SandboxPositionSnapshot,
    };
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    /// 最小合规模块：读取输入长度，输出一条 hold 决策。
    const HOLD_WAT: &str = r#"
(module
  (import "arbpilot" "emit" (func $emit (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"decisions\":[{\"action\":\"hold\",\"reason\":\"test\"}]}")
  (func (export "abi_version") (result i32) i32.const 1)
  (func (export "alloc") (param i32) (result i32) i32.const 4096)
  (func (export "evaluate") (param i32 i32) (result i32)
    i32.const 1024
    i32.const 49
    call $emit
    drop
    i32.const 0))
"#;

    /// 使用结构化辅助函数的模块：不自己拼 JSON，直接读取行情并提交意图。
    const HELPER_WAT: &str = r#"
(module
  (import "arbpilot" "markets" (func $markets (result i32)))
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "BTCUSDT")
  (func (export "abi_version") (result i32) i32.const 1)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (if (i32.gt_s (call $markets) (i32.const 0))
      (then
        (if (f64.gt (call $market (i32.const 0) (i32.const 4)) (f64.const 0.0003))
          (then
            (drop (call $open (i32.const 1024) (i32.const 7) (f64.const 500)))))))
    (drop (call $hold))
    i32.const 0))
"#;

    /// 死循环模块：用于验证燃料/超时中断。
    const LOOP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "abi_version") (result i32) i32.const 1)
  (func (export "alloc") (param i32) (result i32) i32.const 4096)
  (func (export "evaluate") (param i32 i32) (result i32)
    (loop $forever br $forever)
    i32.const 0))
"#;

    fn artifact(bytes: &[u8]) -> SandboxArtifact {
        SandboxArtifact {
            id: Uuid::new_v4(),
            name: "test".into(),
            sha256: hash_wasm(bytes),
            size_bytes: bytes.len(),
            abi_version: Some(SANDBOX_ABI_VERSION),
            source: arb_core::SandboxArtifactSource::BuiltIn,
            notes: None,
            created_at: Utc::now(),
        }
    }

    fn input() -> SandboxInput {
        SandboxInput {
            abi_version: SANDBOX_ABI_VERSION,
            strategy_id: Uuid::new_v4(),
            strategy_version: 1,
            now: Utc::now(),
            limits: SandboxOrderLimits {
                max_notional_usd: dec!(1000),
                max_leverage: dec!(1),
                max_slippage_bps: dec!(10),
                max_unhedged_secs: 3,
            },
            markets: vec![SandboxMarketSnapshot {
                symbol: "BTCUSDT".into(),
                spot_venue: "binance-spot".into(),
                perp_venue: "binance".into(),
                spot_bid: dec!(100),
                spot_ask: dec!(101),
                perp_bid: dec!(100),
                perp_ask: dec!(101),
                funding_rate: dec!(0.0005),
                gross_spread_bps: dec!(5),
                net_edge_bps: dec!(3),
                next_funding_at: None,
                observed_at: Utc::now(),
                max_notional_usd: dec!(1000),
            }],
            position: None::<SandboxPositionSnapshot>,
        }
    }

    #[test]
    fn rejects_modules_importing_wasi() {
        let wat = r#"
(module
  (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "abi_version") (result i32) i32.const 1)
  (func (export "alloc") (param i32) (result i32) i32.const 0)
  (func (export "evaluate") (param i32 i32) (result i32) i32.const 0))
"#;
        let bytes = wat::parse_str(wat).expect("wat 编译");
        let error = inspect(&bytes, &SandboxLimits::default()).expect_err("应拒绝 WASI");
        assert!(error.contains("不允许的依赖"), "{error}");
    }

    #[test]
    fn runs_hold_module() {
        let bytes = wat::parse_str(HOLD_WAT).expect("wat 编译");
        assert_eq!(inspect(&bytes, &SandboxLimits::default()).unwrap(), 1);
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.fuel_consumed > 0);
    }

    #[test]
    fn runs_helper_module_without_emit() {
        let bytes = wat::parse_str(HELPER_WAT).expect("wat 编译");
        assert_eq!(inspect(&bytes, &SandboxLimits::default()).unwrap(), 1);
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::Open { symbol, .. } if symbol == "BTCUSDT"
        )));
    }

    #[test]
    fn stops_infinite_loop() {
        let bytes = wat::parse_str(LOOP_WAT).expect("wat 编译");
        let artifact = artifact(&bytes);
        let limits = SandboxLimits {
            max_fuel: 200_000,
            timeout_ms: 200,
            ..SandboxLimits::default()
        };
        let report = run_blocking(&artifact, &bytes, &input(), &limits);
        assert_eq!(report.status, SandboxRunStatus::Timeout, "{:?}", report.error);
    }

    #[test]
    fn rejects_oversized_output() {
        let bytes = wat::parse_str(HOLD_WAT).expect("wat 编译");
        let artifact = artifact(&bytes);
        let limits = SandboxLimits {
            max_output_bytes: 8,
            ..SandboxLimits::default()
        };
        let report = run_blocking(&artifact, &bytes, &input(), &limits);
        assert_eq!(report.status, SandboxRunStatus::Invalid);
    }
}
