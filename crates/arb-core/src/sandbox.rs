//! WASM 策略沙箱的受限 ABI 与运行结果类型。
//!
//! 沙箱只接收确定性的行情快照，只输出「开仓 / 平仓 / 保持」意图：
//! - 模块无法访问 WASI、文件系统、网络、环境变量或任何交易所密钥；
//! - 主机限制燃料、内存、墙钟时间、输出大小与决策条数；
//! - 输出意图仍必须经过执行器统一风控网关，沙箱本身没有下单能力。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::strategy::{ValidationIssue, ValidationSeverity};

/// 当前沙箱 ABI 版本。模块导出 `abi_version()` 必须返回该值。
pub const SANDBOX_ABI_VERSION: u32 = 1;
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
pub const SANDBOX_POSITION_PRESENT: &str = "position_present";
pub const SANDBOX_POSITION_NOTIONAL: &str = "position_notional";
pub const SANDBOX_POSITION_HELD_SECS: &str = "position_held_secs";
pub const SANDBOX_OPEN: &str = "open";
pub const SANDBOX_CLOSE: &str = "close";
pub const SANDBOX_HOLD: &str = "hold";

/// `market(index, field)` 的字段编号。
pub const SANDBOX_FIELD_SPOT_BID: i32 = 0;
pub const SANDBOX_FIELD_SPOT_ASK: i32 = 1;
pub const SANDBOX_FIELD_PERP_BID: i32 = 2;
pub const SANDBOX_FIELD_PERP_ASK: i32 = 3;
pub const SANDBOX_FIELD_FUNDING_RATE: i32 = 4;
pub const SANDBOX_FIELD_GROSS_SPREAD_BPS: i32 = 5;
pub const SANDBOX_FIELD_NET_EDGE_BPS: i32 = 6;
pub const SANDBOX_FIELD_MAX_NOTIONAL_USD: i32 = 7;

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
            issues.push(error(
                "max_fuel",
                "燃料上限必须在 1 到 5000 万之间",
            ));
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

/// 单条行情快照，字段全部为确定性数值，不含任何凭证。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMarketSnapshot {
    pub symbol: String,
    pub spot_venue: String,
    pub perp_venue: String,
    pub spot_bid: Decimal,
    pub spot_ask: Decimal,
    pub perp_bid: Decimal,
    pub perp_ask: Decimal,
    /// 当期资金费率（小数，如 0.0003）。
    pub funding_rate: Decimal,
    pub gross_spread_bps: Decimal,
    pub net_edge_bps: Decimal,
    pub next_funding_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    /// 该机会按盘口深度可成交的最大名义价值。
    pub max_notional_usd: Decimal,
}

/// 当前持仓快照，避免模块自己维护状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPositionSnapshot {
    pub symbol: String,
    pub notional_usd: Decimal,
    pub opened_at: DateTime<Utc>,
    pub held_secs: u64,
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
}

/// 模块可输出的唯一动作集合。没有撤单、转账、密钥或任意 HTTP 能力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SandboxDecision {
    Open {
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
    Hold {
        #[serde(default)]
        reason: Option<String>,
    },
}

impl SandboxDecision {
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Self::Open { symbol, .. } | Self::Close { symbol, .. } => Some(symbol.as_str()),
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
            reason: None,
        };
        assert!(!decision.validate().is_empty());
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
