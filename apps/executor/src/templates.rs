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

use crate::state::AppState;

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

/// 五个内置模板的默认集合。
pub fn builtin() -> Vec<StrategyTemplate> {
    let now = Utc::now();
    vec![
        StrategyTemplate {
            key: "turtle-breakout".to_string(),
            name: "海龟突破".to_string(),
            description: "价格突破唐奇安通道入场，反向突破或 2 倍 ATR 止损离场。".to_string(),
            definition: definition(
                "海龟突破",
                "空仓时突破前 N 根最高价做多、跌破最低价做空；持仓时反向突破或用 2 倍 ATR 止损。\
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
            description: "现价上下按固定间距布置档位，价格触及后下单，到达相邻档位止盈。".to_string(),
            definition: definition(
                "网格",
                "围绕锚定价在上下方各布置若干档位，价格触及档位时单腿开仓，到达相邻档位止盈。\
                 具体参数在策略详情的技能参数里配置。",
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
}
