//! 技能库：独立于策略实例的 Skill 实体与不可变版本。
//!
//! 作者在技能库里创建 Skill，每次编辑追加一个版本；策略发布时绑定具体版本，
//! 订阅者导入后拿到的是该版本冻结的规则快照。版本一旦写入就不再修改。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::skill::{SkillRule, SkillSettings};

/// 技能列表的文档集合。
pub const SKILLS_COLLECTION: &str = "skills";
/// 技能版本的文档集合；id 为 `{skill_id}:{version}`。
pub const SKILL_VERSIONS_COLLECTION: &str = "skill_versions";

/// 作者侧技能实体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: Uuid,
    pub author_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 最新版本号；`0` 表示尚未创建版本。
    pub latest_version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Skill {
    pub fn new(author_id: Uuid, name: impl Into<String>, description: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            author_id,
            name: name.into(),
            description: description.into(),
            latest_version: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 参数的取值类型。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    #[default]
    Number,
    Percent,
    Integer,
    Boolean,
    Enum {
        options: Vec<String>,
    },
    StringList,
}

/// 参数敏感度：高敏感参数必须由作者给出上下界。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    #[default]
    Low,
    Medium,
    High,
}

/// 一个可配置参数的定义；作用域锚定到来源规则 ID，避免不同规则的同名参数冲突。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamSpec {
    /// 来源规则 ID（`source_rule_id`，缺失时回落 `id`）的字符串形式。
    pub rule_key: String,
    /// 规则内路径，例如 `business.notional_usd`。
    pub path: String,
    pub label: String,
    pub param_type: ParamType,
    /// 默认值；形态与 serde 序列化一致（Decimal 为字符串）。
    pub default: Value,
    pub min: Option<Value>,
    pub max: Option<Value>,
    pub step: Option<Value>,
    pub sensitivity: Sensitivity,
    pub required: bool,
    /// 作者是否允许买家修改；false 表示锁死。
    pub editable: bool,
}

impl ParamSpec {
    pub fn key(&self) -> String {
        format!("{}/{}", self.rule_key, self.path)
    }
}

/// 技能版本的参数空间。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamsSchema {
    pub params: Vec<ParamSpec>,
}

/// 买家提交的一个参数值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamValue {
    pub rule_key: String,
    pub path: String,
    pub value: Value,
}

/// 买家提交的一批参数值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamValues {
    pub values: Vec<ParamValue>,
}

/// 参数空间问题；`rule_key + path` 用于把错误定位到具体字段。
#[derive(Debug, Clone, Serialize)]
pub struct ParamIssue {
    pub rule_key: String,
    pub path: String,
    pub message: String,
}

