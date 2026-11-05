//! WASM 策略沙箱 REST API。
//!
//! 上传的模块只存放在执行器本机；沙箱运行时没有 WASI、文件、网络、
//! 环境变量或交易所密钥，输出也只有开仓/平仓/保持意图。
//! 这些意图在策略运行器里仍需经过统一风控网关。

use arb_core::{
    SandboxArtifact, SandboxArtifactSource, SandboxBinding, SandboxLimits, SandboxRunReport,
    SANDBOX_ABI_VERSION,
};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::{error::ApiError, sandbox, state::AppState};

pub async fn list_artifacts(State(state): State<AppState>) -> Json<Vec<SandboxArtifact>> {
    let mut artifacts = state.store.sandbox_artifacts().await;
    artifacts.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Json(artifacts)
}

#[derive(Debug, Default, Deserialize)]
pub struct UploadQuery {
    pub name: Option<String>,
}

/// 上传 wasm 模块。上传阶段完成编译与导入/导出检查，不合规模块不会入库。
pub async fn upload_artifact(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<SandboxArtifact>), ApiError> {
    let limits = state.config.sandbox;
    if body.is_empty() {
        return Err(ApiError::BadRequest("wasm 文件为空".to_string()));
    }
    if body.len() > limits.max_artifact_bytes {
        return Err(ApiError::BadRequest(format!(
            "wasm 文件超过上限 {} 字节",
            limits.max_artifact_bytes
        )));
    }

    let hash = sandbox::hash_wasm(&body);
    if let Some(existing) = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.sha256 == hash)
    {
        // 同一内容重复上传按幂等处理，避免产生多份绑定。
        return Ok((StatusCode::OK, Json(existing)));
    }

    let abi_version = sandbox::inspect(&body, &limits).map_err(ApiError::BadRequest)?;
    if abi_version != SANDBOX_ABI_VERSION {
        return Err(ApiError::BadRequest(format!(
            "模块 ABI 版本必须为 {SANDBOX_ABI_VERSION}，当前为 {abi_version}"
        )));
    }

    let artifact = SandboxArtifact {
        id: Uuid::new_v4(),
        name: query
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("strategy-{}", &hash[..8])),
        sha256: hash,
        size_bytes: body.len(),
        abi_version: Some(abi_version),
        source: SandboxArtifactSource::Uploaded,
        notes: None,
        created_at: Utc::now(),
    };
    let issues = artifact.validate(&limits);
    if let Some(issue) = issues
        .iter()
        .find(|issue| issue.severity == arb_core::ValidationSeverity::Error)
    {
        return Err(ApiError::BadRequest(format!(
            "{}: {}",
            issue.path, issue.message
        )));
    }

    state
        .store
        .save_sandbox_artifact(&artifact, &body)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(artifact)))
}

#[derive(Debug, Default, Deserialize)]
pub struct DryRunRequest {
    /// 使用该策略的限额、持仓与交易范围构造输入；缺省时用执行器默认限制。
    pub strategy_id: Option<Uuid>,
    /// 只回放指定交易对；缺省时使用全部新鲜资金费率机会。
    pub symbol: Option<String>,
}

/// 用当前行情快照试运行模块，返回燃料、耗时与决策，不产生任何订单。
pub async fn dry_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DryRunRequest>,
) -> Result<Json<SandboxRunReport>, ApiError> {
    let artifact = find_artifact(&state, id).await?;
    let wasm = state
        .store
        .sandbox_module(id)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("沙箱模块 {id} 内容不存在")))?;
    if sandbox::hash_wasm(&wasm) != artifact.sha256 {
        return Err(ApiError::Conflict(
            "模块内容与登记的 sha256 不一致，已拒绝执行".to_string(),
        ));
    }

    let record = match body.strategy_id {
        Some(strategy_id) => Some(
            state
                .strategies
                .read()
                .expect("策略锁不可用")
                .iter()
                .find(|record| record.id == strategy_id)
                .cloned()
                .ok_or_else(|| ApiError::NotFound(format!("策略 {strategy_id} 不存在")))?,
        ),
        None => None,
    };

    let limits = record
        .as_ref()
        .and_then(|record| record.sandbox.as_ref())
        .map(|binding| binding.limits)
        .unwrap_or(state.config.sandbox);
    let input = sandbox::build_input(
        &state,
        record.as_ref(),
        body.symbol.as_deref(),
        body.strategy_id.unwrap_or_else(Uuid::nil),
    );

    Ok(Json(sandbox::evaluate(artifact, wasm, input, limits).await))
}

