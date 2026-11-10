//! 内置策略模板：海龟突破、均线趋势、网格、上币公告跟随与技能策略。
//!
//! 这些模板只是「参数外壳」：具体的触发条件（公告事件、通道突破、均线趋势、网格
//! 档位）、执行步骤与业务规则都保存在技能参数里，由 `skill` 模块的 Rust 运行时解释
//! 执行，所有下单仍然经过统一风控网关。
//!
//! 模板只保存定义与说明，不携带源码或编译产物：实例化时写入模板标识，技能运行时
//! 据此接管策略，因此模板本身无需沙箱模块即可运行。

use arb_core::{
    CostAssumptions, DataRules, EntryRules, ExecutionRules, ExitRules, Instrument,
    StrategyDefinition, StrategyKind, StrategyRisk, StrategyUniverse,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    skill,
    skill_check::{self, Costs},
    skill_library::{self, Skill, SkillVersion},
    state::AppState,
};

/// 一个可直接实例化的内置策略模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyTemplate {
    /// 稳定标识，重复启动时据此幂等写入；实例化后也用它识别技能运行时。
    pub key: String,
    pub name: String,
    pub description: String,
    /// 实例化时复制的默认定义。
    pub definition: StrategyDefinition,
    pub updated_at: DateTime<Utc>,
}

/// 官方内置模板：由官方实例发布到策略市场，用户执行器通过导入获得。
pub const OFFICIAL_TEMPLATE_KEYS: [&str; 4] =
    ["turtle-breakout", "ma-trend", "grid", "listing-follow"];

/// 官方内置资源使用稳定 UUID，重复启动不会产生新记录。
pub(crate) fn official_uuid(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes())
}

pub(crate) fn official_author_id() -> Uuid {
    official_uuid("https://arbpilot.trade/market/author/official")
}

/// 内置模板在技能库里的稳定 ID。
pub fn builtin_skill_id(key: &str) -> Uuid {
    official_uuid(&format!("https://arbpilot.trade/skill/{key}"))
}

/// 内置模板的 v1 版本；版本不可变，后续调整只能追加新版本。
pub async fn builtin_version(state: &AppState, key: &str) -> Option<SkillVersion> {
    if !OFFICIAL_TEMPLATE_KEYS.contains(&key) {
        return None;
    }
    state.store.skill_version(builtin_skill_id(key), 1).await
}

/// 五个内置模板的默认集合。
pub fn builtin() -> Vec<StrategyTemplate> {
    let now = Utc::now();
    vec![
        StrategyTemplate {
            key: "turtle-breakout".to_string(),
            name: "海龟突破".to_string(),
            description: "系统一 20/10、系统二 55/20 唐奇安通道突破，按 N 计算 Unit 并金字塔加仓。"
                .to_string(),
            definition: definition(
                "海龟突破",
                "系统一/系统二突破入场，0.5N 加仓、2N 止损；系统一盈利后跳过下一次信号。\
                 具体参数在策略详情的技能参数里配置。",
                DataRules {
                    timeframe: "1h".to_string(),
                    lookback: 300,
                    fast: 20,
                    slow: 55,
                    atr: 20,
                    channel: 20,
                    ..DataRules::default()
                },
            ),
            updated_at: now,
        },
        StrategyTemplate {
            key: "ma-trend".to_string(),
            name: "均线趋势".to_string(),
            description: "快慢均线多头排列做多、空头排列做空，均线反转离场。".to_string(),
            definition: definition(
                "均线趋势",
                "快均线在慢均线上方且价格站上快线做多，反之做空；快慢均线重新交叉时离场。\
                 具体参数在策略详情的技能参数里配置。",
                DataRules {
                    timeframe: "1h".to_string(),
                    lookback: 200,
                    fast: 20,
                    slow: 55,
                    atr: 20,
                    channel: 20,
                    ..DataRules::default()
                },
            ),
            updated_at: now,
        },
        StrategyTemplate {
            key: "grid".to_string(),
            name: "网格".to_string(),
            description: "在价格区间内按等差或等比布置档位，触及后下单，到达相邻档位止盈。"
                .to_string(),
            definition: definition(
                "网格",
                "在价格区间内按等差或等比布置档位，价格触及档位时单腿开仓，到达相邻档位止盈；\
                 支持触发价、区间上移/下移与整组止盈止损。具体参数在策略详情的技能参数里配置。",
                DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 200,
                    ..DataRules::default()
                },
            ),
            updated_at: now,
        },
        StrategyTemplate {
            key: "listing-follow".to_string(),
            name: "上币公告跟随".to_string(),
            description: "交易所发布上币公告后买入该交易对，持有到期后离场。".to_string(),
            definition: definition(
                "上币公告跟随",
                "主机只注入最近一小时内、命中本策略交易对的上币公告；\
                 公告发布后 15 分钟内市价做多，持有 6 小时后平仓。\
                 具体参数在策略详情的技能参数里配置。",
                DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 200,
                    event_sources: Vec::new(),
                    event_keywords: Vec::new(),
                    event_window_secs: 3_600,
                    ..DataRules::default()
                },
            ),
            updated_at: now,
        },
        StrategyTemplate {
            key: "skill".to_string(),
            name: "技能策略（SKILL.md）".to_string(),
            description: "把 SKILL.md 的技能用途、触发条件、执行步骤、业务规则、输出格式与注意事项转成可配置参数。".to_string(),
            definition: definition(
                "技能策略（SKILL.md）",
                "技能用途、什么时候调用、执行步骤、业务规则、输出格式与注意事项都在策略详情的技能参数里配置；\
                 执行器按公告来源、关键词与时间窗口触发，并按执行交易所优先级市价做多。",
                DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 200,
                    event_sources: Vec::new(),
                    event_keywords: Vec::new(),
                    event_window_secs: 3_600,
                    ..DataRules::default()
                },
            ),
            updated_at: now,
        },
    ]
}