impl ParamIssue {
    fn new(
        rule_key: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule_key: rule_key.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// 可参数化字段白名单：只暴露六章里影响仓位与节奏的数值项。
const PARAM_FIELDS: &[(&str, &str, ParamType, Sensitivity)] = &[
    (
        "business.notional_usd",
        "单笔名义金额（USD）",
        ParamType::Number,
        Sensitivity::High,
    ),
    (
        "business.leverage",
        "杠杆",
        ParamType::Number,
        Sensitivity::High,
    ),
    (
        "business.max_leverage",
        "有效杠杆上限",
        ParamType::Number,
        Sensitivity::High,
    ),
    (
        "business.risk_reward",
        "风险回报比",
        ParamType::Number,
        Sensitivity::Medium,
    ),
    (
        "business.take_profit_pct",
        "止盈百分比",
        ParamType::Percent,
        Sensitivity::Medium,
    ),
    (
        "business.stop_loss_pct",
        "止损百分比",
        ParamType::Percent,
        Sensitivity::High,
    ),
    (
        "business.max_symbols",
        "最大持仓币种数",
        ParamType::Integer,
        Sensitivity::Medium,
    ),
    (
        "business.max_hold_hours",
        "最长持有小时",
        ParamType::Number,
        Sensitivity::Medium,
    ),
    (
        "business.grid_count",
        "网格档位数",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "business.grid_spacing_pct",
        "网格间距（%）",
        ParamType::Percent,
        Sensitivity::Low,
    ),
    (
        "business.grid_lower_price",
        "网格区间下限",
        ParamType::Number,
        Sensitivity::Medium,
    ),
    (
        "business.grid_upper_price",
        "网格区间上限",
        ParamType::Number,
        Sensitivity::Medium,
    ),
    (
        "cautions.max_notional_usd",
        "单笔金额上限（USD）",
        ParamType::Number,
        Sensitivity::High,
    ),
    (
        "cautions.risk_pct",
        "单笔风险（%）",
        ParamType::Percent,
        Sensitivity::High,
    ),
    (
        "cautions.max_daily_opens",
        "每日最多开仓次数",
        ParamType::Integer,
        Sensitivity::Medium,
    ),
    (
        "cautions.max_positions",
        "最大同时持仓数",
        ParamType::Integer,
        Sensitivity::Medium,
    ),
    (
        "cautions.cooldown_hours",
        "冷却小时数",
        ParamType::Number,
        Sensitivity::Medium,
    ),
    (
        "trigger.lookback",
        "回看 K 线根数",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "trigger.fast",
        "快均线周期",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "trigger.slow",
        "慢均线周期",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "trigger.atr",
        "ATR 周期",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "trigger.channel",
        "唐奇安通道周期",
        ParamType::Integer,
        Sensitivity::Low,
    ),
    (
        "trigger.atr_multiple",
        "ATR 止损倍数",
        ParamType::Number,
        Sensitivity::Medium,
    ),
];

impl ParamsSchema {
    pub fn find(&self, rule_key: &str, path: &str) -> Option<&ParamSpec> {
        self.params
            .iter()
            .find(|spec| spec.rule_key == rule_key && spec.path == path)
    }

    pub fn find_mut(&mut self, rule_key: &str, path: &str) -> Option<&mut ParamSpec> {
        self.params
            .iter_mut()
            .find(|spec| spec.rule_key == rule_key && spec.path == path)
    }

    /// 从一组默认参数推导参数空间：默认全部锁死，等价于「卖配置」。
    pub fn from_settings(settings: &SkillSettings) -> Self {
        let mut params = Vec::new();
        for rule in &settings.rules {
            let rule_key = rule.source_rule_id.unwrap_or(rule.id).to_string();
            let Ok(value) = serde_json::to_value(rule) else {
                continue;
            };
            // 通用表达式程序只暴露程序自身的参数与风控，避免和同步后的六章字段互相覆盖。
            if rule.kind == crate::skill::SkillKind::Program {
                if let Some(program) = rule.program.as_ref() {
                    for (key, param) in &program.params {
                        let label = if param.label.trim().is_empty() {
                            key.clone()
                        } else {
                            param.label.clone()
                        };
                        params.push(ParamSpec {
                            rule_key: rule_key.clone(),
                            path: format!("program.params.{key}.value"),
                            label,
                            param_type: ParamType::Number,
                            default: decimal_value(param.value),
                            min: param.min.map(decimal_value),
                            max: param.max.map(decimal_value),
                            step: param.step.map(decimal_value),
                            sensitivity: Sensitivity::Medium,
                            required: false,
                            editable: param.editable,
                        });
                    }
                    for (path, label, param_type, sensitivity, current) in [
                        (
                            "program.sizing.notional_usd",
                            "单笔名义金额（USD）",
                            ParamType::Number,
                            Sensitivity::High,
                            program.sizing.notional_usd,
                        ),
                        (
                            "program.sizing.risk_pct",
                            "单笔风险（%）",
                            ParamType::Percent,
                            Sensitivity::High,
                            program.sizing.risk_pct,
                        ),
                        (
                            "program.risk.stop_loss_pct",
                            "止损百分比",
                            ParamType::Percent,
                            Sensitivity::High,
                            program.risk.stop_loss_pct,
                        ),
                        (
                            "program.risk.take_profit_pct",
                            "止盈百分比",
                            ParamType::Percent,
                            Sensitivity::Medium,
                            program.risk.take_profit_pct,
                        ),
                        (
                            "program.risk.trailing_stop_pct",
                            "移动止损百分比",
                            ParamType::Percent,
                            Sensitivity::Medium,
                            program.risk.trailing_stop_pct,
                        ),
                        (
                            "program.risk.max_hold_hours",
                            "最长持有小时",
                            ParamType::Number,
                            Sensitivity::Medium,
                            program.risk.max_hold_hours,
                        ),
                        (
                            "program.risk.cooldown_hours",
                            "冷却小时数",
                            ParamType::Number,
                            Sensitivity::Medium,
                            program.risk.cooldown_hours,
                        ),
                    ] {
                        let current = decimal_value(current);
                        params.push(ParamSpec {
                            rule_key: rule_key.clone(),
                            path: path.to_string(),
                            label: label.to_string(),
                            param_type: param_type.clone(),
                            default: current.clone(),
                            min: Some(current.clone()),
                            max: Some(current.clone()),
                            step: None,
                            sensitivity: sensitivity.clone(),
                            required: false,
                            editable: true,
                        });
                    }
                }
                continue;
            }
            for (path, label, param_type, sensitivity) in PARAM_FIELDS {
                let Some(current) = pointer(&value, path) else {
                    continue;
                };
                params.push(ParamSpec {
                    rule_key: rule_key.clone(),
                    path: (*path).to_string(),
                    label: (*label).to_string(),
                    param_type: param_type.clone(),
                    default: current.clone(),
                    // 推导出的空间保守处理：上下界等于当前值，买家不可改。
                    min: Some(current.clone()),
                    max: Some(current.clone()),
                    step: None,
                    sensitivity: sensitivity.clone(),
                    required: false,
                    editable: false,
                });
            }
        }
        Self { params }
    }

    /// 校验参数空间是否自洽，并确认每个路径都能定位到真实字段。
    pub fn validate(&self, settings: &SkillSettings) -> Vec<ParamIssue> {
        let mut issues = Vec::new();
        let mut seen = HashSet::new();
        for spec in &self.params {
            if !seen.insert(spec.key()) {
                issues.push(ParamIssue::new(&spec.rule_key, &spec.path, "参数重复定义"));
                continue;
            }
            let Some(rule) = find_rule(settings, &spec.rule_key) else {
                issues.push(ParamIssue::new(
                    &spec.rule_key,
                    &spec.path,
                    "参数指向的规则不存在",
                ));
                continue;
            };
            let Ok(value) = serde_json::to_value(rule) else {
                continue;
            };
            if pointer(&value, &spec.path).is_none() {
                issues.push(ParamIssue::new(
                    &spec.rule_key,
                    &spec.path,
                    "参数路径无法解析",
                ));
                continue;
            }
            if matches!(spec.param_type, ParamType::Enum { .. }) && spec.default.is_null() {
                issues.push(ParamIssue::new(
                    &spec.rule_key,
                    &spec.path,
                    "枚举参数必须给出默认值",
                ));
            }
            if spec.required && spec.default.is_null() {
                issues.push(ParamIssue::new(
                    &spec.rule_key,
                    &spec.path,
                    "必填参数必须给出默认值",
                ));
            }
            if spec.sensitivity == Sensitivity::High && (spec.min.is_none() || spec.max.is_none()) {
                issues.push(ParamIssue::new(
                    &spec.rule_key,
                    &spec.path,
                    "高敏感参数必须同时设置上下界",
                ));
            }
            if let Some(step_value) = numeric(spec.step.as_ref()) {
                if step_value <= 0.0 {
                    issues.push(ParamIssue::new(
                        &spec.rule_key,
                        &spec.path,
                        "步长必须大于 0",
                    ));
                }
            }
            check_bounds(spec, &mut issues);
        }
        issues
    }

    /// 把作者给出的约束收窄到参数空间内；约束越界视为非法。
    pub fn intersect(&self, constraints: &ParamsSchema) -> Result<ParamsSchema, Vec<ParamIssue>> {
        let mut effective = self.clone();
        let mut issues = Vec::new();
        for constraint in &constraints.params {
            let Some(spec) = effective.find_mut(&constraint.rule_key, &constraint.path) else {
                issues.push(ParamIssue::new(
                    &constraint.rule_key,
                    &constraint.path,
                    "约束了不存在的参数",
                ));
                continue;
            };
            if let (Some(base), Some(limit)) =
                (numeric(spec.min.as_ref()), numeric(constraint.min.as_ref()))
            {
                if limit < base {
                    issues.push(ParamIssue::new(
                        &constraint.rule_key,
                        &constraint.path,
                        "约束下界不能小于参数空间下界",
                    ));
                }
            }
            if let (Some(base), Some(limit)) =
                (numeric(spec.max.as_ref()), numeric(constraint.max.as_ref()))
            {
                if limit > base {
                    issues.push(ParamIssue::new(
                        &constraint.rule_key,
                        &constraint.path,
                        "约束上界不能大于参数空间上界",
                    ));
                }
            }
            if constraint.min.is_some() {
                spec.min = constraint.min.clone();
            }
            if constraint.max.is_some() {
                spec.max = constraint.max.clone();
            }
            if constraint.step.is_some() {
                spec.step = constraint.step.clone();
            }
            clamp_default(spec);
        }
        if issues.is_empty() {
            Ok(effective)
        } else {
            Err(issues)
        }
    }
}

/// 逻辑哈希：只覆盖规则骨架与参数空间，不含买家填的具体值。
pub fn hash_logic(settings: &SkillSettings, schema: &ParamsSchema) -> String {
    #[derive(Serialize)]
    struct Digest<'a> {
        settings: &'a SkillSettings,
        params: &'a ParamsSchema,
    }
    match serde_json::to_vec(&Digest {
        settings,
        params: schema,
    }) {
        Ok(bytes) => hex::encode(Sha256::digest(bytes)),
        Err(error) => {
            tracing::error!(%error, "技能逻辑序列化失败，使用空哈希");
            hex::encode(Sha256::digest(b"{}"))
        }
    }
}

/// 用默认值覆盖所有参数，得到不含买家取值的逻辑基线。
pub fn normalize_settings(settings: &SkillSettings, schema: &ParamsSchema) -> SkillSettings {
    let mut normalized = settings.clone();
    for spec in &schema.params {
        if spec.default.is_null() {
            continue;
        }
        let _ = write_param(&mut normalized, spec, &spec.default);
    }
    normalized
}

/// 把买家填的值应用到逻辑基线上；越界或越权会逐项报错。
pub fn apply_params(
    settings: &SkillSettings,
    schema: &ParamsSchema,
    values: &ParamValues,
) -> Result<SkillSettings, Vec<ParamIssue>> {
    let mut applied = normalize_settings(settings, schema);
    let mut issues = Vec::new();
    for supplied in &values.values {
        let Some(spec) = schema.find(&supplied.rule_key, &supplied.path) else {
            issues.push(ParamIssue::new(
                &supplied.rule_key,
                &supplied.path,
                "参数不在本次交付的参数空间内",
            ));
            continue;
        };
        if !spec.editable {
            issues.push(ParamIssue::new(
                &supplied.rule_key,
                &supplied.path,
                "该参数已被作者锁定，不能修改",
            ));
            continue;
        }
        if let Err(message) = write_param(&mut applied, spec, &supplied.value) {
            issues.push(ParamIssue::new(&spec.rule_key, &spec.path, message));
        }
    }
    for spec in &schema.params {
        if spec.required
            && spec.default.is_null()
            && !values
                .values
                .iter()
                .any(|value| value.rule_key == spec.rule_key && value.path == spec.path)
        {
            issues.push(ParamIssue::new(
                &spec.rule_key,
                &spec.path,
                "必填参数未填写",
            ));
        }
    }
    if issues.is_empty() {
        Ok(applied)
    } else {
        Err(issues)
    }
}

fn find_rule<'a>(settings: &'a SkillSettings, key: &str) -> Option<&'a SkillRule> {
    settings
        .rules
        .iter()
        .find(|rule| rule.source_rule_id.unwrap_or(rule.id).to_string() == key)
}

