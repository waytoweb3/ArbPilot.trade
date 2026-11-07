# 执行器镜像：Rust 编译后只保留二进制，运行阶段不包含工具链。
# 构建上下文为仓库根目录（compose 里 context: ../..）。
FROM rust:1-slim-bookworm AS build
WORKDIR /src

# 依赖清单先复制，源码变化时可复用这一层的依赖缓存。
COPY Cargo.toml Cargo.lock ./
COPY crates/arb-core ./crates/arb-core
COPY apps/executor ./apps/executor
COPY apps/desktop/src-tauri ./apps/desktop/src-tauri

RUN cargo build --release -p arbpilot-executor

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/arbpilot-executor /usr/local/bin/arbpilot-executor

# 账户凭证、风控配置与绑定关系都落在这里，必须挂载持久化卷。
WORKDIR /app
ENV ARBPILOT_BIND_ADDR=0.0.0.0:8787 \
    ARBPILOT_ACCOUNTS_FILE=/data/accounts.json \
    ARBPILOT_RISK_FILE=/data/risk.json \
    ARBPILOT_TELEGRAM_BINDINGS_FILE=/data/telegram.json \
    ARBPILOT_LICENSE_FILE=/data/license.token

EXPOSE 8787
CMD ["arbpilot-executor"]
