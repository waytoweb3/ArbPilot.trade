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
| `ARBPILOT_RISK_FILE` | `data/risk_limits.json` | 设置页修改的风控限额，存在时优先于环境变量 |
| `DATABASE_URL` | 空 | 未配置时执行器以内存模式运行 |
| `ARBPILOT_FUNDING_ENABLED` | `true` | 资金费率采集开关，设为 `false` 关闭 |
| `ARBPILOT_FUNDING_SYMBOLS` | `BTCUSDT,ETHUSDT` | 采集的 USDT 本位永续合约，逗号分隔 |
| `ARBPILOT_FUNDING_POLL_SECS` | `60` | 增量采集间隔（秒），最小 15 |
| `ARBPILOT_BINANCE_FAPI_URL` | `https://fapi.binance.com` | Binance 合约公开行情地址，可指向镜像或本地桩 |
| `VITE_EXECUTOR_URL` | `http://127.0.0.1:8787` | 桌面端连接的执行器地址 |
| `VITE_EXECUTOR_TOKEN` | `dev-local-token` | 桌面端默认访问令牌，官方托管版本构建时注入 |

## 常用命令

```bash
cargo test --workspace            # 风控与状态机单元测试
cargo check --workspace           # Rust 编译检查
pnpm build                        # 桌面端类型检查 + 构建
pnpm db:up                        # 用 Docker Compose 启动 PostgreSQL
```

服务器部署：不需要在服务器上敲命令。客户端左侧「文档」页填服务器 IP、账号与密码（或私钥），点「一键部署」即可自动下载发布包、装 Docker、启动执行器与 PostgreSQL，并回填访问令牌；有新版本时同一页点「更新」。也可以登录服务器执行一条命令部署（同样会一并启动 PostgreSQL）：

```bash
curl -fsSL https://download.arbpilot.trade/executor/install.sh | sudo bash
sudo arbpilot update    # 后续升级，保留令牌与数据
```

完整的购买选型、连接与运维步骤见 [自托管部署指南](./docs/自托管部署指南.md)。

发布新版本：`bash deploy/package.sh`，把 `dist/release/` 下的 tar.gz、`manifest.json` 与 `install.sh` 上传到 `https://download.arbpilot.trade/executor/`，客户端即可检查到更新。

客户端自身的更新走独立发布源：`bash deploy/package-desktop.sh` 生成带签名的安装包与 `latest.json`，上传到 `https://download.arbpilot.trade/desktop/` 后，用户在「设置 → 客户端更新 → 检查更新」即可一键升级并重启。

也可以手动执行：

```bash
docker compose -f deploy/docker-compose.yml up -d
```

执行器与数据库端口默认只绑定 `127.0.0.1`，远程访问请使用 SSH 隧道/VPN，不要直接暴露管理端口。

## 当前实现边界

- 行情与账户余额取自真实交易所公开/私有接口；订单经统一风控后直接提交到真实交易所，已覆盖 Binance / Aster / OKX / Bybit / Bitget / Gate / HTX / MEXC / BingX 的 USDT 本位永续；Hyperliquid 与 Lighter 用钱包地址查询余额，填入签名私钥（Hyperliquid API Wallet、Lighter API Key）后同样在本机签名下单。
- 资金费率走各交易所公开接口（无需 API Key）：首次启动回填历史，但只保留最近 1 个月的结算记录（超期数据定期清理），之后按结算时间增量入库；未配置 `DATABASE_URL` 时只保留内存数据，重启即丢。
- 风控、幂等、双腿补偿状态机、订阅不影响仓位管理等约束已在核心层实现；自定义策略在 Wasmtime 沙箱内运行，只输出订单意图，统一经风控网关下单。
- 策略沙箱、内置模板、AI 生成策略代码、WASM 回测、Telegram、订阅支付均已接入；执行器与桌面客户端都支持「检查更新 + 一键升级」。
- 机会扫描目前只覆盖资金费率，CEX-CEX / CEX-DEX 价差扫描尚未实现；实盘腿状态以交易所下单回执为准，尚未轮询成交与启动对账。完整缺口与后续规划见 [项目进度与规划](./docs/项目进度与规划.md)。

## 安全约定

- 桌面端不保存交易所 API Key；密钥只应存放在执行器所在设备的系统密钥库。
- 所有开仓指令统一经过 `RiskEngine::preflight`，Telegram 与策略插件不得绕过。
- 执行器 API 使用 Bearer 令牌；生产环境需替换默认令牌、收紧 CORS，并置于安全隧道之后。