fn pointer<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    value.pointer(&format!("/{}", path.replace('.', "/")))
}

/// Decimal 的 serde 形态是字符串，参数默认值/上下界保持同一形态。
fn decimal_value(value: Decimal) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn numeric(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

fn check_bounds(spec: &ParamSpec, issues: &mut Vec<ParamIssue>) {
    let default = numeric(Some(&spec.default));
    let min = numeric(spec.min.as_ref());
    let max = numeric(spec.max.as_ref());
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            issues.push(ParamIssue::new(
                &spec.rule_key,
                &spec.path,
                "下界不能大于上界",
            ));
            return;
        }
    }
    if let (Some(value), Some(min)) = (default, min) {
        if value < min {
            issues.push(ParamIssue::new(
                &spec.rule_key,
                &spec.path,
                "默认值小于下界",
            ));
        }
    }
    if let (Some(value), Some(max)) = (default, max) {
        if value > max {
            issues.push(ParamIssue::new(
                &spec.rule_key,
                &spec.path,
                "默认值大于上界",
            ));
        }
    }
}

/// 约束收窄后默认值可能越界，夹回区间内。
fn clamp_default(spec: &mut ParamSpec) {
    let Some(value) = numeric(Some(&spec.default)) else {
        return;
    };
    let mut next = value;
    if let Some(min) = numeric(spec.min.as_ref()) {
        next = next.max(min);
    }
    if let Some(max) = numeric(spec.max.as_ref()) {
        next = next.min(max);
    }
    if (next - value).abs() > f64::EPSILON {
        spec.default = Value::String(next.to_string());
    }
}

