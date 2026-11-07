//! WASM + Wasmtime 策略沙箱。
//!
//! 隔离边界：
//! - 不链接 WASI，模块无法读写文件、发起网络请求或读取环境变量；
//! - 只导入 `arbpilot` 命名空间下的 emit/log 与结构化行情读取、决策辅助函数；
//! - 通过燃料限制、epoch 超时、线性内存上限与输出大小上限约束资源；
//! - 输入只有确定性行情快照，输出只有开仓/平仓/挂单/撤单/保持意图；
//! - 本模块不持有交易所密钥，也没有下单能力；意图必须交给统一风控网关。

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
    time::{Duration, Instant},
};

use arb_core::{
    DataRules, Instrument, SandboxArtifact, SandboxCandle, SandboxDecision,
    SandboxEvent, SandboxInput, SandboxLimits, SandboxMarketSnapshot, SandboxOrderLimits,
    SandboxOutput, SandboxPositionSnapshot, SandboxRunReport, SandboxRunStatus,
    SANDBOX_ABI_VERSION, SANDBOX_CANCEL, SANDBOX_CANDLE, SANDBOX_CANDLES, SANDBOX_CLOSE,
    SANDBOX_EMIT, SANDBOX_ENTRYPOINT, SANDBOX_EVENT, SANDBOX_EVENTS, SANDBOX_EVENT_TEXT,
    SANDBOX_HOLD, SANDBOX_IMPORT_NAMESPACE, SANDBOX_INDICATOR, SANDBOX_LIMIT, SANDBOX_LOG,
    SANDBOX_MARKET, SANDBOX_MARKETS, SANDBOX_MARKET_SYMBOL, SANDBOX_OPEN, SANDBOX_OPEN_PAIR,
    SANDBOX_POSITION_ENTRY_PRICE, SANDBOX_POSITION_HELD_BARS, SANDBOX_POSITION_HELD_SECS,
    SANDBOX_POSITION_NOTIONAL, SANDBOX_POSITION_PRESENT, SANDBOX_POSITION_SIDE,
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
/// 单次评估最多注入的事件条数，避免输入过大。
const MAX_SANDBOX_EVENTS: usize = 16;

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
/// 快照只包含行情、K 线与持仓的数值，不含账户、凭证或任何可写能力；静态检查
/// 阶段（无输入）使用空快照，所有读取函数安全返回 0。
#[derive(Debug, Default)]
struct InputSnapshot {
    markets: Vec<SandboxMarketSnapshot>,
    position: Option<SandboxPositionSnapshot>,
    events: Vec<SandboxEvent>,
}

impl InputSnapshot {
    fn from_input(input: &SandboxInput) -> Self {
        Self {
            markets: input.markets.clone(),
            position: input.position.clone(),
            events: input.events.clone(),
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
    const ALLOWED: [&str; 23] = [
        SANDBOX_EMIT,
        SANDBOX_LOG,
        SANDBOX_MARKETS,
        SANDBOX_MARKET,
        SANDBOX_MARKET_SYMBOL,
        SANDBOX_CANDLES,
        SANDBOX_CANDLE,
        SANDBOX_INDICATOR,
        SANDBOX_EVENTS,
        SANDBOX_EVENT,
        SANDBOX_EVENT_TEXT,
        SANDBOX_POSITION_PRESENT,
        SANDBOX_POSITION_NOTIONAL,
        SANDBOX_POSITION_HELD_SECS,
        SANDBOX_POSITION_HELD_BARS,
        SANDBOX_POSITION_SIDE,
        SANDBOX_POSITION_ENTRY_PRICE,
        SANDBOX_OPEN,
        SANDBOX_OPEN_PAIR,
        SANDBOX_CLOSE,
        SANDBOX_LIMIT,
        SANDBOX_CANCEL,
        SANDBOX_HOLD,
    ];
    for import in module.imports() {
        let allowed =
            import.module() == SANDBOX_IMPORT_NAMESPACE && ALLOWED.contains(&import.name());
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
                let Some(bytes) = memory.data(&caller).get(start..end).map(<[u8]>::to_vec) else {
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
                let Some(bytes) = memory.data(&caller).get(start..end).map(<[u8]>::to_vec) else {
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
                if index < 0 {
                    return 0.0;
                }
                caller
                    .data()
                    .snapshot
                    .markets
                    .get(index as usize)
                    .map(|market| market.field(field))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 market 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_MARKET_SYMBOL,
            |mut caller: Caller<'_, HostState>, index: i32, ptr: i32, cap: i32| -> i32 {
                if index < 0 || ptr < 0 || cap < 0 {
                    return 0;
                }
                let Some(symbol) = caller
                    .data()
                    .snapshot
                    .markets
                    .get(index as usize)
                    .map(|market| market.symbol.clone())
                else {
                    return 0;
                };
                let bytes = symbol.as_bytes();
                if bytes.is_empty() || bytes.len() > cap as usize {
                    return 0;
                }
                let Some(memory) = exported_memory(&mut caller) else {
                    return 0;
                };
                let start = ptr as usize;
                let end = start.saturating_add(bytes.len());
                let Some(target) = memory.data_mut(&mut caller).get_mut(start..end) else {
                    return 0;
                };
                target.copy_from_slice(bytes);
                bytes.len() as i32
            },
        )
        .map_err(|error| format!("注册 market_symbol 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_CANDLES,
            |caller: Caller<'_, HostState>, index: i32| -> i32 {
                if index < 0 {
                    return 0;
                }
                caller
                    .data()
                    .snapshot
                    .markets
                    .get(index as usize)
                    .map(|market| market.candles.len().min(i32::MAX as usize) as i32)
                    .unwrap_or(0)
            },
        )
        .map_err(|error| format!("注册 candles 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_CANDLE,
            |caller: Caller<'_, HostState>, index: i32, offset: i32, field: i32| -> f64 {
                if index < 0 || offset < 0 {
                    return 0.0;
                }
                let Some(market) = caller.data().snapshot.markets.get(index as usize) else {
                    return 0.0;
                };
                // offset 0 表示最新一根，从数组末尾往前取。
                let Some(position) = market.candles.len().checked_sub(offset as usize + 1) else {
                    return 0.0;
                };
                market
                    .candles
                    .get(position)
                    .map(|candle| candle.field(field))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 candle 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_INDICATOR,
            |caller: Caller<'_, HostState>, index: i32, id: i32| -> f64 {
                if index < 0 {
                    return 0.0;
                }
                caller
                    .data()
                    .snapshot
                    .markets
                    .get(index as usize)
                    .map(|market| market.indicators.by_id(id))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 indicator 失败: {error}"))?;
    // 事件读取：目前只有交易所上币公告，按发布时间升序给出最近窗口内的事件。
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_EVENTS,
            |caller: Caller<'_, HostState>| -> i32 {
                caller.data().snapshot.events.len().min(i32::MAX as usize) as i32
            },
        )
        .map_err(|error| format!("注册 events 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_EVENT,
            |caller: Caller<'_, HostState>, index: i32, field: i32| -> f64 {
                if index < 0 {
                    return 0.0;
                }
                caller
                    .data()
                    .snapshot
                    .events
                    .get(index as usize)
                    .map(|event| event.field(field))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 event 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_EVENT_TEXT,
            |mut caller: Caller<'_, HostState>,
             index: i32,
             field: i32,
             ptr: i32,
             cap: i32|
             -> i32 {
                if index < 0 || ptr < 0 || cap < 0 {
                    return 0;
                }
                let Some(text) = caller
                    .data()
                    .snapshot
                    .events
                    .get(index as usize)
                    .map(|event| event.text(field).to_string())
                else {
                    return 0;
                };
                let bytes = text.as_bytes();
                if bytes.is_empty() || bytes.len() > cap as usize {
                    return 0;
                }
                let Some(memory) = exported_memory(&mut caller) else {
                    return 0;
                };
                let start = ptr as usize;
                let end = start.saturating_add(bytes.len());
                let Some(target) = memory.data_mut(&mut caller).get_mut(start..end) else {
                    return 0;
                };
                target.copy_from_slice(bytes);
                bytes.len() as i32
            },
        )
        .map_err(|error| format!("注册 event_text 失败: {error}"))?;
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
                caller
                    .data()
                    .snapshot
                    .position
                    .as_ref()
                    .map(|position| to_f64(position.notional_usd))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 position_notional 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_HELD_SECS,
            |caller: Caller<'_, HostState>| -> i64 {
                caller
                    .data()
                    .snapshot
                    .position
                    .as_ref()
                    .map(|position| position.held_secs.min(i64::MAX as u64) as i64)
                    .unwrap_or(0)
            },
        )
        .map_err(|error| format!("注册 position_held_secs 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_HELD_BARS,
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .snapshot
                    .position
                    .as_ref()
                    .map(|position| position.held_bars.min(i32::MAX as u32) as i32)
                    .unwrap_or(0)
            },
        )
        .map_err(|error| format!("注册 position_held_bars 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_SIDE,
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .snapshot
                    .position
                    .as_ref()
                    .map(|position| match position.side {
                        arb_core::PositionSide::Long => 1,
                        arb_core::PositionSide::Short => -1,
                    })
                    .unwrap_or(0)
            },
        )
        .map_err(|error| format!("注册 position_side 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_POSITION_ENTRY_PRICE,
            |caller: Caller<'_, HostState>| -> f64 {
                caller
                    .data()
                    .snapshot
                    .position
                    .as_ref()
                    .map(|position| to_f64(position.entry_price))
                    .unwrap_or(0.0)
            },
        )
        .map_err(|error| format!("注册 position_entry_price 失败: {error}"))?;

    // 决策辅助：直接提交意图，模块不必拼 JSON。
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_OPEN,
            |mut caller: Caller<'_, HostState>,
             ptr: i32,
             len: i32,
             notional: f64,
             side: i32|
             -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                let side = match side {
                    1 => arb_core::PositionSide::Long,
                    -1 => arb_core::PositionSide::Short,
                    _ => return 1,
                };
                let Some(notional_usd) = Decimal::from_f64(notional) else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::Open {
                        symbol,
                        notional_usd,
                        side,
                        venue: None,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 open 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_OPEN_PAIR,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, notional: f64| -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                let Some(notional_usd) = Decimal::from_f64(notional) else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::OpenPair {
                        symbol,
                        notional_usd,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 open_pair 失败: {error}"))?;
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
            SANDBOX_LIMIT,
            |mut caller: Caller<'_, HostState>,
             ptr: i32,
             len: i32,
             price: f64,
             notional: f64,
             side: i32|
             -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                let side = match side {
                    1 => arb_core::PositionSide::Long,
                    -1 => arb_core::PositionSide::Short,
                    _ => return 1,
                };
                let (Some(price), Some(notional_usd)) =
                    (Decimal::from_f64(price), Decimal::from_f64(notional))
                else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::PlaceLimit {
                        symbol,
                        side,
                        // f64 → Decimal 会带上二进制尾差，统一收敛到 8 位小数。
                        price: price.round_dp(8).normalize(),
                        notional_usd: notional_usd.round_dp(2).normalize(),
                        venue: None,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 limit 失败: {error}"))?;
    linker
        .func_wrap(
            SANDBOX_IMPORT_NAMESPACE,
            SANDBOX_CANCEL,
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let Some(symbol) = read_guest_string(&mut caller, ptr, len) else {
                    return 1;
                };
                push_decision(
                    caller.data_mut(),
                    SandboxDecision::Cancel {
                        symbol,
                        reason: None,
                    },
                )
            },
        )
        .map_err(|error| format!("注册 cancel 失败: {error}"))?;
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
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
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

    let (mut store, instance) =
        match instantiate_with_snapshot(&module, limits, InputSnapshot::from_input(input)) {
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

/// 把当前行情、K 线与持仓转成确定性输入。模块看不到账户、密钥或任何凭证。
pub async fn build_input(
    state: &AppState,
    record: Option<&arb_core::StrategyRecord>,
    symbol: Option<&str>,
    fallback_strategy_id: uuid::Uuid,
) -> SandboxInput {
    let now = chrono::Utc::now();
    let (venues, symbols, order_limits, data) = match record {
        Some(record) => (
            record.definition.universe.venues.clone(),
            record.definition.universe.symbols.clone(),
            SandboxOrderLimits {
                max_notional_usd: record
                    .definition
                    .risk
                    .max_notional_usd
                    .min(state.risk_limits().max_order_notional_usd),
                max_leverage: record.definition.execution.leverage,
                max_slippage_bps: record.definition.execution.max_slippage_bps,
                max_unhedged_secs: record.definition.risk.max_unhedged_seconds,
            },
            record.definition.data.clone(),
        ),
        None => (
            vec![
                DEFAULT_SPOT_VENUE.to_string(),
                DEFAULT_PERP_VENUE.to_string(),
            ],
            state.config.market_symbols.clone(),
            SandboxOrderLimits {
                max_notional_usd: state.risk_limits().max_order_notional_usd,
                max_leverage: state.risk_limits().max_leverage,
                max_slippage_bps: Decimal::from(10),
                max_unhedged_secs: 3,
            },
            DataRules::default(),
        ),
    };

    // 策略定义可能不限制交易对，此时回退到执行器的全局交易对列表。
    let symbols = if symbols.is_empty() {
        state.config.market_symbols.clone()
    } else {
        symbols
    };
    // 资金费率直接读取最新入库值，不依赖已被移除的现货-永续机会。
    let mut funding_rates: HashMap<(String, String), Decimal> = HashMap::new();
    for venue in &venues {
        match state.store.funding_latest(venue, &symbols).await {
            Ok(rows) => {
                for row in rows {
                    funding_rates.insert((row.venue, row.symbol), row.funding_rate);
                }
            }
            Err(error) => tracing::warn!(%error, venue, "读取策略资金费率失败"),
        }
    }
    let mut markets = Vec::with_capacity(venues.len() * symbols.len());
    for venue in &venues {
        for market_symbol in &symbols {
            if let Some(filter) = symbol {
                if !filter.eq_ignore_ascii_case(market_symbol) {
                    continue;
                }
            }
            let Some(quote) = state.market.quote(venue, market_symbol) else {
                continue;
            };
            let instrument = instrument_for(venue);
            let candles = match state
                .market
                .candles(venue, market_symbol, &data.timeframe, data.lookback)
                .await
            {
                Ok(candles) => candles
                    .into_iter()
                    .map(|candle| SandboxCandle {
                        open_time: candle.open_time,
                        open: candle.open,
                        high: candle.high,
                        low: candle.low,
                        close: candle.close,
                        volume: candle.volume,
                    })
                    .collect(),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        venue,
                        symbol = market_symbol.as_str(),
                        "拉取策略 K 线失败，指标不可用"
                    );
                    Vec::new()
                }
            };
            let indicators = arb_core::indicators::compute(&candles, &data);
            let funding_rate = (instrument == Instrument::Perp)
                .then(|| {
                    funding_rates
                        .get(&(venue.clone(), market_symbol.clone()))
                        .copied()
                })
                .flatten();

            markets.push(SandboxMarketSnapshot {
                symbol: market_symbol.clone(),
                venue: venue.clone(),
                instrument,
                bid: quote.bid,
                ask: quote.ask,
                last: None,
                funding_rate,
                next_funding_at: None,
                max_notional_usd: order_limits.max_notional_usd,
                observed_at: quote.observed_at,
                candles,
                indicators,
            });
        }
    }

    let position = record
        .and_then(|record| record.runtime.open.as_ref())
        .map(|open| {
            let fallback_venue = record
                .and_then(|record| record.definition.trade_venue())
                .unwrap_or(DEFAULT_PERP_VENUE);
            // 配对持仓以永续腿代表方向；单腿持仓读取自己的交易所。
            let venue = if open.is_pair() {
                open.short_venue.as_str()
            } else {
                open.venue.as_deref().unwrap_or(fallback_venue)
            };
            let market = markets
                .iter()
                .find(|market| market.venue == venue && market.symbol == open.symbol);
            let held_secs = now
                .signed_duration_since(open.opened_at)
                .num_seconds()
                .max(0) as u64;
            let held_bars = market
                .map(|market| {
                    market
                        .candles
                        .iter()
                        .filter(|candle| candle.open_time > open.opened_at)
                        .count() as u32
                })
                .unwrap_or_else(|| (held_secs / timeframe_secs(&data.timeframe)) as u32);
            let entry_price = open.entry_price.unwrap_or_else(|| {
                market
                    .map(|market| (market.bid + market.ask) / Decimal::from(2))
                    .unwrap_or(Decimal::ZERO)
            });

            SandboxPositionSnapshot {
                symbol: open.symbol.clone(),
                venue: venue.to_string(),
                side: if open.is_pair() {
                    arb_core::PositionSide::Short
                } else {
                    open.side.unwrap_or(arb_core::PositionSide::Long)
                },
                notional_usd: open.notional_usd,
                entry_price,
                opened_at: open.opened_at,
                held_secs,
                held_bars,
                entry_net_edge_bps: open.entry_net_edge_bps,
            }
        });

    let events = record
        .map(|record| collect_events(state, record, now))
        .unwrap_or_default();

    SandboxInput {
        abi_version: SANDBOX_ABI_VERSION,
        strategy_id: record
            .map(|record| record.id)
            .unwrap_or(fallback_strategy_id),
        strategy_version: record.map(|record| record.version).unwrap_or(0),
        now,
        limits: order_limits,
        markets,
        position,
        events,
    }
}

/// 把最近的上币公告转成策略可见的事件；只保留来源、关键词与时间窗口都命中的条目。
fn collect_events(
    state: &AppState,
    record: &arb_core::StrategyRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<SandboxEvent> {
    let data = &record.definition.data;
    let symbols = record.definition.universe.symbols.clone();
    let mut events: Vec<SandboxEvent> = state
        .news
        .snapshot(MAX_SANDBOX_EVENTS * 8)
        .items
        .into_iter()
        .filter(|item| {
            data.event_sources.is_empty()
                || data
                    .event_sources
                    .iter()
                    .any(|source| source.eq_ignore_ascii_case(&item.source))
        })
        .filter(|item| {
            let keywords: Vec<String> = data
                .event_keywords
                .iter()
                .map(|keyword| keyword.trim().to_lowercase())
                .filter(|keyword| !keyword.is_empty())
                .collect();
            keywords.is_empty() || {
                let title = item.title.to_lowercase();
                keywords.iter().any(|keyword| title.contains(keyword))
            }
        })
        .filter_map(|item| {
            let age = now.signed_duration_since(item.published_at).num_seconds();
            if age < 0 || age as u64 > data.event_window_secs {
                return None;
            }
            let symbol = matched_symbol(&item.title, &symbols);
            // 策略声明了交易对时只交付命中其交易对的公告，避免无关噪声。
            if !symbols.is_empty() && symbol.is_none() {
                return None;
            }
            Some(SandboxEvent {
                source: item.source,
                kind: "listing".to_string(),
                title: item.title,
                symbol,
                published_at: item.published_at,
                age_secs: age as u64,
            })
        })
        .collect();
    events.sort_by(|left, right| left.published_at.cmp(&right.published_at));
    if events.len() > MAX_SANDBOX_EVENTS {
        events.drain(..events.len() - MAX_SANDBOX_EVENTS);
    }
    events
}

/// 在公告标题里找策略声明的交易对，返回命中的完整交易对。
fn matched_symbol(title: &str, symbols: &[String]) -> Option<String> {
    let text = title.to_uppercase();
    symbols
        .iter()
        .filter(|symbol| !symbol.trim().is_empty())
        .find(|symbol| {
            let upper = symbol.to_uppercase();
            if contains_token(&text, &upper) {
                return true;
            }
            let base = base_asset(symbol);
            !base.is_empty() && contains_token(&text, &base)
        })
        .cloned()
}

/// 去掉常见计价货币后缀，得到标题里可能出现的币种名。
fn base_asset(symbol: &str) -> String {
    const QUOTES: [&str; 6] = ["USDT", "USDC", "FDUSD", "BUSD", "USD", "PERP"];
    let upper = symbol.trim().to_uppercase();
    for quote in QUOTES {
        if let Some(base) = upper.strip_suffix(quote) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    upper
}

/// 按符号边界匹配，避免 "AI" 命中 "SAIL" 这类误判。
fn contains_token(haystack: &str, token: &str) -> bool {
    haystack.match_indices(token).any(|(index, _)| {
        let before = haystack[..index].chars().next_back();
        let after = haystack[index + token.len()..].chars().next();
        !before.is_some_and(|value| value.is_ascii_alphanumeric())
            && !after.is_some_and(|value| value.is_ascii_alphanumeric())
    })
}

/// 以 `-spot` 结尾的交易所标识走现货，其余按永续处理。
fn instrument_for(venue: &str) -> Instrument {
    if venue.trim().to_ascii_lowercase().ends_with("-spot") {
        Instrument::Spot
    } else {
        Instrument::Perp
    }
}

/// K 线周期换算成秒，用于在缺少 K 线时估算持仓根数。
fn timeframe_secs(timeframe: &str) -> u64 {
    match timeframe {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1_800,
        "1h" => 3_600,
        "4h" => 14_400,
        "1d" => 86_400,
        "1w" => 604_800,
        _ => 300,
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
    let len = i32::try_from(payload.len()).map_err(|_| wasmtime::Error::msg("沙箱输入过大"))?;

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
        return Err(wasmtime::Error::msg(format!("模块返回非零状态码 {code}")));
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
    use arb_core::SandboxIndicators;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    /// 最小合规模块：读取输入长度，输出一条 hold 决策。
    const HOLD_WAT: &str = r#"
(module
  (import "arbpilot" "emit" (func $emit (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"decisions\":[{\"action\":\"hold\",\"reason\":\"test\"}]}")
  (func (export "abi_version") (result i32) i32.const 2)
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
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "BTCUSDT")
  (func (export "abi_version") (result i32) i32.const 2)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (if (i32.gt_s (call $markets) (i32.const 0))
      (then
        (if (f64.gt (call $market (i32.const 0) (i32.const 3)) (f64.const 0.0003))
          (then
            (drop (call $open (i32.const 1024) (i32.const 7) (f64.const 500) (i32.const 1)))))))
    (drop (call $hold))
    i32.const 0))
"#;

    /// 读取 K 线与指标的模块：快均线在慢均线上方时做多。
    const KLINE_WAT: &str = r#"
(module
  (import "arbpilot" "candles" (func $candles (param i32) (result i32)))
  (import "arbpilot" "indicator" (func $indicator (param i32 i32) (result f64)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "BTCUSDT")
  (func (export "abi_version") (result i32) i32.const 2)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (if (i32.gt_s (call $candles (i32.const 0)) (i32.const 0))
      (then
        (if (f64.gt
              (call $indicator (i32.const 0) (i32.const 0))
              (call $indicator (i32.const 0) (i32.const 1)))
          (then
            (drop (call $open (i32.const 1024) (i32.const 7) (f64.const 500) (i32.const 1)))))))
    (drop (call $hold))
    i32.const 0))
"#;

    /// 网格模块：登记一笔待触发限价意图并撤销同交易对的旧挂单。
    const GRID_WAT: &str = r#"
(module
  (import "arbpilot" "limit" (func $limit (param i32 i32 f64 f64 i32) (result i32)))
  (import "arbpilot" "cancel" (func $cancel (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "BTCUSDT")
  (func (export "abi_version") (result i32) i32.const 2)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (drop (call $limit (i32.const 1024) (i32.const 7) (f64.const 9500) (f64.const 500) (i32.const 1)))
    (drop (call $cancel (i32.const 1024) (i32.const 7)))
    i32.const 0))
"#;

    /// 事件模块：最新公告命中交易对时，用公告里的交易对开仓。
    const EVENT_WAT: &str = r#"
(module
  (import "arbpilot" "events" (func $events (result i32)))
  (import "arbpilot" "event" (func $event (param i32 i32) (result f64)))
  (import "arbpilot" "event_text" (func $event_text (param i32 i32 i32 i32) (result i32)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "                        ")
  (func (export "abi_version") (result i32) i32.const 2)
  (func (export "alloc") (param i32) (result i32) i32.const 8192)
  (func (export "evaluate") (param i32 i32) (result i32)
    (local $count i32)
    (local $len i32)
    (local.set $count (call $events))
    (if (i32.gt_s (local.get $count) (i32.const 0))
      (then
        (if (f64.ge (call $event (i32.const 0) (i32.const 2)) (f64.const 1))
          (then
            (local.set $len
              (call $event_text (i32.const 0) (i32.const 3) (i32.const 1024) (i32.const 24)))
            (if (i32.gt_s (local.get $len) (i32.const 0))
              (then
                (drop
                  (call $open
                    (i32.const 1024)
                    (local.get $len)
                    (f64.const 500)
                    (i32.const 1)))))))))
    (drop (call $hold))
    i32.const 0))
"#;

    /// 死循环模块：用于验证燃料/超时中断。
    const LOOP_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "abi_version") (result i32) i32.const 2)
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
                venue: "binance".into(),
                instrument: Instrument::Perp,
                bid: dec!(100),
                ask: dec!(101),
                last: None,
                funding_rate: Some(dec!(0.0005)),
                next_funding_at: None,
                max_notional_usd: dec!(1000),
                observed_at: Utc::now(),
                candles: vec![SandboxCandle {
                    open_time: Utc::now() - chrono::Duration::minutes(5),
                    open: dec!(100),
                    high: dec!(102),
                    low: dec!(99),
                    close: dec!(101),
                    volume: dec!(10),
                }],
                indicators: SandboxIndicators {
                    sma_fast: Some(dec!(101)),
                    sma_slow: Some(dec!(100)),
                    ..SandboxIndicators::default()
                },
            }],
            position: None,
            events: Vec::new(),
        }
    }

    #[test]
    fn rejects_modules_importing_wasi() {
        let wat = r#"
(module
  (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "abi_version") (result i32) i32.const 2)
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
        assert_eq!(
            inspect(&bytes, &SandboxLimits::default()).unwrap(),
            SANDBOX_ABI_VERSION
        );
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.fuel_consumed > 0);
    }

    #[test]
    fn runs_helper_module_without_emit() {
        let bytes = wat::parse_str(HELPER_WAT).expect("wat 编译");
        assert_eq!(
            inspect(&bytes, &SandboxLimits::default()).unwrap(),
            SANDBOX_ABI_VERSION
        );
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::Open { symbol, .. } if symbol == "BTCUSDT"
        )));
    }

    #[test]
    fn reads_candles_and_indicators() {
        let bytes = wat::parse_str(KLINE_WAT).expect("wat 编译");
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::Open {
                side: arb_core::PositionSide::Long,
                ..
            }
        )));
    }

    #[test]
    fn submits_limit_and_cancel_decisions() {
        let bytes = wat::parse_str(GRID_WAT).expect("wat 编译");
        let artifact = artifact(&bytes);
        let report = run_blocking(&artifact, &bytes, &input(), &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::PlaceLimit {
                symbol,
                side: arb_core::PositionSide::Long,
                price,
                ..
            } if symbol == "BTCUSDT" && *price == dec!(9500)
        )));
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::Cancel { symbol, .. } if symbol == "BTCUSDT"
        )));
    }

    /// 事件导入：有命中交易对的公告才开仓，没有公告时保持空仓。
    #[test]
    fn reads_events_and_opens_on_matched_listing() {
        let bytes = wat::parse_str(EVENT_WAT).expect("wat 编译");
        let artifact = artifact(&bytes);

        let mut with_event = input();
        with_event.events = vec![SandboxEvent {
            source: "binance".to_string(),
            kind: "listing".to_string(),
            title: "Binance Will List XYZ".to_string(),
            symbol: Some("XYZUSDT".to_string()),
            published_at: Utc::now(),
            age_secs: 30,
        }];
        let report = run_blocking(&artifact, &bytes, &with_event, &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(report.decisions.iter().any(|decision| matches!(
            decision,
            SandboxDecision::Open { symbol, .. } if symbol == "XYZUSDT"
        )));

        let empty = input();
        let report = run_blocking(&artifact, &bytes, &empty, &SandboxLimits::default());
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(!report
            .decisions
            .iter()
            .any(|decision| matches!(decision, SandboxDecision::Open { .. })));
    }

    #[test]
    fn matches_event_symbols_on_word_boundaries() {
        let symbols = vec!["BTCUSDT".to_string(), "AIUSDT".to_string()];
        assert_eq!(
            matched_symbol("Binance Will List BTC (BTC)", &symbols),
            Some("BTCUSDT".to_string())
        );
        // 子串命中不算：SAIL 里包含 AI。
        assert_eq!(matched_symbol("Binance Will List SAIL", &symbols), None);
        assert_eq!(base_asset("BTCUSDT"), "BTC");
        assert_eq!(base_asset("ETHFDUSD"), "ETH");
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
        assert_eq!(
            report.status,
            SandboxRunStatus::Timeout,
            "{:?}",
            report.error
        );
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