fn definition(name: &str, notes: &str, data: DataRules) -> StrategyDefinition {
    StrategyDefinition {
        name: name.to_string(),
        kind: StrategyKind::Custom,
        universe: StrategyUniverse {
            venues: vec!["binance".to_string()],
            symbols: vec!["BTCUSDT".to_string()],
            instruments: vec![Instrument::Perp],
        },
        entry: EntryRules {
            funding_spread_min: Decimal::ZERO,
            net_edge_min: Decimal::ZERO,
        },
        exit: ExitRules {
            spread_below: Decimal::ZERO,
            max_holding_hours: Decimal::from(24),
        },
        risk: StrategyRisk {
            max_notional_usd: Decimal::from(500),
            max_unhedged_seconds: 3,
        },
        execution: ExecutionRules {
            leverage: Decimal::ONE,
            max_slippage_bps: Decimal::from(10),
            cooldown_secs: 60,
        },
        data,
        costs: CostAssumptions::default(),
        notes: Some(notes.to_string()),
    }
}

/// 启动时补齐模板。
///
/// 四个官方技能只存在于服务器：官方实例补齐全部模板并发布到策略市场，用户执行器
/// 通过导入发布获得冻结定义与技能参数，不需要本地模板副本。本地只补齐通用
/// 「技能策略」模板，并清掉历史版本遗留的官方模板行。
pub async fn seed(state: &AppState) {
    let all = builtin();
    let local = |template: &&StrategyTemplate| {
        state.config.public_mode || !OFFICIAL_TEMPLATE_KEYS.contains(&template.key.as_str())
    };
    let stored = state.store.strategy_templates().await;

    if !state.config.public_mode {
        for saved in stored
            .iter()
            .filter(|saved| OFFICIAL_TEMPLATE_KEYS.contains(&saved.key.as_str()))
        {
            state.store.delete_strategy_template(&saved.key).await;
        }
    }

    for template in all.iter().filter(local) {
        match stored.iter().find(|saved| saved.key == template.key) {
            Some(saved) => {
                if saved.name != template.name || saved.definition.name != template.definition.name
                {
                    let mut updated = saved.clone();
                    updated.name = template.name.clone();
                    updated.definition.name = template.definition.name.clone();
                    updated.updated_at = Utc::now();
                    state.store.save_strategy_template(&updated).await;
                }
            }
            None => state.store.save_strategy_template(template).await,
        }
    }

    rename_legacy_strategies(state, &all).await;
    seed_skills(state).await;
}