/// 按参数类型检查并写回；返回错误消息供接口直接展示。
fn write_param(
    settings: &mut SkillSettings,
    spec: &ParamSpec,
    value: &Value,
) -> Result<(), String> {
    let coerced = coerce(spec, value)?;
    let index = settings
        .rules
        .iter()
        .position(|rule| rule.source_rule_id.unwrap_or(rule.id).to_string() == spec.rule_key)
        .ok_or_else(|| "参数指向的规则不存在".to_string())?;
    let mut raw = serde_json::to_value(&settings.rules[index])
        .map_err(|error| format!("规则序列化失败: {error}"))?;
    {
        let slot = raw
            .pointer_mut(&format!("/{}", spec.path.replace('.', "/")))
            .ok_or_else(|| "参数路径无法解析".to_string())?;
        *slot = coerced;
    }
    let rule: SkillRule =
        serde_json::from_value(raw).map_err(|error| format!("参数值类型不匹配: {error}"))?;
    settings.rules[index] = rule;
    Ok(())
}

fn coerce(spec: &ParamSpec, value: &Value) -> Result<Value, String> {
    match &spec.param_type {
        ParamType::Integer => {
            let number = numeric(Some(value)).ok_or_else(|| "需要整数".to_string())?;
            check_range(spec, number)?;
            Ok(Value::Number(serde_json::Number::from(
                number.round() as i64
            )))
        }
        ParamType::Number | ParamType::Percent => {
            let number = numeric(Some(value)).ok_or_else(|| "需要数字".to_string())?;
            check_range(spec, number)?;
            // Decimal 的 serde 形态是字符串，统一写成字符串保证哈希稳定。
            Ok(Value::String(match value {
                Value::String(text) => text.clone(),
                _ => number.to_string(),
            }))
        }
        ParamType::Boolean => match value {
            Value::Bool(flag) => Ok(Value::Bool(*flag)),
            _ => Err("需要布尔值".to_string()),
        },
        ParamType::Enum { options } => {
            let text = value.as_str().ok_or_else(|| "需要枚举字符串".to_string())?;
            if !options.iter().any(|option| option == text) {
                return Err(format!("取值必须是：{}", options.join("、")));
            }
            Ok(Value::String(text.to_string()))
        }
        ParamType::StringList => {
            let items = value
                .as_array()
                .ok_or_else(|| "需要字符串数组".to_string())?;
            if !items.iter().all(|item| item.is_string()) {
                return Err("需要字符串数组".to_string());
            }
            Ok(value.clone())
        }
    }
}

