//! 内置策略模板：海龟突破、均线趋势、网格、上币公告跟随。
//!
//! 模板（定义 + WAT 源码）存在数据库里，启动时补齐缺失模板并把源码编译成沙箱
//! 模块入库。实例化只是复制定义并绑定同一个模块；运行时模块输出的意图仍然要
//! 经过统一风控网关，模板本身没有任何下单或密钥能力。

use arb_core::{
    CostAssumptions, DataRules, EntryRules, ExecutionRules, ExitRules, Instrument, SandboxArtifact,
    SandboxArtifactSource, StrategyDefinition, StrategyKind, StrategyRisk, StrategyUniverse,
    ValidationSeverity,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{sandbox, state::AppState};

/// 一个可直接实例化的内置策略模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyTemplate {
    /// 稳定标识，重复启动时据此幂等写入。
    pub key: String,
    pub name: String,
    pub description: String,
    /// WebAssembly Text 源码；数据库是唯一事实来源。
    pub source: String,
    /// 实例化时复制的默认定义。
    pub definition: StrategyDefinition,
    /// 由上面的源码编译得到的沙箱模块。
    #[serde(default)]
    pub artifact_id: Option<Uuid>,
    #[serde(default)]
    pub artifact_sha256: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// 海龟突破：价格突破唐奇安通道做多/做空，反向突破或 2 倍 ATR 止损离场。
pub const TURTLE_WAT: &str = r#";; ArbPilot 内置模板：海龟突破（唐奇安通道）
;; - 空仓：价格突破前 N 根最高价做多，跌破前 N 根最低价做空；
;; - 持仓：反向突破通道另一侧离场，浮亏达到 2 倍 ATR 时止损。
;; 一次只持有一条腿，所有意图都必须经过统一风控网关。
(module
  (import "arbpilot" "markets" (func $markets (result i32)))
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "market_symbol" (func $market_symbol (param i32 i32 i32) (result i32)))
  (import "arbpilot" "indicator" (func $indicator (param i32 i32) (result f64)))
  (import "arbpilot" "position_present" (func $position_present (result i32)))
  (import "arbpilot" "position_side" (func $position_side (result i32)))
  (import "arbpilot" "position_entry_price" (func $position_entry_price (result f64)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "close" (func $close (param i32 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))

  (memory (export "memory") 1)
  ;; 交易对名称缓冲区：由 market_symbol 在运行期写入。
  (data (i32.const 1024) "                        ")
  (global $symbol_ptr i32 (i32.const 1024))
  (global $symbol_cap i32 (i32.const 24))
  ;; 止损距离：2 倍 ATR。
  (global $stop_atr f64 (f64.const 2))

  (global $heap (mut i32) (i32.const 8192))
  (func (export "abi_version") (result i32) i32.const 2)

  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    local.get $len
    i32.const 65536
    i32.div_u
    i32.const 1
    i32.add
    memory.grow
    drop
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr)

  (func (export "evaluate") (param $ptr i32) (param $len i32) (result i32)
    (local $symbol_len i32)
    (local $last f64)
    (local $high f64)
    (local $low f64)
    (local $atr f64)
    (local $side i32)
    (local $entry f64)

    (local.set $symbol_len
      (call $market_symbol
        (i32.const 0)
        (global.get $symbol_ptr)
        (global.get $symbol_cap)))
    (if (i32.le_s (local.get $symbol_len) (i32.const 0))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; indicator 5/6 = 唐奇安通道上/下轨，4 = ATR；数据不足时为 0。
    (local.set $high (call $indicator (i32.const 0) (i32.const 5)))
    (local.set $low (call $indicator (i32.const 0) (i32.const 6)))
    (local.set $atr (call $indicator (i32.const 0) (i32.const 4)))
    (local.set $last (call $market (i32.const 0) (i32.const 2)))

    (if (i32.or
          (f64.le (local.get $high) (f64.const 0))
          (f64.le (local.get $low) (f64.const 0)))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 空仓：突破通道即入场，名义价值取主机给出的上限。
    (if (i32.eqz (call $position_present))
      (then
        (if (f64.gt (local.get $last) (local.get $high))
          (then
            (drop
              (call $open
                (global.get $symbol_ptr)
                (local.get $symbol_len)
                (call $market (i32.const 0) (i32.const 5))
                (i32.const 1)))))
        (if (f64.lt (local.get $last) (local.get $low))
          (then
            (drop
              (call $open
                (global.get $symbol_ptr)
                (local.get $symbol_len)
                (call $market (i32.const 0) (i32.const 5))
                (i32.const -1)))))
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 持仓：反向突破通道另一侧，或浮亏达到 2 倍 ATR 时离场。
    (local.set $side (call $position_side))
    (local.set $entry (call $position_entry_price))

    (if (i32.gt_s (local.get $side) (i32.const 0))
      (then
        (if (i32.or
              (f64.lt (local.get $last) (local.get $low))
              (i32.and
                (f64.gt (local.get $atr) (f64.const 0))
                (f64.lt
                  (local.get $last)
                  (f64.sub
                    (local.get $entry)
                    (f64.mul (global.get $stop_atr) (local.get $atr))))))
          (then
            (drop (call $close (global.get $symbol_ptr) (local.get $symbol_len)))))))

    (if (i32.lt_s (local.get $side) (i32.const 0))
      (then
        (if (i32.or
              (f64.gt (local.get $last) (local.get $high))
              (i32.and
                (f64.gt (local.get $atr) (f64.const 0))
                (f64.gt
                  (local.get $last)
                  (f64.add
                    (local.get $entry)
                    (f64.mul (global.get $stop_atr) (local.get $atr))))))
          (then
            (drop (call $close (global.get $symbol_ptr) (local.get $symbol_len)))))))

    (drop (call $hold))
    i32.const 0))
"#;

/// 均线趋势：快慢均线多头排列且价格在快线上方做多，空头排列做空，均线翻转离场。
pub const MA_TREND_WAT: &str = r#";; ArbPilot 内置模板：均线趋势
;; - 空仓：快均线在慢均线上方且价格站上快线做多；反之做空；
;; - 持仓：快慢均线重新交叉（趋势翻转）时离场。
(module
  (import "arbpilot" "markets" (func $markets (result i32)))
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "market_symbol" (func $market_symbol (param i32 i32 i32) (result i32)))
  (import "arbpilot" "indicator" (func $indicator (param i32 i32) (result f64)))
  (import "arbpilot" "position_present" (func $position_present (result i32)))
  (import "arbpilot" "position_side" (func $position_side (result i32)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "close" (func $close (param i32 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 1024) "                        ")
  (global $symbol_ptr i32 (i32.const 1024))
  (global $symbol_cap i32 (i32.const 24))

  (global $heap (mut i32) (i32.const 8192))
  (func (export "abi_version") (result i32) i32.const 2)

  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    local.get $len
    i32.const 65536
    i32.div_u
    i32.const 1
    i32.add
    memory.grow
    drop
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr)

  (func (export "evaluate") (param $ptr i32) (param $len i32) (result i32)
    (local $symbol_len i32)
    (local $last f64)
    (local $fast f64)
    (local $slow f64)
    (local $side i32)

    (local.set $symbol_len
      (call $market_symbol
        (i32.const 0)
        (global.get $symbol_ptr)
        (global.get $symbol_cap)))
    (if (i32.le_s (local.get $symbol_len) (i32.const 0))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; indicator 0/1 = 快/慢简单均线；K 线不足时返回 0。
    (local.set $fast (call $indicator (i32.const 0) (i32.const 0)))
    (local.set $slow (call $indicator (i32.const 0) (i32.const 1)))
    (local.set $last (call $market (i32.const 0) (i32.const 2)))

    (if (i32.or
          (f64.le (local.get $fast) (f64.const 0))
          (f64.le (local.get $slow) (f64.const 0)))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    (if (i32.eqz (call $position_present))
      (then
        (if (i32.and
              (f64.gt (local.get $fast) (local.get $slow))
              (f64.gt (local.get $last) (local.get $fast)))
          (then
            (drop
              (call $open
                (global.get $symbol_ptr)
                (local.get $symbol_len)
                (call $market (i32.const 0) (i32.const 5))
                (i32.const 1)))))
        (if (i32.and
              (f64.lt (local.get $fast) (local.get $slow))
              (f64.lt (local.get $last) (local.get $fast)))
          (then
            (drop
              (call $open
                (global.get $symbol_ptr)
                (local.get $symbol_len)
                (call $market (i32.const 0) (i32.const 5))
                (i32.const -1)))))
        (drop (call $hold))
        (return (i32.const 0))))

    (local.set $side (call $position_side))

    (if (i32.gt_s (local.get $side) (i32.const 0))
      (then
        (if (f64.lt (local.get $fast) (local.get $slow))
          (then
            (drop (call $close (global.get $symbol_ptr) (local.get $symbol_len)))))))

    (if (i32.lt_s (local.get $side) (i32.const 0))
      (then
        (if (f64.gt (local.get $fast) (local.get $slow))
          (then
            (drop (call $close (global.get $symbol_ptr) (local.get $symbol_len)))))))

    (drop (call $hold))
    i32.const 0))
"#;

/// 网格：每轮先撤销旧档位，再在现价上下挂出影子限价单；触及后才真正下单。
pub const GRID_WAT: &str = r#";; ArbPilot 内置模板：网格
;; - 每轮评估先撤销该交易对的旧挂单，再按最新价格重排档位；
;; - 现价下方挂买单、上方挂卖单；持多仓只留卖档，持空仓只留买档；
;; - 挂单只是本地的待触发意图，价格触及后才会经统一风控网关下真实订单。
(module
  (import "arbpilot" "markets" (func $markets (result i32)))
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "market_symbol" (func $market_symbol (param i32 i32 i32) (result i32)))
  (import "arbpilot" "position_present" (func $position_present (result i32)))
  (import "arbpilot" "position_side" (func $position_side (result i32)))
  (import "arbpilot" "limit" (func $limit (param i32 i32 f64 f64 i32) (result i32)))
  (import "arbpilot" "cancel" (func $cancel (param i32 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 1024) "                        ")
  (global $symbol_ptr i32 (i32.const 1024))
  (global $symbol_cap i32 (i32.const 24))
  ;; 每档间距 0.5%，每档使用可用名义额度的 25%。
  (global $step f64 (f64.const 0.005))
  (global $share f64 (f64.const 0.25))

  (global $heap (mut i32) (i32.const 8192))
  (func (export "abi_version") (result i32) i32.const 2)

  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    local.get $len
    i32.const 65536
    i32.div_u
    i32.const 1
    i32.add
    memory.grow
    drop
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr)

  ;; 现价下方挂买单（做多入场）。
  (func $place_buys
      (param $last f64) (param $notional f64) (param $count i32) (param $symbol_len i32)
    (local $i i32)
    (local.set $i (i32.const 1))
    (block $done
      (loop $levels
        (br_if $done (i32.gt_s (local.get $i) (local.get $count)))
        (drop
          (call $limit
            (global.get $symbol_ptr)
            (local.get $symbol_len)
            (f64.mul
              (local.get $last)
              (f64.sub
                (f64.const 1)
                (f64.mul
                  (global.get $step)
                  (f64.convert_i32_s (local.get $i)))))
            (local.get $notional)
            (i32.const 1)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $levels))))

  ;; 现价上方挂卖单（做空入场，或平掉多头）。
  (func $place_sells
      (param $last f64) (param $notional f64) (param $count i32) (param $symbol_len i32)
    (local $i i32)
    (local.set $i (i32.const 1))
    (block $done
      (loop $levels
        (br_if $done (i32.gt_s (local.get $i) (local.get $count)))
        (drop
          (call $limit
            (global.get $symbol_ptr)
            (local.get $symbol_len)
            (f64.mul
              (local.get $last)
              (f64.add
                (f64.const 1)
                (f64.mul
                  (global.get $step)
                  (f64.convert_i32_s (local.get $i)))))
            (local.get $notional)
            (i32.const -1)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $levels))))

  (func (export "evaluate") (param $ptr i32) (param $len i32) (result i32)
    (local $symbol_len i32)
    (local $present i32)
    (local $side i32)
    (local $last f64)
    (local $notional f64)

    (local.set $symbol_len
      (call $market_symbol
        (i32.const 0)
        (global.get $symbol_ptr)
        (global.get $symbol_cap)))
    (if (i32.le_s (local.get $symbol_len) (i32.const 0))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 重排前先撤掉旧档位，避免档位随价格漂移无限堆积。
    (drop (call $cancel (global.get $symbol_ptr) (local.get $symbol_len)))

    (local.set $last (call $market (i32.const 0) (i32.const 2)))
    (local.set $notional
      (f64.mul
        (call $market (i32.const 0) (i32.const 5))
        (global.get $share)))

    (if (i32.or
          (f64.le (local.get $last) (f64.const 0))
          (f64.le (local.get $notional) (f64.const 0)))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    (local.set $present (call $position_present))
    (local.set $side (call $position_side))

    (if (i32.eqz (local.get $present))
      (then
        (call $place_buys
          (local.get $last) (local.get $notional) (i32.const 3) (local.get $symbol_len))
        (call $place_sells
          (local.get $last) (local.get $notional) (i32.const 3) (local.get $symbol_len)))
      (else
        (if (i32.gt_s (local.get $side) (i32.const 0))
          (then
            (call $place_sells
              (local.get $last) (local.get $notional) (i32.const 1) (local.get $symbol_len)))
          (else
            (call $place_buys
              (local.get $last) (local.get $notional) (i32.const 1) (local.get $symbol_len))))))

    (drop (call $hold))
    i32.const 0))
"#;

/// 上币公告跟随：交易所发布命中交易对的上币公告后立即做多，超时离场。
///
/// 主机只注入策略声明来源、关键词与时间窗口内的公告；模块看到的事件已经按交易对
/// 过滤，因此只需要判断最新一条是否足够新。
pub const LISTING_FOLLOW_WAT: &str = r#";; ArbPilot 内置模板：上币公告跟随
;; - 空仓：最近一条公告在 15 分钟内且命中交易对 → 市价做多；
;; - 持仓：达到最长持有时长（6 小时）主动离场；
;; 一次只持有一条腿，所有意图都必须经过统一风控网关。
(module
  (import "arbpilot" "market" (func $market (param i32 i32) (result f64)))
  (import "arbpilot" "market_symbol" (func $market_symbol (param i32 i32 i32) (result i32)))
  (import "arbpilot" "events" (func $events (result i32)))
  (import "arbpilot" "event" (func $event (param i32 i32) (result f64)))
  (import "arbpilot" "position_present" (func $position_present (result i32)))
  (import "arbpilot" "position_held_secs" (func $position_held_secs (result i64)))
  (import "arbpilot" "open" (func $open (param i32 i32 f64 i32) (result i32)))
  (import "arbpilot" "close" (func $close (param i32 i32) (result i32)))
  (import "arbpilot" "hold" (func $hold (result i32)))

  (memory (export "memory") 1)
  ;; 交易对名称缓冲区：由 market_symbol 在运行期写入。
  (data (i32.const 1024) "                        ")
  (global $symbol_ptr i32 (i32.const 1024))
  (global $symbol_cap i32 (i32.const 24))
  ;; 公告有效期与最长持仓时间（秒）。
  (global $event_max_age f64 (f64.const 900))
  (global $max_hold_secs i64 (i64.const 21600))

  (global $heap (mut i32) (i32.const 8192))
  (func (export "abi_version") (result i32) i32.const 2)

  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    local.get $len
    i32.const 65536
    i32.div_u
    i32.const 1
    i32.add
    memory.grow
    drop
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr)

  (func (export "evaluate") (param $ptr i32) (param $len i32) (result i32)
    (local $symbol_len i32)
    (local $last f64)
    (local $notional f64)
    (local $count i32)
    (local $latest i32)

    (local.set $symbol_len
      (call $market_symbol
        (i32.const 0)
        (global.get $symbol_ptr)
        (global.get $symbol_cap)))
    (if (i32.le_s (local.get $symbol_len) (i32.const 0))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; market 字段 2 = 最新价，5 = 本笔可用名义价值上限。
    (local.set $last (call $market (i32.const 0) (i32.const 2)))
    (local.set $notional (call $market (i32.const 0) (i32.const 5)))
    (if (i32.or
          (f64.le (local.get $last) (f64.const 0))
          (f64.le (local.get $notional) (f64.const 0)))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 持仓：只做离场判断，达到最长持有时长就平仓。
    (if (call $position_present)
      (then
        (if (i64.ge_s (call $position_held_secs) (global.get $max_hold_secs))
          (then
            (drop (call $close (global.get $symbol_ptr) (local.get $symbol_len)))))
        (drop (call $hold))
        (return (i32.const 0))))

    (local.set $count (call $events))
    (if (i32.le_s (local.get $count) (i32.const 0))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    ;; 取最新一条公告：字段 2 表示命中交易对（1/0），字段 1 是距今秒数。
    (local.set $latest (i32.sub (local.get $count) (i32.const 1)))
    (if (i32.eqz (i32.trunc_f64_s (call $event (local.get $latest) (i32.const 2))))
      (then
        (drop (call $hold))
        (return (i32.const 0))))
    (if (f64.gt (call $event (local.get $latest) (i32.const 1)) (global.get $event_max_age))
      (then
        (drop (call $hold))
        (return (i32.const 0))))

    (drop
      (call $open
        (global.get $symbol_ptr)
        (local.get $symbol_len)
        (local.get $notional)
        (i32.const 1)))
    (drop (call $hold))
    i32.const 0))
"#;

/// 四个内置模板的默认集合。
pub fn builtin() -> Vec<StrategyTemplate> {
    let now = Utc::now();
    vec![
        StrategyTemplate {
            key: "turtle-breakout".to_string(),
            name: "海龟突破".to_string(),
            description: "价格突破唐奇安通道入场，反向突破或 2 倍 ATR 止损离场。".to_string(),
            source: TURTLE_WAT.to_string(),
            definition: definition(
                "海龟突破",
                "空仓时突破前 N 根最高价做多、跌破最低价做空；持仓时反向突破或用 2 倍 ATR 止损。",
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
            artifact_id: None,
            artifact_sha256: None,
            updated_at: now,
        },
        StrategyTemplate {
            key: "ma-trend".to_string(),
            name: "均线趋势".to_string(),
            description: "快慢均线多头排列做多、空头排列做空，均线反转离场。".to_string(),
            source: MA_TREND_WAT.to_string(),
            definition: definition(
                "均线趋势",
                "快均线在慢均线上方且价格站上快线做多，反之做空；快慢均线重新交叉时离场。",
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
            artifact_id: None,
            artifact_sha256: None,
            updated_at: now,
        },
        StrategyTemplate {
            key: "grid".to_string(),
            name: "网格".to_string(),
            description: "现价上下按固定间距挂影子限价单，价格触及后才真正下单。".to_string(),
            source: GRID_WAT.to_string(),
            definition: definition(
                "网格",
                "每轮先撤销旧挂单，再按最新价格在上下方挂出 3 档买单与 3 档卖单；\
                 单档为可用名义额度的 25%，间距 0.5%。",
                DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 200,
                    ..DataRules::default()
                },
            ),
            artifact_id: None,
            artifact_sha256: None,
            updated_at: now,
        },
        StrategyTemplate {
            key: "listing-follow".to_string(),
            name: "上币公告跟随".to_string(),
            description: "交易所发布上币公告后买入该交易对，持有到期后离场。".to_string(),
            source: LISTING_FOLLOW_WAT.to_string(),
            definition: definition(
                "上币公告跟随",
                "主机只注入最近一小时内、命中本策略交易对的上币公告；\
                 公告发布后 15 分钟内市价做多，持有 6 小时后平仓。",
                DataRules {
                    timeframe: "5m".to_string(),
                    lookback: 200,
                    event_sources: Vec::new(),
                    event_keywords: Vec::new(),
                    event_window_secs: 3_600,
                    ..DataRules::default()
                },
            ),
            artifact_id: None,
            artifact_sha256: None,
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

/// 启动时补齐模板并确保每个模板都有可执行模块。
///
/// 数据库里的模板是唯一事实来源：已存在的模板不会整体被内置定义覆盖，只在缺少
/// 编译产物（例如清空过模块表）时按库里的源码重新编译；内置模板不可在界面编辑，
/// 因此名称始终跟随代码，避免旧库停留在历史命名。
pub async fn seed(state: &AppState) {
    let builtins = builtin();
    let mut stored = state.store.strategy_templates().await;
    for template in &builtins {
        match stored.iter_mut().find(|saved| saved.key == template.key) {
            Some(saved) => {
                if saved.name != template.name
                    || saved.definition.name != template.definition.name
                {
                    saved.name = template.name.clone();
                    saved.definition.name = template.definition.name.clone();
                    let updated = saved.clone();
                    state.store.save_strategy_template(&updated).await;
                }
            }
            None => {
                state.store.save_strategy_template(template).await;
                stored.push(template.clone());
            }
        }
    }

    for mut template in stored {
        let artifact = match compile(state, &template).await {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::error!(%error, key = %template.key, "编译内置策略模板失败");
                continue;
            }
        };
        if template.artifact_id != Some(artifact.id) {
            template.artifact_id = Some(artifact.id);
            template.artifact_sha256 = Some(artifact.sha256);
            template.updated_at = Utc::now();
            state.store.save_strategy_template(&template).await;
        }
    }

    rename_legacy_strategies(state, &builtins).await;
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
        let Some((_, name)) = legacy.iter().find(|(old, _)| *old == record.definition.name) else {
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

/// 把模板源码编译成沙箱模块并入库；相同 sha256 复用已有模块。
pub async fn compile(
    state: &AppState,
    template: &StrategyTemplate,
) -> anyhow::Result<SandboxArtifact> {
    if template.source.len() > MAX_SOURCE_BYTES {
        anyhow::bail!(
            "模板源码 {} 字节，超过上限 {MAX_SOURCE_BYTES} 字节",
            template.source.len()
        );
    }
    let limits = state.config.sandbox;
    let wasm = wat::parse_str(&template.source)?;
    if wasm.len() > limits.max_artifact_bytes {
        anyhow::bail!(
            "模板编译产物 {} 字节，超过沙箱上限 {} 字节",
            wasm.len(),
            limits.max_artifact_bytes
        );
    }

    let hash = sandbox::hash_wasm(&wasm);
    if let Some(existing) = state
        .store
        .sandbox_artifacts()
        .await
        .into_iter()
        .find(|artifact| artifact.sha256 == hash)
    {
        return Ok(existing);
    }

    let abi_version = sandbox::inspect(&wasm, &limits).map_err(anyhow::Error::msg)?;
    let artifact = SandboxArtifact {
        id: Uuid::new_v4(),
        name: format!("template-{}", template.key),
        sha256: hash,
        size_bytes: wasm.len(),
        abi_version: Some(abi_version),
        source: SandboxArtifactSource::BuiltIn,
        notes: Some("内置策略模板".to_string()),
        created_at: Utc::now(),
    };
    if let Some(issue) = artifact
        .validate(&limits)
        .into_iter()
        .find(|issue| issue.severity == ValidationSeverity::Error)
    {
        anyhow::bail!("{}: {}", issue.path, issue.message);
    }

    state.store.save_sandbox_artifact(&artifact, &wasm).await?;
    Ok(artifact)
}

/// 模板源码最大长度，与策略代码保持一致。
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::{
        Instrument, PositionSide, SandboxCandle, SandboxDecision, SandboxEvent, SandboxIndicators,
        SandboxInput, SandboxLimits, SandboxMarketSnapshot, SandboxOrderLimits, SandboxRunReport,
        SandboxRunStatus,
    };
    use rust_decimal_macros::dec;

    fn artifact(name: &str, bytes: &[u8]) -> SandboxArtifact {
        SandboxArtifact {
            id: Uuid::new_v4(),
            name: name.to_string(),
            sha256: sandbox::hash_wasm(bytes),
            size_bytes: bytes.len(),
            abi_version: Some(arb_core::SANDBOX_ABI_VERSION),
            source: SandboxArtifactSource::BuiltIn,
            notes: None,
            created_at: Utc::now(),
        }
    }

    /// 合成一段行情：只填模板真正读取的字段。
    fn input(last: Decimal, max_notional: Decimal, indicators: SandboxIndicators) -> SandboxInput {
        SandboxInput {
            abi_version: arb_core::SANDBOX_ABI_VERSION,
            strategy_id: Uuid::new_v4(),
            strategy_version: 1,
            now: Utc::now(),
            limits: SandboxOrderLimits {
                max_notional_usd: dec!(1000),
                max_leverage: Decimal::ONE,
                max_slippage_bps: dec!(10),
                max_unhedged_secs: 3,
            },
            markets: vec![SandboxMarketSnapshot {
                symbol: "BTCUSDT".into(),
                venue: "binance".into(),
                instrument: Instrument::Perp,
                bid: last,
                ask: last,
                last: Some(last),
                funding_rate: None,
                next_funding_at: None,
                max_notional_usd: max_notional,
                observed_at: Utc::now(),
                candles: vec![SandboxCandle {
                    open_time: Utc::now(),
                    open: last,
                    high: last,
                    low: last,
                    close: last,
                    volume: Decimal::ONE,
                }],
                indicators,
            }],
            position: None,
            events: Vec::new(),
        }
    }

    async fn run(source: &str, input: SandboxInput) -> SandboxRunReport {
        let bytes = wat::parse_str(source).expect("wat 编译");
        sandbox::inspect(&bytes, &SandboxLimits::default()).expect("沙箱检查");
        sandbox::evaluate(
            artifact("template-test", &bytes),
            bytes,
            input,
            SandboxLimits::default(),
        )
        .await
    }

    fn only_decision(report: &SandboxRunReport) -> &SandboxDecision {
        report
            .decisions
            .iter()
            .find(|decision| !matches!(decision, SandboxDecision::Hold { .. }))
            .unwrap_or_else(|| panic!("模板没有产出决策: {:?}", report.logs))
    }

    #[test]
    fn builtin_templates_compile_and_pass_sandbox_checks() {
        for template in builtin() {
            assert!(template.source.len() <= MAX_SOURCE_BYTES);
            let wasm = wat::parse_str(&template.source)
                .unwrap_or_else(|error| panic!("{} 模板无法编译: {error}", template.key));
            let abi = sandbox::inspect(&wasm, &arb_core::SandboxLimits::default())
                .unwrap_or_else(|error| panic!("{} 未通过沙箱检查: {error}", template.key));
            assert_eq!(abi, arb_core::SANDBOX_ABI_VERSION);
        }
    }

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

    /// 海龟模板：价格突破通道上轨 → 做多，名义价值取主机上限。
    #[tokio::test]
    async fn turtle_template_opens_long_on_breakout() {
        let report = run(
            TURTLE_WAT,
            input(
                dec!(101),
                dec!(500),
                SandboxIndicators {
                    channel_high: Some(dec!(100)),
                    channel_low: Some(dec!(90)),
                    atr: Some(dec!(2)),
                    ..SandboxIndicators::default()
                },
            ),
        )
        .await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert_eq!(
            only_decision(&report),
            &SandboxDecision::Open {
                symbol: "BTCUSDT".into(),
                notional_usd: dec!(500),
                side: PositionSide::Long,
                venue: None,
                reason: None,
            }
        );
    }

    /// 均线模板：快线在慢线上方且价格站上快线 → 做多。
    #[tokio::test]
    async fn ma_trend_template_opens_long() {
        let report = run(
            MA_TREND_WAT,
            input(
                dec!(102),
                dec!(500),
                SandboxIndicators {
                    sma_fast: Some(dec!(101)),
                    sma_slow: Some(dec!(100)),
                    ..SandboxIndicators::default()
                },
            ),
        )
        .await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        match only_decision(&report) {
            SandboxDecision::Open {
                side, notional_usd, ..
            } => {
                assert_eq!(*side, PositionSide::Long);
                assert_eq!(*notional_usd, dec!(500));
            }
            other => panic!("期望开仓决策，实际 {other:?}"),
        }
    }

    /// 网格模板：空仓时在现价上下各挂三档；每档使用可用名义额度的 25%。
    #[tokio::test]
    async fn grid_template_places_limit_ladders() {
        let report = run(
            GRID_WAT,
            input(dec!(100), dec!(400), SandboxIndicators::default()),
        )
        .await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);

        let mut buys = Vec::new();
        let mut sells = Vec::new();
        let mut cancelled = false;
        for decision in &report.decisions {
            match decision {
                SandboxDecision::PlaceLimit {
                    side,
                    price,
                    notional_usd,
                    symbol,
                    ..
                } => {
                    assert_eq!(symbol, "BTCUSDT");
                    assert_eq!(*notional_usd, dec!(100));
                    match side {
                        PositionSide::Long => buys.push(*price),
                        PositionSide::Short => sells.push(*price),
                    }
                }
                SandboxDecision::Cancel { symbol, .. } => {
                    assert_eq!(symbol, "BTCUSDT");
                    cancelled = true;
                }
                _ => {}
            }
        }

        assert!(cancelled, "每轮重排前应先撤销旧挂单");
        assert_eq!(buys.len(), 3);
        assert_eq!(sells.len(), 3);
        assert!(buys.iter().all(|price| *price < dec!(100)));
        assert!(sells.iter().all(|price| *price > dec!(100)));
    }

    fn event(published_at: chrono::DateTime<Utc>, age_secs: u64) -> SandboxEvent {
        SandboxEvent {
            source: "binance".to_string(),
            kind: "listing".to_string(),
            title: "Binance Will List XYZ".to_string(),
            symbol: Some("BTCUSDT".to_string()),
            published_at,
            age_secs,
        }
    }

    /// 上币公告模板：命中交易对的新公告 → 市价做多。
    #[tokio::test]
    async fn listing_follow_template_opens_on_fresh_event() {
        let now = Utc::now();
        let mut input = input(dec!(100), dec!(500), SandboxIndicators::default());
        input.events = vec![event(now, 60)];

        let report = run(LISTING_FOLLOW_WAT, input).await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert_eq!(
            only_decision(&report),
            &SandboxDecision::Open {
                symbol: "BTCUSDT".into(),
                notional_usd: dec!(500),
                side: PositionSide::Long,
                venue: None,
                reason: None,
            }
        );
    }

    /// 公告过期或没有公告时保持空仓。
    #[tokio::test]
    async fn listing_follow_template_ignores_stale_and_missing_events() {
        let now = Utc::now();
        for events in [Vec::new(), vec![event(now, 1_800)]] {
            let mut input = input(dec!(100), dec!(500), SandboxIndicators::default());
            input.events = events;

            let report = run(LISTING_FOLLOW_WAT, input).await;
            assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
            assert!(
                report
                    .decisions
                    .iter()
                    .all(|decision| matches!(decision, SandboxDecision::Hold { .. })),
                "不应产生开仓决策: {:?}",
                report.decisions
            );
        }
    }

    /// 持仓超过最长持有时长后平仓。
    #[tokio::test]
    async fn listing_follow_template_closes_after_max_hold() {
        let now = Utc::now();
        let mut input = input(dec!(100), dec!(500), SandboxIndicators::default());
        input.position = Some(arb_core::SandboxPositionSnapshot {
            symbol: "BTCUSDT".into(),
            venue: "binance".into(),
            side: PositionSide::Long,
            notional_usd: dec!(500),
            entry_price: dec!(100),
            opened_at: now - chrono::Duration::hours(7),
            held_secs: 25_200,
            held_bars: 84,
            entry_net_edge_bps: Decimal::ZERO,
        });

        let report = run(LISTING_FOLLOW_WAT, input).await;
        assert_eq!(report.status, SandboxRunStatus::Ok, "{:?}", report.error);
        assert!(
            report
                .decisions
                .iter()
                .any(|decision| matches!(decision, SandboxDecision::Close { .. })),
            "应产生平仓决策: {:?}",
            report.decisions
        );
    }
}
