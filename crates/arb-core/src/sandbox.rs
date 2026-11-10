//! WASM 策略沙箱的受限 ABI 与运行结果类型。
//!
//! 沙箱只接收确定性的行情快照，只输出「开仓 / 平仓 / 挂单 / 撤单 / 保持」意图：
//! - 模块无法访问 WASI、文件系统、网络、环境变量或任何交易所密钥；
//! - 主机限制燃料、内存、墙钟时间、输出大小与决策条数；
//! - 输出意图仍必须经过执行器统一风控网关，沙箱本身没有下单能力。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    strategy::{Instrument, ValidationIssue, ValidationSeverity},
    types::PositionSide,
};

/// 当前沙箱 ABI 版本。模块导出 `abi_version()` 必须返回该值。
pub const SANDBOX_ABI_VERSION: u32 = 2;
/// 模块必须导出的决策函数名。
pub const SANDBOX_ENTRYPOINT: &str = "evaluate";
/// 主机提供给模块的回调，用于提交决策 JSON。
pub const SANDBOX_EMIT: &str = "emit";
/// 主机提供给模块的日志回调（受限条数）。
pub const SANDBOX_LOG: &str = "log";
/// 模块导入命名空间。
pub const SANDBOX_IMPORT_NAMESPACE: &str = "arbpilot";

/// 可选的结构化行情读取与决策辅助函数。
///
/// 这些导入让模块无需自己解析 JSON 就能读取确定性的行情字段并提交意图，
/// 仍然没有任何文件、网络、环境变量或交易能力。
pub const SANDBOX_MARKETS: &str = "markets";
pub const SANDBOX_MARKET: &str = "market";
/// 把第 `index` 个交易对的名称写入模块内存，返回字节数；失败返回 0。
pub const SANDBOX_MARKET_SYMBOL: &str = "market_symbol";
pub const SANDBOX_CANDLES: &str = "candles";
pub const SANDBOX_CANDLE: &str = "candle";
pub const SANDBOX_INDICATOR: &str = "indicator";
pub const SANDBOX_POSITION_PRESENT: &str = "position_present";
pub const SANDBOX_POSITION_NOTIONAL: &str = "position_notional";
pub const SANDBOX_POSITION_HELD_SECS: &str = "position_held_secs";
pub const SANDBOX_POSITION_HELD_BARS: &str = "position_held_bars";
pub const SANDBOX_POSITION_SIDE: &str = "position_side";
pub const SANDBOX_POSITION_ENTRY_PRICE: &str = "position_entry_price";
pub const SANDBOX_OPEN: &str = "open";
/// 资金费率配对开仓：现货多 + 永续空，交易所与交易对取自策略定义。
pub const SANDBOX_OPEN_PAIR: &str = "open_pair";
pub const SANDBOX_CLOSE: &str = "close";
/// 网格挂单：登记一个价格触及时才真正下单的限价意图。
pub const SANDBOX_LIMIT: &str = "limit";
/// 撤销该策略在某交易对上的全部待触发限价意图。
pub const SANDBOX_CANCEL: &str = "cancel";
pub const SANDBOX_HOLD: &str = "hold";
/// 最近可见事件的条数。
pub const SANDBOX_EVENTS: &str = "events";
/// 读取第 `index` 条事件的数值字段。
pub const SANDBOX_EVENT: &str = "event";
/// 把第 `index` 条事件的文本字段写入模块内存，返回字节数；失败返回 0。
pub const SANDBOX_EVENT_TEXT: &str = "event_text";

/// `event(index, field)` 的字段编号。
pub const SANDBOX_EVENT_PUBLISHED_AT: i32 = 0;
/// 距本次评估的秒数。
pub const SANDBOX_EVENT_AGE_SECS: i32 = 1;
/// 是否命中策略声明的交易对（0/1）。
pub const SANDBOX_EVENT_MATCHED: i32 = 2;

/// `event_text(index, field, ptr, cap)` 的字段编号。
pub const SANDBOX_EVENT_TEXT_SOURCE: i32 = 0;
pub const SANDBOX_EVENT_TEXT_KIND: i32 = 1;
pub const SANDBOX_EVENT_TEXT_TITLE: i32 = 2;
/// 命中的交易对；未命中时为空字符串。
pub const SANDBOX_EVENT_TEXT_SYMBOL: i32 = 3;