fn check_range(spec: &ParamSpec, value: f64) -> Result<(), String> {
    if let Some(min) = numeric(spec.min.as_ref()) {
        if value < min {
            return Err(format!("不能小于 {min}"));
        }
    }
    if let Some(max) = numeric(spec.max.as_ref()) {
        if value > max {
            return Err(format!("不能大于 {max}"));
        }
    }
    Ok(())
}

/// 一个不可变的技能版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVersion {
    pub skill_id: Uuid,
    pub version: u32,
    pub settings: SkillSettings,
    /// 参数空间；缺失时视为全部锁死。
    #[serde(default)]
    pub params_schema: ParamsSchema,
    /// 规则集的 SHA-256；发布与导入据此校验交付物。
    pub settings_sha256: String,
    /// 逻辑 + 参数空间的哈希，不含买家填的值；Skill 版发布用这条做校验。
    #[serde(default)]
    pub logic_sha256: String,
    /// 七维校验报告（`skill_check::Report::view()`）。
    #[serde(default)]
    pub validation: Value,
    /// 是否通过校验；未通过的版本可以保存，但不能发布。
    pub passed: bool,
    #[serde(default)]
    pub changelog: String,
    pub created_at: DateTime<Utc>,
}

impl SkillVersion {
    pub fn new(
        skill_id: Uuid,
        version: u32,
        settings: SkillSettings,
        params_schema: ParamsSchema,
        validation: Value,
        passed: bool,
        changelog: impl Into<String>,
    ) -> Self {
        let settings_sha256 = hash_settings(&settings);
        let logic_sha256 = hash_logic(&settings, &params_schema);
        Self {
            skill_id,
            version,
            settings,
            params_schema,
            settings_sha256,
            logic_sha256,
            validation,
            passed,
            changelog: changelog.into(),
            created_at: Utc::now(),
        }
    }
}

