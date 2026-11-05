# ArbPilot.trade

自托管量化交易终端：桌面端（Tauri 2 + React）负责配置与监控，Rust 执行器负责行情、风控与订单执行。用户资金留在交易所/钱包，密钥只保存在执行器所在设备。

> 产品方向、定价假设与待验证事项见 [ArbPilot-项目说明.md](./ArbPilot-项目说明.md)。

## 目录结构

```
.
├── apps/
│   ├── desktop/              # Tauri 2 + React + TypeScript + Tailwind v4
│   │   ├── src/              # 页面、组件、API 客户端、状态
│   │   └── src-tauri/        # 桌面壳（不接触交易所密钥）
│   ├── site/                 # 产品介绍站（Vite + React + Tailwind v4）
│   └── executor/             # Rust 执行器（Axum REST/WebSocket）
│       ├── src/
│       │   ├── routes/       # health / opportunities / funding / orders / positions / strategies / control / ws
│       │   ├── exchange/     # 交易所公开行情、账户查询与实盘下单适配器
│       │   ├── funding.rs    # 资金费率采集（历史回填 + 增量轮询）
│       │   ├── market.rs     # 盘口行情与机会扫描
│       │   ├── store.rs      # PostgreSQL（未配置时降级内存模式）
│       │   └── state.rs      # 共享状态与暂停开关
│       └── migrations/       # SQLx 迁移
├── crates/arb-core/          # 领域模型、风控引擎、双腿状态机、交易所 trait
├── deploy/                   # Docker Compose + 执行器镜像
└── Cargo.toml                # Rust workspace
```

## 本地启动

```bash
# 1. 安装前端依赖
pnpm install

# 2. 启动执行器（默认 127.0.0.1:8787，内存模式）
cp .env.example .env
cargo run -p arbpilot-executor

# 3. 另开终端启动桌面端开发模式
pnpm dev:desktop
```

只调试界面可运行 `pnpm dev`（浏览器打开 http://localhost:1420）。

产品介绍站在 `apps/site`，运行 `pnpm dev:site`（http://localhost:1421），页面数据为本地演示。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `ARBPILOT_BIND_ADDR` | `127.0.0.1:8787` | 执行器监听地址 |
| `ARBPILOT_API_TOKEN` | `dev-local-token` | 执行器访问令牌，正式部署必须修改 |
| `ARBPILOT_ALLOW_LIVE_TRADING` | `false` | 真实交易开关 |
| `ARBPILOT_RISK_FILE` | `data/risk_limits.json` | 设置页修改的风控限额，存在时优先于环境变量 |
| `DATABASE_URL` | 空 | 未配置时执行器以内存模式运行 |
| `ARBPILOT_FUNDING_ENABLED` | `true` | 资金费率采集开关，设为 `false` 关闭 |
| `ARBPILOT_FUNDING_SYMBOLS` | `BTCUSDT,ETHUSDT` | 采集的 USDT 本位永续合约，逗号分隔 |
| `ARBPILOT_FUNDING_POLL_SECS` | `60` | 增量采集间隔（秒），最小 15 |
| `ARBPILOT_BINANCE_FAPI_URL` | `https://fapi.binance.com` | Binance 合约公开行情地址，可指向镜像或本地桩 |
| `VITE_EXECUTOR_URL` | `http://127.0.0.1:8787` | 桌面端连接的执行器地址 |

## 常用命令

```bash
cargo test --workspace            # 风控与状态机单元测试
cargo check --workspace           # Rust 编译检查
pnpm build                        # 桌面端类型检查 + 构建
pnpm db:up                        # 用 Docker Compose 启动 PostgreSQL
```

服务器部署：编辑 `deploy/.env`（或导出环境变量）后执行

```bash
docker compose -f deploy/docker-compose.yml up -d
```

执行器与数据库端口默认只绑定 `127.0.0.1`，远程访问请使用 SSH 隧道/VPN，不要直接暴露管理端口。

## 当前实现边界

- 行情与账户余额取自真实交易所公开/私有接口；下单需显式开启 `ARBPILOT_ALLOW_LIVE_TRADING`，CEX 侧已覆盖 Binance / Aster / OKX / Bybit / Bitget / Gate / HTX / MEXC / BingX 的 USDT 本位永续；Hyperliquid、Lighter 目前仅支持钱包地址只读查询，下单需钱包私钥签名。
- Binance 资金费率走公开接口（无需 API Key）：首次启动回填历史，但只保留最近 1 个月的结算记录（超期数据定期清理），之后按结算时间增量入库；未配置 `DATABASE_URL` 时只保留内存数据，重启即丢。
- 风控、幂等、双腿补偿状态机、订阅不影响仓位管理等约束已在核心层实现。
- 策略沙箱（WASM）、Telegram、自动更新、订阅支付尚未接入，按路线图留在后续阶段。

## 安全约定

- 桌面端不保存交易所 API Key；密钥只应存放在执行器所在设备的系统密钥库。
- 所有开仓指令统一经过 `RiskEngine::preflight`，Telegram 与策略插件不得绕过。
- 执行器 API 使用 Bearer 令牌；生产环境需替换默认令牌、收紧 CORS，并置于安全隧道之后。
