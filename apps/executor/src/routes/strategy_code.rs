//! 策略代码 REST API：AI 生成、查看、修改、编译与部署。
//!
//! 代码只描述「看到行情后输出什么意图」，与手写模块共用同一套沙箱检查；
//! 部署只写入沙箱绑定，运行时的每笔订单仍必须经过统一风控网关。

use arb_core::{
    SandboxArtifact, SandboxArtifactSource, SandboxBinding, StrategyCode, StrategyCodeOrigin,
    StrategyEventLevel, StrategyRecord, StrategyStatus, ValidationSeverity,
};
use axum::{extract::Path, extract::State, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::ApiError, llm, sandbox, state::AppState};

use super::strategies;

/// 尚未写代码时返回的模板，演示如何读取行情并提交意图。
pub const CODE_TEMPLATE: &str = r#";; ArbPilot 策略代码模板（WebAssembly Text）
;; 沙箱只提供行情、K 线、指标读取与 open/open_pair/close/hold；
;; 没有文件、网络、密钥或下单能力，所有意图仍会经过统一风控网关。
(module
  (import "arbpilot" "markets" (func $markets (result i32)))
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "candles" (func $candles (param i32) (result i32)))
  (import "arbpilot" "indicator" (func $indicator (param i32 i32) (result f64)))
  (import "arbpilot" "position_present" (func $position_present (result i32)))
  (import "arbpilot" "position_held_secs" (func $position_held_secs (result i64)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "close" (func $close (param i32 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))

  (memory (export "memory") 1)
  ;; 示例交易对；请改成策略 universe 中的符号，并同步调整长度。
  (data (i32.const 1024) "BTCUSDT")

  ;; 最长持仓时间：86400 秒 = 24 小时。
  (global $max_holding_secs i64 (i64.const 86400))

  (func (export "abi_version") (result i32) i32.const 2)

  ;; alloc 需要保证主机写入输入 JSON 时有足够内存，且不覆盖 data 段。
  (global $heap (mut i32) (i32.const 8192))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    local.get $len
    i32.const 65536
    i32.div_u
    i32.const 1
    i32.add
    memory.grow
    drop
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr)

  (func (export "evaluate") (param $ptr i32) (param $len i32) (result i32)
    ;; 已有持仓：超过最长持仓时间则平仓，否则保持。
    (if (i32.ne (call $position_present) (i32.const 0))
      (then
        (if (i64.ge_s (call $position_held_secs) (global.get $max_holding_secs))
          (then
            (drop (call $close (i32.const 1024) (i32.const 7)))))
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 没有持仓：快均线在慢均线上方（趋势向上）时按市场上限做多。
    ;; indicator 编号 0 = 快均线、1 = 慢均线；数据不足时返回 0。
    ;; market 字段 5 = max_notional_usd；open 最后一个参数 1 = 做多、-1 = 做空。
    (if (i32.gt_s (call $candles (i32.const 0)) (i32.const 0))
      (then
        (if (f64.gt
              (call $indicator (i32.const 0) (i32.const 0))
              (call $indicator (i32.const 0) (i32.const 1)))
          (then
            (drop
              (call $open
                (i32.const 1024)
                (i32.const 7)
                (call $market (i32.const 0) (i32.const 5))
                (i32.const 1)))))))

    (drop (call $hold))
    i32.const 0))
"#;

#[derive(Debug, Serialize)]
pub struct CodeView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<StrategyCode>,
    pub template: String,
}

/// 读取策略源码；尚未生成时同时返回模板。
pub async fn get_code(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CodeView>, ApiError> {
    strategies::find(&state, id)?;
    let code = state.store.strategy_code(id).await;
    Ok(Json(CodeView {
        code,
        template: CODE_TEMPLATE.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SaveCodeRequest {
    pub source: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// 保存用户编辑后的源码。运行中的策略必须先停止，避免热替换决策逻辑。
pub async fn save_code(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveCodeRequest>,
) -> Result<Json<StrategyCode>, ApiError> {
    let mut record = strategies::find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再修改代码".to_string()));
    }

    let existing = state.store.strategy_code(id).await;
    let source_changed = existing
        .as_ref()
        .map(|code| code.source != body.source)
        .unwrap_or(true);
    let code = match existing {
        Some(mut code) => {
            if source_changed {
                // 源码变化后旧的编译产物不再对应当前代码，必须重新编译与部署。
                code.artifact_id = None;
                code.artifact_sha256 = None;
                code.deployed_at = None;
            }
            code.source = body.source;
            if body.notes.is_some() {
                code.notes = body.notes;
            }
            code.updated_at = Utc::now();
            code
        }
        None => {
            let mut code = StrategyCode::new(id, body.source, StrategyCodeOrigin::User);
            code.notes = body.notes;
            code
        }
    };

    let issues = code.validate();
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return Err(ApiError::BadRequest(format!(
            "{}: {}",
            issue.path, issue.message
        )));
    }

    state.store.save_strategy_code(&code).await;
    if source_changed {
        record.runtime.record(
            StrategyEventLevel::Info,
            "策略代码已更新，需重新编译并部署后才会生效".to_string(),
            None,
        );
        record.updated_at = Utc::now();
        strategies::replace(&state, &record);
        strategies::persist(&state, &record).await;
    }

    Ok(Json(code))
}

#[derive(Debug, Deserialize)]
pub struct GenerateCodeRequest {
    /// 自然语言描述，例如「当资金费率高于 5 bps 时开仓，持仓超过 8 小时平仓」。
    pub prompt: String,
    /// 多轮对话历史；为空时等价于一次性描述。
    #[serde(default)]
    pub history: Vec<llm::ChatTurn>,
}

/// 根据自然语言描述生成源码并保存；不会自动编译或部署。
pub async fn generate_code(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<GenerateCodeRequest>,
) -> Result<Json<StrategyCode>, ApiError> {
    let record = strategies::find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再生成代码".to_string()));
    }
    let prompt = body.prompt.trim();
    if prompt.is_empty() {
        return Err(ApiError::BadRequest("请填写策略描述".to_string()));
    }

    // 带上已有代码，让 AI 在现有逻辑上继续修改，而不是每次从零重写。
    let current = state.store.strategy_code(id).await;
    let generated = llm::generate(
        state.config.as_ref(),
        prompt,
        &describe(&record),
        current.as_ref().map(|code| code.source.as_str()),
        &body.history,
    )
    .await
    .map_err(|message| {
        if message.contains("ARBPILOT_LLM_API_KEY") {
            ApiError::BadRequest(message)
        } else {
            ApiError::Internal(message)
        }
    })?;

    let mut code = StrategyCode::new(id, generated.source, StrategyCodeOrigin::Ai);
    code.prompt = Some(prompt.to_string());
    code.notes = generated.notes;

    let issues = code.validate();
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return Err(ApiError::BadRequest(format!(
            "AI 生成结果未通过校验 {}: {}",
            issue.path, issue.message
        )));
    }

    state.store.save_strategy_code(&code).await;
    Ok(Json(code))
}

/// 编译当前源码为 wasm 模块；只入库，不改变策略绑定。
pub async fn compile_code(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CompileResponse>, ApiError> {
    let record = strategies::find(&state, id)?;
    let mut code = load_code(&state, id).await?;
    let artifact = compile_source(&state, &record, &mut code).await?;
    state.store.save_strategy_code(&code).await;
    Ok(Json(CompileResponse { artifact, code }))
}

/// 编译并部署：把生成的模块绑定到策略，并记录运行事件。
pub async fn deploy_code(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeployResponse>, ApiError> {
    let mut record = strategies::find(&state, id)?;
    if record.status == StrategyStatus::Running {
        return Err(ApiError::Conflict("请先停止策略再部署代码".to_string()));
    }
    let mut code = load_code(&state, id).await?;
    let artifact = compile_source(&state, &record, &mut code).await?;

    record.sandbox = Some(SandboxBinding::new(&artifact, state.config.sandbox));
    record.runtime.record(
        StrategyEventLevel::Info,
        format!(
            "已部署策略代码 {}（{}），输出仍需通过统一风控网关",
            artifact.name,
            &artifact.sha256[..12.min(artifact.sha256.len())]
        ),
        None,
    );
    record.updated_at = Utc::now();

    code.deployed_at = Some(Utc::now());
    code.updated_at = Utc::now();

    strategies::replace(&state, &record);
    strategies::persist(&state, &record).await;
    state.store.save_strategy_code(&code).await;

    Ok(Json(DeployResponse {
        code,
        strategy: record,
    }))
}

#[derive(Debug, Serialize)]
pub struct CompileResponse {
    pub artifact: SandboxArtifact,
    pub code: StrategyCode,
}

#[derive(Debug, Serialize)]
pub struct DeployResponse {
    pub code: StrategyCode,
    pub strategy: StrategyRecord,
}

async fn load_code(state: &AppState, id: Uuid) -> Result<StrategyCode, ApiError> {
    state
        .store
        .strategy_code(id)
        .await
        .ok_or_else(|| ApiError::BadRequest("策略还没有代码，请先生成或保存源码".to_string()))
}

/// 把 WAT 编译成 wasm，完成沙箱静态检查后入库。
async fn compile_source(
    state: &AppState,
    record: &StrategyRecord,
    code: &mut StrategyCode,
) -> Result<SandboxArtifact, ApiError> {
    let issues = code.validate();
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return Err(ApiError::BadRequest(format!(
            "{}: {}",
            issue.path, issue.message
        )));
    }

    let limits = state.config.sandbox;
    let wasm = wat::parse_str(&code.source)
        .map_err(|error| ApiError::BadRequest(format!("WAT 编译失败: {error}")))?;
    if wasm.len() > limits.max_artifact_bytes {
        return Err(ApiError::BadRequest(format!(
            "编译产物 {} 字节，超过沙箱上限 {} 字节",
            wasm.len(),
            limits.max_artifact_bytes
        )));
    }

    let hash = sandbox::hash_wasm(&wasm);
    if let Some(existing) = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.sha256 == hash)
    {
        code.artifact_id = Some(existing.id);
        code.artifact_sha256 = Some(existing.sha256.clone());
        code.updated_at = Utc::now();
        return Ok(existing);
    }

    let abi_version = sandbox::inspect(&wasm, &limits).map_err(ApiError::BadRequest)?;
    let artifact = SandboxArtifact {
        id: Uuid::new_v4(),
        name: format!("{}-v{}-code", record.name(), record.version),
        sha256: hash,
        size_bytes: wasm.len(),
        abi_version: Some(abi_version),
        source: SandboxArtifactSource::Uploaded,
        notes: Some("由策略代码编译生成".to_string()),
        created_at: Utc::now(),
    };
    let issues = artifact.validate(&limits);
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        return Err(ApiError::BadRequest(format!(
            "{}: {}",
            issue.path, issue.message
        )));
    }

    state
        .store
        .save_sandbox_artifact(&artifact, &wasm)
        .await
        .map_err(ApiError::from)?;

    code.artifact_id = Some(artifact.id);
    code.artifact_sha256 = Some(artifact.sha256.clone());
    code.updated_at = Utc::now();
    Ok(artifact)
}

/// 把策略定义压缩成给 AI 的上下文，避免它猜测交易对与阈值。
fn describe(record: &StrategyRecord) -> String {
    let definition = &record.definition;
    let mut lines = vec![
        format!("名称：{}", definition.name),
        format!("类型：{:?}", definition.kind),
        format!("交易所：{}", definition.universe.venues.join(" / ")),
        format!(
            "交易对：{}",
            if definition.universe.symbols.is_empty() {
                "（未限制）".to_string()
            } else {
                definition.universe.symbols.join(", ")
            }
        ),
        format!(
            "入场：funding_spread_min={}, net_edge_min={}",
            definition.entry.funding_spread_min, definition.entry.net_edge_min
        ),
        format!(
            "出场：spread_below={}, max_holding_hours={}",
            definition.exit.spread_below, definition.exit.max_holding_hours
        ),
        format!(
            "风控：max_notional_usd={}, max_unhedged_seconds={}",
            definition.risk.max_notional_usd, definition.risk.max_unhedged_seconds
        ),
    ];
    if let Some(notes) = definition
        .notes
        .as_deref()
        .filter(|notes| !notes.trim().is_empty())
    {
        lines.push(format!("备注：{notes}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_compiles_and_passes_sandbox_checks() {
        let wasm = wat::parse_str(CODE_TEMPLATE).expect("模板应可编译");
        let abi = sandbox::inspect(&wasm, &arb_core::SandboxLimits::default())
            .expect("模板应通过沙箱检查");
        assert_eq!(abi, arb_core::SANDBOX_ABI_VERSION);
    }
}