#[derive(Debug, Deserialize)]
pub struct BindSandboxRequest {
    pub artifact_id: Uuid,
    #[serde(default)]
    pub limits: Option<SandboxLimits>,
}

/// 把模块绑定到策略。运行中的策略必须先停止，避免中途替换决策逻辑。
pub async fn bind(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
    Json(body): Json<BindSandboxRequest>,
) -> Result<Json<arb_core::StrategyRecord>, ApiError> {
    let artifact = find_artifact(&state, body.artifact_id).await?;
    let limits = body.limits.unwrap_or(state.config.sandbox);
    if limits.has_errors() {
        let issue = limits
            .validate()
            .into_iter()
            .find(|issue| {
                issue.severity == arb_core::ValidationSeverity::Error
            })
            .expect("已有错误");
        return Err(ApiError::BadRequest(format!(
            "{}: {}",
            issue.path, issue.message
        )));
    }

    let mut record = {
        let strategies = state.strategies.read().expect("策略锁不可用");
        strategies
            .iter()
            .find(|record| record.id == strategy_id)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("策略 {strategy_id} 不存在")))?
    };
    if record.status == arb_core::StrategyStatus::Running {
        return Err(ApiError::Conflict(
            "请先停止策略再绑定或切换沙箱模块".to_string(),
        ));
    }

    let binding = SandboxBinding::new(&artifact, limits);
    record.sandbox = Some(binding);
    record.runtime.record(
        arb_core::StrategyEventLevel::Info,
        format!(
            "已绑定沙箱模块 {}（{}），输出仍需通过统一风控网关",
            artifact.name,
            &artifact.sha256[..12]
        ),
        None,
    );
    record.updated_at = Utc::now();

    replace_strategy(&state, &record);
    state.store.save_strategy(&record).await;
    Ok(Json(record))
}

pub async fn unbind(
    State(state): State<AppState>,
    Path(strategy_id): Path<Uuid>,
) -> Result<Json<arb_core::StrategyRecord>, ApiError> {
    let mut record = {
        let strategies = state.strategies.read().expect("策略锁不可用");
        strategies
            .iter()
            .find(|record| record.id == strategy_id)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("策略 {strategy_id} 不存在")))?
    };
    if record.status == arb_core::StrategyStatus::Running {
        return Err(ApiError::Conflict(
            "请先停止策略再解绑沙箱模块".to_string(),
        ));
    }

    record.sandbox = None;
    record.runtime.record(
        arb_core::StrategyEventLevel::Info,
        "已解绑沙箱模块，恢复内置资金费率规则".to_string(),
        None,
    );
    record.updated_at = Utc::now();

    replace_strategy(&state, &record);
    state.store.save_strategy(&record).await;
    Ok(Json(record))
}

async fn find_artifact(state: &AppState, id: Uuid) -> Result<SandboxArtifact, ApiError> {
    state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("沙箱模块 {id} 不存在")))
}

fn replace_strategy(state: &AppState, record: &arb_core::StrategyRecord) {
    let mut strategies = state.strategies.write().expect("策略锁不可用");
    match strategies
        .iter_mut()
        .find(|existing| existing.id == record.id)
    {
        Some(existing) => *existing = record.clone(),
        None => strategies.push(record.clone()),
    }
}
