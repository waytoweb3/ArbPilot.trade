//! 第一维：SKILL.md 的 YAML 结构校验。
//!
//! 运行期用 `#[serde(default)]` 把整份规则反序列化，缺章会静默回落默认值。
//! 校验器不能复用那条路径，否则「缺了哪一章」永远发现不了；这里改为逐字段读取，
//! 缺失、类型错误、未知字段、非法枚举与不支持的版本都会留下明确结论。

use rust_decimal::Decimal;
use serde_yaml::{Mapping, Value};
use uuid::Uuid;

use super::{Dimension, Issue};
use crate::skill::{
    SkillAction, SkillBusinessRules, SkillCautions, SkillDirection, SkillKind, SkillOutput,
    SkillRule, SkillSteps, SkillTrigger,
};

/// 当前支持的 SKILL.md 版本。
pub const SUPPORTED_VERSION: u32 = 1;

const ROOT_FIELDS: &[&str] = &[
    "version", "name", "purpose", "enabled", "trigger", "steps", "business", "output", "cautions",
];
const TRIGGER_FIELDS: &[&str] = &[
    "kind",
    "event_sources",
    "event_keywords",
    "event_window_secs",
    "timeframe",
    "lookback",
    "fast",
    "slow",
    "atr",
    "channel",
    "atr_multiple",
];
const STEPS_FIELDS: &[&str] = &["execution_mode", "execution_venues", "direction"];
const BUSINESS_FIELDS: &[&str] = &[
    "notional_usd",
    "leverage",
    "take_profit_pct",
    "stop_loss_pct",
    "grid_levels",
    "grid_spacing_pct",
    "grid_share_pct",
];
const OUTPUT_FIELDS: &[&str] = &["action", "notify"];
const CAUTIONS_FIELDS: &[&str] = &["max_notional_usd", "max_daily_opens"];

/// 解析结果：只有整份文档没有 `Error` 时才返回可执行的规则。
pub struct Parsed {
    pub rule: Option<SkillRule>,
    pub issues: Vec<Issue>,
}

/// 解析并做结构校验。
pub fn parse(text: &str) -> Parsed {
    let mut issues = Vec::new();
    let value: Value = match serde_yaml::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.syntax",
                format!("YAML 语法错误：{error}"),
            ));
            return Parsed { rule: None, issues };
        }
    };
    let Some(root) = value.as_mapping() else {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.root_mapping",
            "SKILL.md 顶层必须是键值映射，实际不是",
        ));
        return Parsed { rule: None, issues };
    };
    unknown_fields(root, ROOT_FIELDS, "skill", &mut issues);

    check_version(root, &mut issues);
    let name = required_string(root, "name", "skill", &mut issues).unwrap_or_default();
    let purpose = required_string(root, "purpose", "skill", &mut issues).unwrap_or_default();
    let enabled = optional_bool(root, "enabled", "skill", true, &mut issues);

    let trigger = match required_mapping(root, "trigger", "skill", &mut issues) {
        Some(map) => {
            unknown_fields(map, TRIGGER_FIELDS, "skill.trigger", &mut issues);
            build_trigger(map, &mut issues)
        }
        None => (SkillKind::default(), SkillTrigger::default()),
    };
    let steps = match required_mapping(root, "steps", "skill", &mut issues) {
        Some(map) => {
            unknown_fields(map, STEPS_FIELDS, "skill.steps", &mut issues);
            build_steps(map, &mut issues)
        }
        None => SkillSteps::default(),
    };
    let business = match required_mapping(root, "business", "skill", &mut issues) {
        Some(map) => {
            unknown_fields(map, BUSINESS_FIELDS, "skill.business", &mut issues);
            build_business(map, &mut issues)
        }
        None => SkillBusinessRules::default(),
    };
    let output = match optional_chapter(root, "output", "skill", OUTPUT_FIELDS, &mut issues) {
        Some(map) => build_output(map, &mut issues),
        None => SkillOutput::default(),
    };
    let cautions = match optional_chapter(root, "cautions", "skill", CAUTIONS_FIELDS, &mut issues) {
        Some(map) => build_cautions(map, &mut issues),
        None => SkillCautions::default(),
    };

    let rule = SkillRule {
        id: Uuid::new_v4(),
        name,
        enabled,
        purpose,
        kind: trigger.0,
        trigger: trigger.1,
        steps,
        business,
        output,
        cautions,
    };
    let failed = issues
        .iter()
        .any(|issue| issue.severity == super::Severity::Error);
    Parsed {
        rule: (!failed).then_some(rule),
        issues,
    }
}

