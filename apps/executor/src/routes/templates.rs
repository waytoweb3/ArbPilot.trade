//! 内置策略模板：列出数据库中的模板，并按模板定义创建策略实例。

use arb_core::{SkillBinding, StrategyDefinition, StrategyRecord, StrategySource, StrategyStatus};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{error::ApiError, routes::strategies, state::AppState, templates::StrategyTemplate};

/// 模板列表的精简视图：只返回定义与说明，实例化由技能运行时接管。
#[derive(Debug, Serialize)]
pub struct TemplateView {
    pub key: String,
    pub name: String,
    pub description: String,
    pub definition: StrategyDefinition,
    pub updated_at: DateTime<Utc>,
}

impl From<StrategyTemplate> for TemplateView {
    fn from(template: StrategyTemplate) -> Self {
        Self {
            key: template.key,
            name: template.name,
            description: template.description,
            definition: template.definition,
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

/// 按模板创建一条草稿策略：复制定义并绑定模板标识，由技能运行时解释执行。
pub async fn instantiate(
    State(state): State<AppState>,
    Path(key): Path<String>,
    body: Option<Json<InstantiateRequest>>,
) -> Result<(StatusCode, Json<StrategyRecord>), ApiError> {
    let template = load(&state, &key).await?;

    let name = body
        .and_then(|Json(body)| body.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| template.name.clone());
    // 同一模板可以创建多条策略（例如分别跟随不同交易所的上币公告），重名时自动编号。
    let name = unique_name(&state, name);

    let mut definition = template.definition.clone();
    definition.name = name;

    let mut record =
        StrategyRecord::new(definition, StrategySource::BuiltIn, StrategyStatus::Draft);
    // 内置模板同样以技能库版本交付：绑定具体版本与规则哈希，运行时按冻结规则执行。
    record.skill = Some(
        match crate::templates::builtin_version(&state, &template.key).await {
            Some(version) => SkillBinding {
                template_key: template.key.clone(),
                // 上币公告跟随继续跟随本机监听设置，这里只登记来源版本。
                settings: if template.key == "listing-follow" {
                    None
                } else {
                    serde_json::to_value(crate::skill_library::normalize_settings(
                        &version.settings,
                        &version.params_schema,
                    ))
                    .ok()
                },
                skill_id: Some(version.skill_id),
                skill_version: Some(version.version),
                skill_sha256: Some(version.logic_sha256.clone()),
                params_schema: serde_json::to_value(&version.params_schema).ok(),
                param_constraints: None,
            },
            None => SkillBinding {
                template_key: template.key.clone(),
                ..SkillBinding::default()
            },
        },
    );
    record.runtime.record(
        arb_core::StrategyEventLevel::Info,
        format!("由模板「{}」创建，已启用技能参数运行时", template.name),
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

/// 已存在同名策略时依次追加 ` 2`、` 3`…，让同一模板可以创建多条实例。
fn unique_name(state: &AppState, base: String) -> String {
    let existing: Vec<String> = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .map(|record| record.definition.name.clone())
        .collect();
    if !existing.iter().any(|name| name == &base) {
        return base;
    }
    // 名称本身带编号时（如「上币公告跟随 2」）先去掉编号再递增，避免出现「2 2」。
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ');
    let stem = if stem.is_empty() { base.as_str() } else { stem };
    for index in 2.. {
        let candidate = format!("{stem} {index}");
        if !existing.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    unreachable!("名称编号始终会找到一个空闲值")
}