/// `market(index, field)` 的字段编号。
pub const SANDBOX_FIELD_BID: i32 = 0;
pub const SANDBOX_FIELD_ASK: i32 = 1;
pub const SANDBOX_FIELD_LAST: i32 = 2;
pub const SANDBOX_FIELD_FUNDING_RATE: i32 = 3;
/// 0 = 现货，1 = 永续。
pub const SANDBOX_FIELD_INSTRUMENT: i32 = 4;
pub const SANDBOX_FIELD_MAX_NOTIONAL_USD: i32 = 5;

/// `candle(market, offset, field)` 的字段编号；`offset` 0 表示最新一根。
pub const SANDBOX_CANDLE_OPEN: i32 = 0;
pub const SANDBOX_CANDLE_HIGH: i32 = 1;
pub const SANDBOX_CANDLE_LOW: i32 = 2;
pub const SANDBOX_CANDLE_CLOSE: i32 = 3;
pub const SANDBOX_CANDLE_VOLUME: i32 = 4;
/// 开盘时间（Unix 秒）。
pub const SANDBOX_CANDLE_OPEN_TIME: i32 = 5;

/// `indicator(market, id)` 的编号，缺数据时返回 0。
pub const SANDBOX_INDICATOR_SMA_FAST: i32 = 0;
pub const SANDBOX_INDICATOR_SMA_SLOW: i32 = 1;
pub const SANDBOX_INDICATOR_EMA_FAST: i32 = 2;
pub const SANDBOX_INDICATOR_EMA_SLOW: i32 = 3;
pub const SANDBOX_INDICATOR_ATR: i32 = 4;
pub const SANDBOX_INDICATOR_CHANNEL_HIGH: i32 = 5;
pub const SANDBOX_INDICATOR_CHANNEL_LOW: i32 = 6;

/// 沙箱资源限额。默认值保守，可按策略调整但需通过校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxLimits {
    /// 单次评估允许消耗的燃料（近似 CPU 指令数）。
    pub max_fuel: u64,
    /// 模块可增长到的最大线性内存（字节）。
    pub max_memory_bytes: usize,
    /// 墙钟超时（毫秒），超时通过 epoch 中断。
    pub timeout_ms: u64,
    /// 单次输出 JSON 的最大字节数。
    pub max_output_bytes: usize,
    /// 单次评估最多接受的决策条数。
    pub max_decisions: usize,
    /// 单次评估最多保留的日志行数。
    pub max_log_lines: usize,
    /// 允许上传的 wasm 文件最大字节数。
    pub max_artifact_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            max_fuel: 5_000_000,
            max_memory_bytes: 16 * 1024 * 1024,
            timeout_ms: 1_000,
            max_output_bytes: 16 * 1024,
            max_decisions: 8,
            max_log_lines: 32,
            max_artifact_bytes: 512 * 1024,
        }
    }
}

impl SandboxLimits {
    /// 硬上限：接口不接受更宽松的配置，避免沙箱退化为无限执行。
    pub const HARD_MAX_FUEL: u64 = 50_000_000;
    pub const HARD_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
    pub const HARD_MAX_TIMEOUT_MS: u64 = 5_000;

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let error = |path: &str, message: &str| ValidationIssue {
            path: path.to_string(),
            severity: ValidationSeverity::Error,
            message: message.to_string(),
        };

        if self.max_fuel == 0 || self.max_fuel > Self::HARD_MAX_FUEL {
            issues.push(error("max_fuel", "燃料上限必须在 1 到 5000 万之间"));
        }
        if self.max_memory_bytes == 0 || self.max_memory_bytes > Self::HARD_MAX_MEMORY_BYTES {
            issues.push(error(
                "max_memory_bytes",
                "内存上限必须在 1 字节到 64 MiB 之间",
            ));
        }
        if self.timeout_ms == 0 || self.timeout_ms > Self::HARD_MAX_TIMEOUT_MS {
            issues.push(error("timeout_ms", "超时必须在 1 到 5000 毫秒之间"));
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > 256 * 1024 {
            issues.push(error(
                "max_output_bytes",
                "输出上限必须在 1 字节到 256 KiB 之间",
            ));
        }
        if self.max_decisions == 0 || self.max_decisions > 32 {
            issues.push(error("max_decisions", "决策条数上限必须在 1 到 32 之间"));
        }
        if self.max_log_lines > 128 {
            issues.push(error("max_log_lines", "日志行数上限不能超过 128"));
        }
        if self.max_artifact_bytes == 0 || self.max_artifact_bytes > 4 * 1024 * 1024 {
            issues.push(error(
                "max_artifact_bytes",
                "wasm 文件上限必须在 1 字节到 4 MiB 之间",
            ));
        }
        issues
    }