fn check_version(root: &Mapping, issues: &mut Vec<Issue>) {
    let Some(value) = required(root, "version", "skill", issues) else {
        return;
    };
    let Some(version) = as_u64(value, "skill.version", issues) else {
        return;
    };
    if version != u64::from(SUPPORTED_VERSION) {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.unsupported_version",
            format!("不支持的 SKILL.md 版本 {version}，当前仅支持 {SUPPORTED_VERSION}"),
        ));
    }
}

fn build_trigger(map: &Mapping, issues: &mut Vec<Issue>) -> (SkillKind, SkillTrigger) {
    let default = SkillTrigger::default();
    let kind = match lookup(map, "kind") {
        Some(value) => parse_kind(value, "skill.trigger.kind", issues).unwrap_or_default(),
        None => {
            missing_field("skill.trigger", "kind", issues);
            SkillKind::default()
        }
    };
    let trigger = SkillTrigger {
        event_sources: optional_string_list(map, "event_sources", "skill.trigger", issues),
        event_keywords: optional_string_list(map, "event_keywords", "skill.trigger", issues),
        event_window_secs: optional_i64(
            map,
            "event_window_secs",
            "skill.trigger",
            default.event_window_secs,
            issues,
        ),
        timeframe: optional_string(
            map,
            "timeframe",
            "skill.trigger",
            &default.timeframe,
            issues,
        ),
        lookback: optional_u32(map, "lookback", "skill.trigger", default.lookback, issues),
        fast: optional_u32(map, "fast", "skill.trigger", default.fast, issues),
        slow: optional_u32(map, "slow", "skill.trigger", default.slow, issues),
        atr: optional_u32(map, "atr", "skill.trigger", default.atr, issues),
        channel: optional_u32(map, "channel", "skill.trigger", default.channel, issues),
        atr_multiple: optional_decimal(
            map,
            "atr_multiple",
            "skill.trigger",
            default.atr_multiple,
            issues,
        ),
    };
    (kind, trigger)
}

fn build_steps(map: &Mapping, issues: &mut Vec<Issue>) -> SkillSteps {
    let default = SkillSteps::default();
    let execution_mode = match lookup(map, "execution_mode") {
        Some(value) => parse_execution_mode(value, "skill.steps.execution_mode", issues)
            .unwrap_or(default.execution_mode),
        None => {
            missing_field("skill.steps", "execution_mode", issues);
            default.execution_mode
        }
    };
    SkillSteps {
        execution_mode,
        execution_venues: optional_string_list(map, "execution_venues", "skill.steps", issues),
        direction: match lookup(map, "direction") {
            Some(value) => {
                parse_direction(value, "skill.steps.direction", issues).unwrap_or(default.direction)
            }
            None => {
                issues.push(Issue::info(
                    Dimension::Structure,
                    "structure.default_applied",
                    "skill.steps.direction 未填写，使用默认值 long",
                ));
                default.direction
            }
        },
    }
}

fn build_business(map: &Mapping, issues: &mut Vec<Issue>) -> SkillBusinessRules {
    let default = SkillBusinessRules::default();
    SkillBusinessRules {
        notional_usd: required_decimal(map, "notional_usd", "skill.business", issues)
            .unwrap_or(default.notional_usd),
        leverage: required_decimal(map, "leverage", "skill.business", issues)
            .unwrap_or(default.leverage),
        take_profit_pct: optional_decimal(
            map,
            "take_profit_pct",
            "skill.business",
            default.take_profit_pct,
            issues,
        ),
        stop_loss_pct: optional_decimal(
            map,
            "stop_loss_pct",
            "skill.business",
            default.stop_loss_pct,
            issues,
        ),
        grid_levels: optional_u32(
            map,
            "grid_levels",
            "skill.business",
            default.grid_levels,
            issues,
        ),
        grid_spacing_pct: optional_decimal(
            map,
            "grid_spacing_pct",
            "skill.business",
            default.grid_spacing_pct,
            issues,
        ),
        grid_share_pct: optional_decimal(
            map,
            "grid_share_pct",
            "skill.business",
            default.grid_share_pct,
            issues,
        ),
    }
}