/// 规则集哈希：对序列化后的字节做 SHA-256，字段顺序由结构体固定。
pub fn hash_settings(settings: &SkillSettings) -> String {
    match serde_json::to_vec(settings) {
        Ok(bytes) => hex::encode(Sha256::digest(bytes)),
        Err(error) => {
            tracing::error!(%error, "技能参数序列化失败，使用空哈希");
            hex::encode(Sha256::digest(b"{}"))
        }
    }
}

/// 给每条规则写入来源标记；来源规则 ID 首次写入后保持不变，便于升级对齐。
pub fn tag_rules(settings: &mut SkillSettings, skill_id: Uuid, version: u32) {
    for rule in &mut settings.rules {
        if rule.source_rule_id.is_none() {
            rule.source_rule_id = Some(rule.id);
        }
        rule.source_skill_id = Some(skill_id);
        rule.source_skill_version = Some(version);
    }
}

/// 规则差异：按来源规则 ID（缺失时用当前 ID）对齐。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RuleDiff {
    pub added: Vec<SkillRule>,
    pub removed: Vec<SkillRule>,
    pub changed: Vec<RuleChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuleChange {
    pub rule_id: Uuid,
    pub name: String,
    pub changed_fields: Vec<String>,
    pub before: SkillRule,
    pub after: SkillRule,
}

/// 比较两组规则；用于作者查看版本差异，也用于订阅者升级前预览。
pub fn diff_rules(before: &SkillSettings, after: &SkillSettings) -> RuleDiff {
    let before_map = rule_map(before);
    let after_map = rule_map(after);
    let mut diff = RuleDiff::default();

    for (key, rule) in &after_map {
        match before_map.get(key) {
            None => diff.added.push((*rule).clone()),
            Some(previous) => {
                let changed_fields = changed_fields(previous, rule);
                if !changed_fields.is_empty() {
                    diff.changed.push(RuleChange {
                        rule_id: *key,
                        name: rule.name.clone(),
                        changed_fields,
                        before: (*previous).clone(),
                        after: (*rule).clone(),
                    });
                }
            }
        }
    }
    for (key, rule) in &before_map {
        if !after_map.contains_key(key) {
            diff.removed.push((*rule).clone());
        }
    }
    diff
}

fn rule_map(settings: &SkillSettings) -> HashMap<Uuid, &SkillRule> {
    settings
        .rules
        .iter()
        .map(|rule| (rule.source_rule_id.unwrap_or(rule.id), rule))
        .collect()
}