    pub fn has_errors(&self) -> bool {
        self.validate()
            .iter()
            .any(|issue| issue.severity == ValidationSeverity::Error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxArtifactSource {
    Uploaded,
    BuiltIn,
}

/// 已上传并完成静态校验的 wasm 模块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxArtifact {
    pub id: Uuid,
    pub name: String,
    /// 模块内容 sha256，用于绑定版本与复现。
    pub sha256: String,
    pub size_bytes: usize,
    /// 解析到的 ABI 版本；无法解析时为 None。
    pub abi_version: Option<u32>,
    pub source: SandboxArtifactSource,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SandboxArtifact {
    pub fn validate(&self, limits: &SandboxLimits) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.name.trim().is_empty() {
            issues.push(ValidationIssue {
                path: "name".into(),
                severity: ValidationSeverity::Error,
                message: "模块名称不能为空".into(),
            });
        }
        if self.size_bytes == 0 || self.size_bytes > limits.max_artifact_bytes {
            issues.push(ValidationIssue {
                path: "size_bytes".into(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "wasm 文件大小必须在 1 字节到 {} 字节之间",
                    limits.max_artifact_bytes
                ),
            });
        }
        if self.abi_version != Some(SANDBOX_ABI_VERSION) {
            issues.push(ValidationIssue {
                path: "abi_version".into(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "模块 ABI 版本必须为 {}，当前为 {:?}",
                    SANDBOX_ABI_VERSION, self.abi_version
                ),
            });
        }
        issues
    }
}

/// 策略与沙箱模块的绑定。策略运行时会用该模块替代内置决策逻辑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxBinding {
    pub artifact_id: Uuid,
    pub artifact_hash: String,
    pub abi_version: u32,
    pub limits: SandboxLimits,
    pub attached_at: DateTime<Utc>,
}

impl SandboxBinding {
    pub fn new(artifact: &SandboxArtifact, limits: SandboxLimits) -> Self {
        Self {
            artifact_id: artifact.id,
            artifact_hash: artifact.sha256.clone(),
            abi_version: SANDBOX_ABI_VERSION,
            limits,
            attached_at: Utc::now(),
        }
    }
}

/// 单根 K 线。模块无法访问历史接口，只能看到主机传入的确定性窗口。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxCandle {
    pub open_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// 主机侧计算好的指标，避免模块自己维护滚动窗口。
///
/// 数据不足时字段为 `None`，模块读取辅助函数会得到 0。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxIndicators {
    pub sma_fast: Option<Decimal>,
    pub sma_slow: Option<Decimal>,
    pub ema_fast: Option<Decimal>,
    pub ema_slow: Option<Decimal>,
    /// 按周期索引的简单均线：指标层只算配置里出现的周期。
    #[serde(default)]
    pub sma_by_period: std::collections::BTreeMap<u32, Decimal>,
    /// 平均真实波幅。
    pub atr: Option<Decimal>,
    /// 唐奇安通道上轨（前 N 根最高价）。
    pub channel_high: Option<Decimal>,
    /// 唐奇安通道下轨（前 N 根最低价）。
    pub channel_low: Option<Decimal>,
}

impl SandboxIndicators {
    /// 取指定周期的简单均线；未计算或数据不足时返回 `None`。
    pub fn sma(&self, period: u32) -> Option<Decimal> {
        self.sma_by_period.get(&period).copied()
    }