fn build_output(map: &Mapping, issues: &mut Vec<Issue>) -> SkillOutput {
    let default = SkillOutput::default();
    SkillOutput {
        action: match lookup(map, "action") {
            Some(value) => {
                parse_action(value, "skill.output.action", issues).unwrap_or(default.action)
            }
            None => {
                issues.push(Issue::info(
                    Dimension::Structure,
                    "structure.default_applied",
                    "skill.output.action 未填写，使用默认值 open_long",
                ));
                default.action
            }
        },
        notify: optional_bool(map, "notify", "skill.output", default.notify, issues),
    }
}

fn build_cautions(map: &Mapping, issues: &mut Vec<Issue>) -> SkillCautions {
    let default = SkillCautions::default();
    SkillCautions {
        max_notional_usd: optional_decimal(
            map,
            "max_notional_usd",
            "skill.cautions",
            default.max_notional_usd,
            issues,
        ),
        max_daily_opens: optional_u32(
            map,
            "max_daily_opens",
            "skill.cautions",
            default.max_daily_opens,
            issues,
        ),
    }
}

fn lookup<'a>(map: &'a Mapping, name: &str) -> Option<&'a Value> {
    map.iter()
        .find_map(|(key, value)| (key.as_str() == Some(name)).then_some(value))
}

fn unknown_fields(map: &Mapping, allowed: &[&str], path: &str, issues: &mut Vec<Issue>) {
    for key in map.keys() {
        let Some(name) = key.as_str() else {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.non_string_key",
                format!("{path} 下存在非字符串字段名"),
            ));
            continue;
        };
        if !allowed.contains(&name) {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.unknown_field",
                format!(
                    "{path}.{name} 不是可识别字段；允许的字段：{}",
                    allowed.join(", ")
                ),
            ));
        }
    }
}

fn required<'a>(
    map: &'a Mapping,
    name: &str,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<&'a Value> {
    let value = lookup(map, name);
    if value.is_none() {
        missing_field(path, name, issues);
    }
    value
}

fn missing_field(path: &str, name: &str, issues: &mut Vec<Issue>) {
    issues.push(Issue::error(
        Dimension::Structure,
        "structure.missing_field",
        format!("缺少必填字段 {path}.{name}"),
    ));
}

fn required_mapping<'a>(
    root: &'a Mapping,
    name: &str,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<&'a Mapping> {
    match required(root, name, path, issues)? {
        Value::Mapping(map) => Some(map),
        _ => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.type_mismatch",
                format!("{path}.{name} 必须是键值映射"),
            ));
            None
        }
    }
}

fn optional_chapter<'a>(
    root: &'a Mapping,
    name: &str,
    path: &str,
    allowed: &[&str],
    issues: &mut Vec<Issue>,
) -> Option<&'a Mapping> {
    match lookup(root, name) {
        Some(Value::Mapping(map)) => {
            unknown_fields(map, allowed, &format!("{path}.{name}"), issues);
            Some(map)
        }
        Some(_) => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.type_mismatch",
                format!("{path}.{name} 必须是键值映射"),
            ));
            None
        }
        None => {
            issues.push(Issue::info(
                Dimension::Structure,
                "structure.default_applied",
                format!("{path}.{name} 整章未填写，使用默认值"),
            ));
            None
        }
    }
}