/// 只比较规则顶层章节，足以说明「改了什么」，又不会把整个 JSON 展开。
fn changed_fields(before: &SkillRule, after: &SkillRule) -> Vec<String> {
    let (Ok(left), Ok(right)) = (serde_json::to_value(before), serde_json::to_value(after)) else {
        return Vec::new();
    };
    let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
        return Vec::new();
    };
    let mut fields: Vec<String> = left
        .keys()
        .filter(|key| left.get(*key) != right.get(*key))
        .cloned()
        .collect();
    fields.sort();
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str) -> SkillRule {
        SkillRule {
            id: Uuid::new_v4(),
            name: name.to_string(),
            ..SkillRule::default()
        }
    }

    #[test]
    fn hash_is_stable_and_changes_with_rules() {
        let settings = SkillSettings {
            rules: vec![rule("海龟突破")],
        };
        let first = hash_settings(&settings);
        assert_eq!(first, hash_settings(&settings.clone()));

        let mut edited = settings.clone();
        edited.rules[0].name = "改名".to_string();
        assert_ne!(first, hash_settings(&edited));
    }

    #[test]
    fn tag_rules_keeps_original_source_id() {
        let original = rule("原规则");
        let original_id = original.id;
        let mut settings = SkillSettings {
            rules: vec![original],
        };
        let skill_id = Uuid::new_v4();
        tag_rules(&mut settings, skill_id, 2);

        let tagged = &settings.rules[0];
        assert_eq!(tagged.id, original_id);
        assert_eq!(tagged.source_rule_id, Some(original_id));
        assert_eq!(tagged.source_skill_id, Some(skill_id));
        assert_eq!(tagged.source_skill_version, Some(2));
    }

    #[test]
    fn diff_aligns_rules_by_source_id() {
        let mut old = rule("旧规则");
        old.source_rule_id = Some(Uuid::new_v4());
        let source_id = old.source_rule_id.expect("有来源 ID");
        let mut new = old.clone();
        new.name = "新规则".to_string();
        new.business.notional_usd = rust_decimal::Decimal::from(200);

        let diff = diff_rules(
            &SkillSettings { rules: vec![old] },
            &SkillSettings { rules: vec![new] },
        );
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].rule_id, source_id);
        assert!(diff.changed[0].changed_fields.contains(&"name".to_string()));
        assert!(diff.changed[0]
            .changed_fields
            .contains(&"business".to_string()));
    }

    #[test]
    fn derived_schema_is_locked_and_blocks_buyer_edits() {
        let mut settings = SkillSettings {
            rules: vec![rule("海龟突破")],
        };
        let skill_id = Uuid::new_v4();
        tag_rules(&mut settings, skill_id, 1);

        let schema = ParamsSchema::from_settings(&settings);
        assert!(!schema.params.is_empty(), "默认规则应能推导出参数");
        assert!(schema.params.iter().all(|spec| !spec.editable));
        assert!(schema.validate(&settings).is_empty());

        // 默认值覆盖不改变逻辑哈希：作者取值不算进交付物。
        let baseline = normalize_settings(&settings, &schema);
        assert_eq!(
            hash_logic(&settings, &schema),
            hash_logic(&baseline, &schema)
        );

        let spec = schema.params[0].clone();
        let values = ParamValues {
            values: vec![ParamValue {
                rule_key: spec.rule_key.clone(),
                path: spec.path.clone(),
                value: Value::String("999".to_string()),
            }],
        };
        assert!(apply_params(&settings, &schema, &values).is_err());
    }

    #[test]
    fn editable_param_applies_within_bounds() {
        let mut settings = SkillSettings {
            rules: vec![rule("海龟突破")],
        };
        tag_rules(&mut settings, Uuid::new_v4(), 1);
        let mut schema = ParamsSchema::from_settings(&settings);
        let spec = schema
            .params
            .iter_mut()
            .find(|spec| spec.path == "business.notional_usd")
            .expect("默认规则包含单笔金额");
        spec.editable = true;
        spec.min = Some(Value::String("10".to_string()));
        spec.max = Some(Value::String("1000".to_string()));
        let spec = spec.clone();

        let values = ParamValues {
            values: vec![ParamValue {
                rule_key: spec.rule_key.clone(),
                path: spec.path.clone(),
                value: Value::String("500".to_string()),
            }],
        };
        let applied = apply_params(&settings, &schema, &values).expect("范围内的值应被接受");
        assert_eq!(
            applied.rules[0].business.notional_usd,
            rust_decimal::Decimal::from(500)
        );

        let too_large = ParamValues {
            values: vec![ParamValue {
                rule_key: spec.rule_key,
                path: spec.path,
                value: Value::String("5000".to_string()),
            }],
        };
        assert!(apply_params(&settings, &schema, &too_large).is_err());
    }
}
