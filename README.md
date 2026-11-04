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
│   └── executor/             # Rust 执行器（Axum REST/WebSocket）
│       ├── src/
│       │   ├── routes/       # health / opportunities / orders / positions / strategies / control / ws
│       │   ├── exchange/     # 交易所适配器（当前仅模拟盘）
│       │   ├── market.rs     # 模拟行情与机会扫描
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

# 2. 启动执行器（默认 127.0.0.1:8787，模拟盘、内存模式）
cp .env.example .env
cargo run -p arbpilot-executor

# 3. 另开终端启动桌面端开发模式
pnpm dev:desktop
```

只调试界面可运行 `pnpm dev`（浏览器打开 http://localhost:1420）。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `ARBPILOT_BIND_ADDR` | `127.0.0.1:8787` | 执行器监听地址 |
| `ARBPILOT_API_TOKEN` | `dev-local-token` | 执行器访问令牌，正式部署必须修改 |
| `ARBPILOT_MODE` | `paper` | `paper` / `live`；真实交易需显式开启 |
| `ARBPILOT_ALLOW_LIVE_TRADING` | `false` | 真实交易开关 |
| `DATABASE_URL` | 空 | 未配置时执行器以内存模式运行 |
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

- 行情、订单成交与账户余额均为模拟数据，`live` 模式尚未接入真实交易所适配器。
- 风控、幂等、双腿补偿状态机、订阅不影响仓位管理等约束已在核心层实现。
- 策略沙箱（WASM）、Telegram、自动更新、订阅支付尚未接入，按路线图留在后续阶段。

## 安全约定

- 桌面端不保存交易所 API Key；密钥只应存放在执行器所在设备的系统密钥库。
- 所有开仓指令统一经过 `RiskEngine::preflight`，Telegram 与策略插件不得绕过。
- 执行器 API 使用 Bearer 令牌；生产环境需替换默认令牌、收紧 CORS，并置于安全隧道之后。