fn required_string(
    map: &Mapping,
    name: &str,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<String> {
    let value = required(map, name, path, issues)?;
    as_string(value, &format!("{path}.{name}"), issues)
}

fn optional_string(
    map: &Mapping,
    name: &str,
    path: &str,
    default: &str,
    issues: &mut Vec<Issue>,
) -> String {
    match lookup(map, name) {
        Some(value) => as_string(value, &format!("{path}.{name}"), issues)
            .unwrap_or_else(|| default.to_string()),
        None => {
            default_applied(path, name, default, issues);
            default.to_string()
        }
    }
}

fn optional_bool(
    map: &Mapping,
    name: &str,
    path: &str,
    default: bool,
    issues: &mut Vec<Issue>,
) -> bool {
    match lookup(map, name) {
        Some(value) => as_bool(value, &format!("{path}.{name}"), issues).unwrap_or(default),
        None => {
            default_applied(path, name, &default, issues);
            default
        }
    }
}

fn optional_u32(
    map: &Mapping,
    name: &str,
    path: &str,
    default: u32,
    issues: &mut Vec<Issue>,
) -> u32 {
    match lookup(map, name) {
        Some(value) => as_u64(value, &format!("{path}.{name}"), issues)
            .and_then(|number| u32::try_from(number).ok())
            .unwrap_or_else(|| {
                if value.as_u64().is_some() {
                    issues.push(Issue::error(
                        Dimension::Structure,
                        "structure.out_of_range",
                        format!("{path}.{name} 超出 u32 范围"),
                    ));
                }
                default
            }),
        None => {
            default_applied(path, name, &default, issues);
            default
        }
    }
}

fn optional_i64(
    map: &Mapping,
    name: &str,
    path: &str,
    default: i64,
    issues: &mut Vec<Issue>,
) -> i64 {
    match lookup(map, name) {
        Some(value) => as_i64(value, &format!("{path}.{name}"), issues).unwrap_or(default),
        None => {
            default_applied(path, name, &default, issues);
            default
        }
    }
}

fn optional_decimal(
    map: &Mapping,
    name: &str,
    path: &str,
    default: Decimal,
    issues: &mut Vec<Issue>,
) -> Decimal {
    match lookup(map, name) {
        Some(value) => as_decimal(value, &format!("{path}.{name}"), issues).unwrap_or(default),
        None => {
            default_applied(path, name, &default, issues);
            default
        }
    }
}

fn required_decimal(
    map: &Mapping,
    name: &str,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<Decimal> {
    let value = required(map, name, path, issues)?;
    as_decimal(value, &format!("{path}.{name}"), issues)
}

fn optional_string_list(
    map: &Mapping,
    name: &str,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Vec<String> {
    let Some(value) = lookup(map, name) else {
        default_applied(path, name, "[]", issues);
        return Vec::new();
    };
    let Some(items) = value.as_sequence() else {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.type_mismatch",
            format!("{path}.{name} 必须是字符串数组"),
        ));
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item.as_str() {
            Some(text) => Some(text.to_string()),
            None => {
                issues.push(Issue::error(
                    Dimension::Structure,
                    "structure.type_mismatch",
                    format!("{path}.{name} 里存在非字符串元素"),
                ));
                None
            }
        })
        .collect()
}

fn default_applied<T: std::fmt::Display + ?Sized>(
    path: &str,
    name: &str,
    default: &T,
    issues: &mut Vec<Issue>,
) {
    issues.push(Issue::info(
        Dimension::Structure,
        "structure.default_applied",
        format!("{path}.{name} 未填写，使用默认值 {default}"),
    ));
}

fn as_string(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<String> {
    value.as_str().map(str::to_string).or_else(|| {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.type_mismatch",
            format!("{path} 必须是字符串"),
        ));
        None
    })
}

fn as_bool(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<bool> {
    value.as_bool().or_else(|| {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.type_mismatch",
            format!("{path} 必须是布尔值 true/false"),
        ));
        None
    })
}

fn as_u64(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<u64> {
    value.as_u64().or_else(|| {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.type_mismatch",
            format!("{path} 必须是非负整数"),
        ));
        None
    })
}

fn as_i64(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<i64> {
    value.as_i64().or_else(|| {
        issues.push(Issue::error(
            Dimension::Structure,
            "structure.type_mismatch",
            format!("{path} 必须是整数"),
        ));
        None
    })
}

fn as_decimal(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<Decimal> {
    if let Some(text) = value.as_str() {
        return match text.trim().parse::<Decimal>() {
            Ok(decimal) => Some(decimal),
            Err(_) => {
                issues.push(Issue::error(
                    Dimension::Structure,
                    "structure.type_mismatch",
                    format!("{path} 必须是数字，实际是字符串 `{text}`"),
                ));
                None
            }
        };
    }
    if let Some(integer) = value.as_i64() {
        return Some(Decimal::from(integer));
    }
    if let Some(float) = value.as_f64() {
        return Decimal::try_from(float).ok().or_else(|| {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.type_mismatch",
                format!("{path} 无法转换为十进制数"),
            ));
            None
        });
    }
    issues.push(Issue::error(
        Dimension::Structure,
        "structure.type_mismatch",
        format!("{path} 必须是数字"),
    ));
    None
}

fn parse_kind(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<SkillKind> {
    let text = as_string(value, path, issues)?;
    match text.trim().to_ascii_lowercase().as_str() {
        "event" => Some(SkillKind::Event),
        "breakout" => Some(SkillKind::Breakout),
        "ma_trend" | "ma-trend" => Some(SkillKind::MaTrend),
        "grid" => Some(SkillKind::Grid),
        other => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.invalid_enum",
                format!("{path} 的值 `{other}` 非法；允许 event / breakout / ma_trend / grid"),
            ));
            None
        }
    }
}