    /// 按 `SANDBOX_INDICATOR_*` 编号取值；缺数据返回 0。
    pub fn by_id(&self, id: i32) -> f64 {
        let value = match id {
            SANDBOX_INDICATOR_SMA_FAST => self.sma_fast,
            SANDBOX_INDICATOR_SMA_SLOW => self.sma_slow,
            SANDBOX_INDICATOR_EMA_FAST => self.ema_fast,
            SANDBOX_INDICATOR_EMA_SLOW => self.ema_slow,
            SANDBOX_INDICATOR_ATR => self.atr,
            SANDBOX_INDICATOR_CHANNEL_HIGH => self.channel_high,
            SANDBOX_INDICATOR_CHANNEL_LOW => self.channel_low,
            _ => None,
        };
        value.map(to_f64).unwrap_or(0.0)
    }
}

impl SandboxCandle {
    /// 按 `SANDBOX_CANDLE_*` 编号取值。
    pub fn field(&self, id: i32) -> f64 {
        let value = match id {
            SANDBOX_CANDLE_OPEN => self.open,
            SANDBOX_CANDLE_HIGH => self.high,
            SANDBOX_CANDLE_LOW => self.low,
            SANDBOX_CANDLE_CLOSE => self.close,
            SANDBOX_CANDLE_VOLUME => self.volume,
            SANDBOX_CANDLE_OPEN_TIME => Decimal::from(self.open_time.timestamp()),
            _ => Decimal::ZERO,
        };
        to_f64(value)
    }
}

/// 一条交给策略判断的事件。目前来自交易所上币公告。
///
/// 主机只提供已经发生的公开信息，事件本身不带交易能力；模块自行决定是否开仓。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxEvent {
    pub source: String,
    pub kind: String,
    pub title: String,
    /// 命中策略声明的交易对时给出该交易对，否则为空。
    #[serde(default)]
    pub symbol: Option<String>,
    pub published_at: DateTime<Utc>,
    /// 距本次评估的秒数，由主机预先算好，模块不需要自己处理时间。
    #[serde(default)]
    pub age_secs: u64,
}

impl SandboxEvent {
    /// 按 `SANDBOX_EVENT_*` 编号取值；缺数据返回 0。
    pub fn field(&self, id: i32) -> f64 {
        let value = match id {
            SANDBOX_EVENT_PUBLISHED_AT => Decimal::from(self.published_at.timestamp()),
            SANDBOX_EVENT_AGE_SECS => Decimal::from(self.age_secs),
            SANDBOX_EVENT_MATCHED => {
                if self.symbol.is_some() {
                    Decimal::ONE
                } else {
                    Decimal::ZERO
                }
            }
            _ => Decimal::ZERO,
        };
        to_f64(value)
    }

    /// 按 `SANDBOX_EVENT_TEXT_*` 编号取文本；未知编号返回空串。
    pub fn text(&self, id: i32) -> &str {
        match id {
            SANDBOX_EVENT_TEXT_SOURCE => self.source.as_str(),
            SANDBOX_EVENT_TEXT_KIND => self.kind.as_str(),
            SANDBOX_EVENT_TEXT_TITLE => self.title.as_str(),
            SANDBOX_EVENT_TEXT_SYMBOL => self.symbol.as_deref().unwrap_or_default(),
            _ => "",
        }
    }
}

/// 单个可交易市场（交易所 + 交易对 + 品种）的快照。
///
/// 字段全部为确定性数值，不含任何凭证；K 线与指标使用策略定义里的周期。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMarketSnapshot {
    pub symbol: String,
    pub venue: String,
    pub instrument: Instrument,
    pub bid: Decimal,
    pub ask: Decimal,
    /// 最新成交价；缺失时模块可退回用中间价。
    pub last: Option<Decimal>,
    /// 资金费率（仅永续有值）。
    pub funding_rate: Option<Decimal>,
    pub next_funding_at: Option<DateTime<Utc>>,
    /// 该市场按盘口深度可成交的最大名义价值。
    pub max_notional_usd: Decimal,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub candles: Vec<SandboxCandle>,
    #[serde(default)]
    pub indicators: SandboxIndicators,
}