/// 把官方内置模板补成技能库里的 Skill + v1 版本。
///
/// 四个模板不再只靠模板标识特例识别：它们和用户创建的技能共用同一条
/// 「技能 → 版本 → 发布 → 订阅 → 导入 → 规则实例」链路，发布与导入都用
/// 这里的版本哈希校验。版本一旦写入就不再覆盖，调整默认参数只能追加新版本。
///
/// 每个实例都会补齐：官方实例据此发布，用户实例也能在技能库里看到并复用同一份版本。
pub async fn seed_skills(state: &AppState) {
    let templates: Vec<StrategyTemplate> = builtin()
        .into_iter()
        .filter(|template| OFFICIAL_TEMPLATE_KEYS.contains(&template.key.as_str()))
        .collect();

    let skills = state.store.skills().await;
    let limits = state.risk_limits();
    let candles: Vec<arb_core::SandboxCandle> = Vec::new();
    for template in templates {
        let Some(kind) = skill::kind_from_template(&template.key) else {
            continue;
        };
        let id = builtin_skill_id(&template.key);
        let existing = skills.iter().find(|item| item.id == id);
        // 已存在 v1 且技能信息一致时保持原样：版本不可变，也不重复写库。
        if matches!(existing, Some(skill) if skill.latest_version >= 1 && skill.name == template.name)
            && state.store.skill_version(id, 1).await.is_some()
        {
            continue;
        }
        let mut skill = match existing {
            Some(existing) => existing.clone(),
            None => {
                let mut skill = Skill::new(
                    official_author_id(),
                    template.name.clone(),
                    template.description.clone(),
                );
                skill.id = id;
                skill
            }
        };

        // 已存在同号版本时保留历史冻结规则，只把技能实体补到 v1。
        if state.store.skill_version(id, 1).await.is_none() {
            let mut settings =
                skill::default_settings(kind, template.definition.execution.leverage);
            skill_library::tag_rules(&mut settings, id, 1);
            let mut report = skill_check::Report::default();
            for rule in &settings.rules {
                for issue in
                    skill_check::evaluate_rule(rule, &limits, &candles, Costs::default()).issues
                {
                    report.push(issue);
                }
            }
            let view = report.view();
            let version = SkillVersion::new(
                id,
                1,
                settings,
                serde_json::to_value(&view).unwrap_or_default(),
                view.passed,
                "官方内置模板",
            );
            state.store.save_skill_version(&version).await;
        }

        skill.latest_version = skill.latest_version.max(1);
        skill.name = template.name.clone();
        skill.description = template.description.clone();
        skill.updated_at = Utc::now();
        state.store.save_skill(&skill).await;
    }
}

/// 旧版本把带「 · BTCUSDT」的模板默认名复制进了已创建的策略。
/// 启动时按旧名精确匹配归一，用户自行改过的名称不受影响。
async fn rename_legacy_strategies(state: &AppState, builtins: &[StrategyTemplate]) {
    let legacy: Vec<(String, String)> = builtins
        .iter()
        .map(|template| {
            (
                format!("{} · BTCUSDT", template.name),
                template.name.clone(),
            )
        })
        .collect();
    for mut record in state.store.load_strategies().await {
        let Some((_, name)) = legacy
            .iter()
            .find(|(old, _)| *old == record.definition.name)
        else {
            continue;
        };
        record.definition.name = name.clone();
        {
            let mut strategies = state.strategies.write().expect("策略锁不可用");
            if let Some(existing) = strategies.iter_mut().find(|item| item.id == record.id) {
                *existing = record.clone();
            }
        }
        state.store.save_strategy(&record).await;
        state.store.save_strategy_version(&record).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_definitions_pass_validation() {
        let limits = arb_core::RiskLimits::default();
        for template in builtin() {
            let validation = template.definition.validate(&limits);
            assert!(
                !validation.has_errors(),
                "{} 定义有错误: {:?}",
                template.key,
                validation.issues
            );
        }
    }

    /// 每个内置模板都必须能被技能运行时识别，实例化后无需沙箱模块即可运行。
    #[test]
    fn builtin_templates_map_to_skill_kinds() {
        for template in builtin() {
            assert!(
                crate::skill::kind_from_template(&template.key).is_some(),
                "{} 没有对应的技能触发方式",
                template.key
            );
        }
    }

    /// 内置模板补成技能库版本后必须可发布，且导入端重新打标得到同一份哈希。
    #[test]
    fn builtin_skill_versions_are_publishable() {
        let limits = arb_core::RiskLimits::default();
        let mut ids = std::collections::HashSet::new();
        for key in OFFICIAL_TEMPLATE_KEYS {
            let id = builtin_skill_id(key);
            assert!(ids.insert(id), "{key} 的技能 ID 与其他模板冲突");

            let kind = crate::skill::kind_from_template(key).expect("有对应触发方式");
            let mut settings = crate::skill::default_settings(kind, Decimal::ONE);
            skill_library::tag_rules(&mut settings, id, 1);

            let mut report = skill_check::Report::default();
            for rule in &settings.rules {
                for issue in skill_check::evaluate_rule(rule, &limits, &[], Costs::default()).issues
                {
                    report.push(issue);
                }
            }
            assert!(report.view().passed, "{key} 的默认参数未通过校验");

            let mut imported = settings.clone();
            skill_library::tag_rules(&mut imported, id, 1);
            assert_eq!(
                skill_library::hash_settings(&settings),
                skill_library::hash_settings(&imported),
                "{key} 发布与导入的规则哈希不一致"
            );
        }
    }
}
