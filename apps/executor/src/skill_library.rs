//! 技能库：独立于策略实例的 Skill 实体与不可变版本。
//!
//! 作者在技能库里创建 Skill，每次编辑追加一个版本；策略发布时绑定具体版本，
//! 订阅者导入后拿到的是该版本冻结的规则快照。版本一旦写入就不再修改。

use std::collections::HashMap;

use chrono::{DateTime, Utc};
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

/// 一个不可变的技能版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVersion {
    pub skill_id: Uuid,
    pub version: u32,
    pub settings: SkillSettings,
    /// 规则集的 SHA-256；发布与导入据此校验交付物。
    pub settings_sha256: String,
    /// 六维校验报告（`skill_check::Report::view()`）。
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
        validation: Value,
        passed: bool,
        changelog: impl Into<String>,
    ) -> Self {
        let settings_sha256 = hash_settings(&settings);
        Self {
            skill_id,
            version,
            settings,
            settings_sha256,
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
    fn tag_rules_keeps_original_rule_id() {
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
}