impl SandboxMarketSnapshot {
    /// 按 `SANDBOX_FIELD_*` 编号取值；缺数据返回 0。
    pub fn field(&self, id: i32) -> f64 {
        let value = match id {
            SANDBOX_FIELD_BID => self.bid,
            SANDBOX_FIELD_ASK => self.ask,
            SANDBOX_FIELD_LAST => self
                .last
                .unwrap_or((self.bid + self.ask) / Decimal::from(2)),
            SANDBOX_FIELD_FUNDING_RATE => self.funding_rate.unwrap_or_default(),
            SANDBOX_FIELD_INSTRUMENT => match self.instrument {
                Instrument::Spot => Decimal::ZERO,
                Instrument::Perp => Decimal::ONE,
            },
            SANDBOX_FIELD_MAX_NOTIONAL_USD => self.max_notional_usd,
            _ => Decimal::ZERO,
        };
        to_f64(value)
    }
}

fn to_f64(value: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    value.to_f64().unwrap_or(0.0)
}

/// 当前持仓快照，避免模块自己维护状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPositionSnapshot {
    pub symbol: String,
    pub venue: String,
    pub side: PositionSide,
    pub notional_usd: Decimal,
    pub entry_price: Decimal,
    pub opened_at: DateTime<Utc>,
    pub held_secs: u64,
    /// 已持仓的 K 线根数，供按 bar 计数的策略使用。
    #[serde(default)]
    pub held_bars: u32,
    #[serde(default)]
    pub entry_net_edge_bps: Decimal,
}

/// 订单边界，模块输出的名义价值与杠杆会被限制在该范围内。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxOrderLimits {
    pub max_notional_usd: Decimal,
    pub max_leverage: Decimal,
    pub max_slippage_bps: Decimal,
    pub max_unhedged_secs: u64,
}

/// 传给模块的完整输入。主机以 JSON 写入模块线性内存。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxInput {
    pub abi_version: u32,
    pub strategy_id: Uuid,
    pub strategy_version: u32,
    pub now: DateTime<Utc>,
    pub limits: SandboxOrderLimits,
    pub markets: Vec<SandboxMarketSnapshot>,
    #[serde(default)]
    pub position: Option<SandboxPositionSnapshot>,
    /// 最近可见的事件，按发布时间升序；没有事件时为空。
    #[serde(default)]
    pub events: Vec<SandboxEvent>,
}

/// 模块可输出的唯一动作集合。没有撤单、转账、密钥或任意 HTTP 能力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SandboxDecision {
    /// 单腿方向性开仓；`venue` 为空时使用策略主交易所。
    Open {
        symbol: String,
        notional_usd: Decimal,
        side: PositionSide,
        #[serde(default)]
        venue: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// 资金费率配对开仓：现货多 + 永续空。
    OpenPair {
        symbol: String,
        notional_usd: Decimal,
        #[serde(default)]
        reason: Option<String>,
    },
    Close {
        symbol: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// 网格挂单：先登记一个待触发的限价意图，价格触及时才真正下单。
    ///
    /// 执行器不向交易所提交挂单，因此不需要撤单接口，也不会留下无人管理的挂单；
    /// 触发后的订单与普通开仓一样经过统一风控网关。
    PlaceLimit {
        symbol: String,
        side: PositionSide,
        price: Decimal,
        notional_usd: Decimal,
        #[serde(default)]
        venue: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// 撤销该策略在某交易对上的全部待触发限价意图（网格重排）。
    Cancel {
        symbol: String,
        #[serde(default)]
        reason: Option<String>,
    },
    Hold {
        #[serde(default)]
        reason: Option<String>,
    },
}

impl SandboxDecision {
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Self::Open { symbol, .. }
            | Self::OpenPair { symbol, .. }
            | Self::Close { symbol, .. }
            | Self::PlaceLimit { symbol, .. }
            | Self::Cancel { symbol, .. } => Some(symbol.as_str()),
            Self::Hold { .. } => None,
        }
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        match self {
            Self::Open {
                symbol,
                notional_usd,
                ..
            }
            | Self::OpenPair {
                symbol,
                notional_usd,
                ..
            } => {
                if symbol.trim().is_empty() {
                    issues.push(ValidationIssue {
                        path: "decisions.symbol".into(),
                        severity: ValidationSeverity::Error,
                        message: "开仓决策缺少交易对".into(),
                    });
                }
                if *notional_usd <= Decimal::ZERO {
                    issues.push(ValidationIssue {
                        path: "decisions.notional_usd".into(),
                        severity: ValidationSeverity::Error,
                        message: "开仓名义价值必须大于 0".into(),
                    });
                }
            }
            Self::Close { symbol, .. } => {
                if symbol.trim().is_empty() {
                    issues.push(ValidationIssue {
                        path: "decisions.symbol".into(),
                        severity: ValidationSeverity::Error,
                        message: "平仓决策缺少交易对".into(),
                    });
                }
            }
            Self::PlaceLimit {
                symbol,
                price,
                notional_usd,
                ..
            } => {
                if symbol.trim().is_empty() {
                    issues.push(ValidationIssue {
                        path: "decisions.symbol".into(),
                        severity: ValidationSeverity::Error,
                        message: "挂单决策缺少交易对".into(),
                    });
                }
                if *price <= Decimal::ZERO {
                    issues.push(ValidationIssue {
                        path: "decisions.price".into(),
                        severity: ValidationSeverity::Error,
                        message: "挂单价格必须大于 0".into(),
                    });
                }
                if *notional_usd <= Decimal::ZERO {
                    issues.push(ValidationIssue {
                        path: "decisions.notional_usd".into(),
                        severity: ValidationSeverity::Error,
                        message: "挂单名义价值必须大于 0".into(),
                    });
                }
            }
            Self::Cancel { symbol, .. } => {
                if symbol.trim().is_empty() {
                    issues.push(ValidationIssue {
                        path: "decisions.symbol".into(),
                        severity: ValidationSeverity::Error,
                        message: "撤单决策缺少交易对".into(),
                    });
                }
            }
            Self::Hold { .. } => {}
        }
        issues
    }
}

