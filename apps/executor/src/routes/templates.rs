//! 内置策略模板：列出数据库中的模板，并按模板定义创建策略实例。

use arb_core::{
    SandboxBinding, StrategyDefinition, StrategyRecord, StrategySource, StrategyStatus,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    routes::strategies,
    state::AppState,
    templates::{self, StrategyTemplate},
};

/// 模板列表的精简视图：不返回 WAT 源码，避免列表响应过大。
#[derive(Debug, Serialize)]
pub struct TemplateView {
    pub key: String,
    pub name: String,
    pub description: String,
    pub definition: StrategyDefinition,
    pub artifact_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

impl From<StrategyTemplate> for TemplateView {
    fn from(template: StrategyTemplate) -> Self {
        Self {
            key: template.key,
            name: template.name,
            description: template.description,
            definition: template.definition,
            artifact_id: template.artifact_id,
            updated_at: template.updated_at,
        }
    }
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<TemplateView>> {
    let mut templates: Vec<TemplateView> = state
        .store
        .strategy_templates()
        .await
        .into_iter()
        .map(TemplateView::from)
        .collect();
    templates.sort_by(|left, right| left.key.cmp(&right.key));
    Json(templates)
}

#[derive(Debug, Default, Deserialize)]
pub struct InstantiateRequest {
    /// 实例名称；不填时使用模板名称。
    #[serde(default)]
    pub name: Option<String>,
}

/// 按模板创建一条草稿策略：复制定义并绑定模板编译出的沙箱模块。
pub async fn instantiate(
    State(state): State<AppState>,
    Path(key): Path<String>,
    body: Option<Json<InstantiateRequest>>,
) -> Result<(StatusCode, Json<StrategyRecord>), ApiError> {
    let mut template = load(&state, &key).await?;
    let artifact = templates::compile(&state, &template)
        .await
        .map_err(|error| ApiError::Internal(format!("模板 {key} 编译失败: {error}")))?;
    if template.artifact_id != Some(artifact.id) {
        template.artifact_id = Some(artifact.id);
        template.artifact_sha256 = Some(artifact.sha256.clone());
        template.updated_at = Utc::now();
        state.store.save_strategy_template(&template).await;
    }

    let name = body
        .and_then(|Json(body)| body.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| template.name.clone());

    let mut definition = template.definition.clone();
    definition.name = name;

    let mut record =
        StrategyRecord::new(definition, StrategySource::BuiltIn, StrategyStatus::Draft);
    record.sandbox = Some(SandboxBinding::new(&artifact, state.config.sandbox));
    record.runtime.record(
        arb_core::StrategyEventLevel::Info,
        format!(
            "由模板「{}」创建，已绑定模块 {}",
            template.name, artifact.name
        ),
        None,
    );
    record.validation = record.definition.validate(&state.risk_limits());

    strategies::replace(&state, &record);
    strategies::persist(&state, &record).await;

    Ok((StatusCode::CREATED, Json(record)))
}

async fn load(state: &AppState, key: &str) -> Result<StrategyTemplate, ApiError> {
    state
        .store
        .strategy_templates()
        .await
        .into_iter()
        .find(|template| template.key == key)
        .ok_or_else(|| ApiError::NotFound(format!("模板 {key} 不存在")))
}