fn parse_execution_mode(
    value: &Value,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<arb_core::StrategyExecutionMode> {
    let text = as_string(value, path, issues)?;
    match text.trim().to_ascii_lowercase().as_str() {
        "real" => Some(arb_core::StrategyExecutionMode::Real),
        "paper" => Some(arb_core::StrategyExecutionMode::Paper),
        other => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.invalid_enum",
                format!("{path} 的值 `{other}` 非法；允许 real / paper"),
            ));
            None
        }
    }
}

fn parse_direction(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<SkillDirection> {
    let text = as_string(value, path, issues)?;
    match text.trim().to_ascii_lowercase().as_str() {
        "both" => Some(SkillDirection::Both),
        "long" => Some(SkillDirection::Long),
        "short" => Some(SkillDirection::Short),
        other => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.invalid_enum",
                format!("{path} 的值 `{other}` 非法；允许 both / long / short"),
            ));
            None
        }
    }
}

fn parse_action(value: &Value, path: &str, issues: &mut Vec<Issue>) -> Option<SkillAction> {
    let text = as_string(value, path, issues)?;
    match text.trim().to_ascii_lowercase().as_str() {
        "open_long" | "open-long" => Some(SkillAction::OpenLong),
        "notify_only" | "notify-only" => Some(SkillAction::NotifyOnly),
        other => {
            issues.push(Issue::error(
                Dimension::Structure,
                "structure.invalid_enum",
                format!("{path} 的值 `{other}` 非法；允许 open_long / notify_only"),
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_check::Severity;

    fn errors(issues: &[Issue]) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|issue| issue.severity == Severity::Error)
            .collect()
    }

    fn codes(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|issue| issue.code).collect()
    }

    const MINIMAL: &str = r#"
version: 1
name: 网格
purpose: 在现价上下挂网格。
trigger:
  kind: grid
steps:
  execution_mode: paper
business:
  notional_usd: 100
  leverage: 1
"#;

    #[test]
    fn minimal_document_applies_defaults_and_reports_them() {
        let parsed = parse(MINIMAL);
        let rule = parsed.rule.expect("最小文档应通过");
        assert_eq!(rule.trigger.timeframe, "1h");
        assert_eq!(rule.trigger.lookback, 200);
        assert_eq!(rule.business.grid_levels, 3);
        assert!(codes(&parsed.issues).contains(&"structure.default_applied"));
        assert!(errors(&parsed.issues).is_empty());
    }

    #[test]
    fn missing_chapter_is_an_error() {
        let text = MINIMAL.replace("business:\n  notional_usd: 100\n  leverage: 1", "");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(codes(&parsed.issues).contains(&"structure.missing_field"));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let text = MINIMAL.replace("name: 网格", "name: 网格\nowner: me");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(errors(&parsed.issues)
            .iter()
            .any(|issue| issue.code == "structure.unknown_field"));
    }

    #[test]
    fn missing_required_business_field_is_an_error() {
        let text = MINIMAL.replace("  leverage: 1\n", "");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(errors(&parsed.issues)
            .iter()
            .any(|issue| issue.message.contains("skill.business.leverage")));
    }

    #[test]
    fn wrong_type_is_rejected() {
        let text = MINIMAL.replace("notional_usd: 100", "notional_usd: abc");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(errors(&parsed.issues)
            .iter()
            .any(|issue| issue.code == "structure.type_mismatch"));
    }

    #[test]
    fn invalid_enum_is_rejected() {
        let text = MINIMAL.replace("kind: grid", "kind: moon");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(codes(&parsed.issues).contains(&"structure.invalid_enum"));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let text = MINIMAL.replace("version: 1", "version: 2");
        let parsed = parse(&text);
        assert!(parsed.rule.is_none());
        assert!(codes(&parsed.issues).contains(&"structure.unsupported_version"));
    }

    #[test]
    fn syntax_error_is_reported() {
        let parsed = parse("version: [unclosed");
        assert!(parsed.rule.is_none());
        assert!(codes(&parsed.issues).contains(&"structure.syntax"));
    }

    #[test]
    fn documented_aliases_are_accepted() {
        let text = MINIMAL.replace("kind: grid", "kind: ma-trend");
        let parsed = parse(&text);
        assert_eq!(parsed.rule.expect("别名应可解析").kind, SkillKind::MaTrend);
    }
}