/// 模块通过 `emit` 提交的 JSON 结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxOutput {
    #[serde(default)]
    pub decisions: Vec<SandboxDecision>,
    #[serde(default)]
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxRunStatus {
    /// 正常运行，输出已通过结构校验。
    Ok,
    /// 模块可运行，但输出不合法（字段缺失、越界等）。
    Invalid,
    /// 模块内部返回错误或触发 trap。
    Error,
    /// 超出燃料或墙钟时间。
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRunReport {
    pub artifact_id: Uuid,
    pub artifact_hash: String,
    pub abi_version: u32,
    pub status: SandboxRunStatus,
    pub decisions: Vec<SandboxDecision>,
    pub logs: Vec<String>,
    pub fuel_limit: u64,
    pub fuel_consumed: u64,
    pub max_memory_bytes: usize,
    pub duration_ms: u64,
    pub output_bytes: usize,
    #[serde(default)]
    pub error: Option<String>,
    pub ran_at: DateTime<Utc>,
}

impl SandboxRunReport {
    pub fn failed(
        artifact: &SandboxArtifact,
        limits: &SandboxLimits,
        status: SandboxRunStatus,
        error: impl Into<String>,
        duration_ms: u64,
    ) -> Self {
        Self {
            artifact_id: artifact.id,
            artifact_hash: artifact.sha256.clone(),
            abi_version: SANDBOX_ABI_VERSION,
            status,
            decisions: Vec::new(),
            logs: Vec::new(),
            fuel_limit: limits.max_fuel,
            fuel_consumed: 0,
            max_memory_bytes: limits.max_memory_bytes,
            duration_ms,
            output_bytes: 0,
            error: Some(error.into()),
            ran_at: Utc::now(),
        }
    }
}

impl SandboxOutput {
    /// 截断超量决策与日志，并丢弃结构非法的决策。
    pub fn sanitize(mut self, limits: &SandboxLimits) -> (Self, Vec<ValidationIssue>) {
        let mut issues = Vec::new();
        if self.decisions.len() > limits.max_decisions {
            issues.push(ValidationIssue {
                path: "decisions".into(),
                severity: ValidationSeverity::Warning,
                message: format!(
                    "模块输出 {} 条决策，超过上限 {}，已截断",
                    self.decisions.len(),
                    limits.max_decisions
                ),
            });
            self.decisions.truncate(limits.max_decisions);
        }
        if self.logs.len() > limits.max_log_lines {
            self.logs.truncate(limits.max_log_lines);
        }
        self.logs.iter_mut().for_each(|line| {
            if line.chars().count() > 500 {
                *line = line.chars().take(500).collect();
            }
        });
        (self, issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn default_limits_are_valid() {
        assert!(!SandboxLimits::default().has_errors());
    }

    #[test]
    fn limits_reject_unbounded_fuel() {
        let limits = SandboxLimits {
            max_fuel: u64::MAX,
            ..SandboxLimits::default()
        };
        assert!(limits.has_errors());
    }

    #[test]
    fn output_truncates_decisions_and_logs() {
        let limits = SandboxLimits {
            max_decisions: 1,
            max_log_lines: 1,
            ..SandboxLimits::default()
        };
        let output = SandboxOutput {
            decisions: vec![
                SandboxDecision::Hold { reason: None },
                SandboxDecision::Open {
                    symbol: "BTCUSDT".into(),
                    notional_usd: dec!(100),
                    side: PositionSide::Long,
                    venue: None,
                    reason: None,
                },
            ],
            logs: vec!["a".into(), "b".into()],
        };
        let (sanitized, issues) = output.sanitize(&limits);
        assert_eq!(sanitized.decisions.len(), 1);
        assert_eq!(sanitized.logs.len(), 1);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn decision_rejects_zero_notional() {
        let decision = SandboxDecision::Open {
            symbol: "BTCUSDT".into(),
            notional_usd: Decimal::ZERO,
            side: PositionSide::Short,
            venue: None,
            reason: None,
        };
        assert!(!decision.validate().is_empty());
    }

    #[test]
    fn missing_indicators_read_as_zero() {
        let indicators = SandboxIndicators::default();
        assert_eq!(indicators.by_id(SANDBOX_INDICATOR_ATR), 0.0);
        assert_eq!(indicators.by_id(SANDBOX_INDICATOR_CHANNEL_HIGH), 0.0);
    }

    #[test]
    fn open_pair_requires_positive_notional() {
        let decision = SandboxDecision::OpenPair {
            symbol: "BTCUSDT".into(),
            notional_usd: dec!(0),
            reason: None,
        };
        assert!(!decision.validate().is_empty());
    }

    #[test]
    fn place_limit_requires_price_and_notional() {
        let decision = SandboxDecision::PlaceLimit {
            symbol: "BTCUSDT".into(),
            side: PositionSide::Long,
            price: dec!(0),
            notional_usd: dec!(100),
            venue: None,
            reason: None,
        };
        assert!(!decision.validate().is_empty());

        let valid = SandboxDecision::PlaceLimit {
            symbol: "BTCUSDT".into(),
            side: PositionSide::Long,
            price: dec!(9_500),
            notional_usd: dec!(100),
            venue: None,
            reason: None,
        };
        assert!(valid.validate().is_empty());
        assert_eq!(valid.symbol(), Some("BTCUSDT"));
    }

    #[test]
    fn market_field_exposes_quote_and_instrument() {
        let market = SandboxMarketSnapshot {
            symbol: "BTCUSDT".into(),
            venue: "binance".into(),
            instrument: Instrument::Perp,
            bid: dec!(100),
            ask: dec!(101),
            last: None,
            funding_rate: Some(dec!(0.0005)),
            next_funding_at: None,
            max_notional_usd: dec!(1000),
            observed_at: Utc::now(),
            candles: Vec::new(),
            indicators: SandboxIndicators::default(),
        };
        assert_eq!(market.field(SANDBOX_FIELD_BID), 100.0);
        assert_eq!(market.field(SANDBOX_FIELD_LAST), 100.5);
        assert_eq!(market.field(SANDBOX_FIELD_FUNDING_RATE), 0.0005);
        assert_eq!(market.field(SANDBOX_FIELD_INSTRUMENT), 1.0);
    }

    #[test]
    fn artifact_requires_matching_abi() {
        let artifact = SandboxArtifact {
            id: Uuid::new_v4(),
            name: "test".into(),
            sha256: "abc".into(),
            size_bytes: 1024,
            abi_version: Some(SANDBOX_ABI_VERSION),
            source: SandboxArtifactSource::Uploaded,
            notes: None,
            created_at: Utc::now(),
        };
        assert!(artifact.validate(&SandboxLimits::default()).is_empty());

        let wrong = SandboxArtifact {
            abi_version: Some(SANDBOX_ABI_VERSION + 1),
            ..artifact
        };
        assert!(!wrong.validate(&SandboxLimits::default()).is_empty());
    }
}
