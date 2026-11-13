//! 参数空间维度：确认作者定义的参数能被买家安全地改成不同实例。

use crate::skill::SkillSettings;
use crate::skill_library::ParamsSchema;

use super::{Dimension, Issue};

/// 校验参数空间；问题按 `rule_key + path` 定位。
pub fn check(schema: &ParamsSchema, settings: &SkillSettings) -> Vec<Issue> {
    let mut issues = Vec::new();
    for problem in schema.validate(settings) {
        issues.push(Issue::error(
            Dimension::Params,
            "param_invalid",
            format!(
                "{} · {}：{}",
                problem.rule_key, problem.path, problem.message
            ),
        ));
    }
    // 全部锁死时商品其实是「配置版」，提醒作者放开一部分参数。
    if !schema.params.is_empty() {
        let locked = schema.params.iter().filter(|spec| !spec.editable).count();
        if locked * 2 > schema.params.len() {
            issues.push(Issue::warning(
                Dimension::Params,
                "params_mostly_locked",
                format!(
                    "{locked}/{} 个参数被锁死，买家几乎无法生成不同实例",
                    schema.params.len()
                ),
            ));
        }
    }
    issues
}
