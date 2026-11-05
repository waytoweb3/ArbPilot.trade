#!/usr/bin/env bash
# ArbPilot 执行器一键部署：安装 Docker、生成访问令牌、构建并启动执行器与 PostgreSQL。
# 可重复执行：已存在的 deploy/.env 与数据卷不会被覆盖。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$ROOT_DIR/deploy/.env"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"
EXECUTOR_PORT="${ARBPILOT_EXECUTOR_PORT:-8787}"
# compose 的端口映射也读这个变量，需导出到子进程。
export ARBPILOT_EXECUTOR_PORT="$EXECUTOR_PORT"

log() { printf '\033[1;36m[arbpilot]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[arbpilot]\033[0m %s\n' "$*"; }
fail() {
  printf '\033[1;31m[arbpilot]\033[0m %s\n' "$*" >&2
  exit 1
}

random_hex() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "$1"
  else
    head -c "$1" /dev/urandom | od -An -tx1 | tr -d ' \n'
  fi
}

http_get() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsS --max-time 3 "$1" >/dev/null 2>&1
  else
    wget -q -T 3 -O /dev/null "$1"
  fi
}

[ -f "$COMPOSE_FILE" ] || fail "找不到 $COMPOSE_FILE，请在仓库根目录执行 bash deploy/install.sh"
[ "$(uname -s)" = "Linux" ] || warn "当前不是 Linux，脚本只适用于服务器环境"

if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 || fail "需要 root 权限，请用 sudo bash deploy/install.sh 运行"
  SUDO="sudo"
else
  SUDO=""
fi

# ---------------------------------------------------------------------------
# 1. Docker
# ---------------------------------------------------------------------------
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
  log "安装 curl（用于下载 Docker 安装脚本与健康检查）"
  $SUDO apt-get update -qq && $SUDO apt-get install -y curl
fi

if ! command -v docker >/dev/null 2>&1; then
  log "未检测到 Docker，使用官方脚本安装"
  if command -v curl >/dev/null 2>&1; then
    $SUDO sh -c 'curl -fsSL https://get.docker.com | sh' || fail "Docker 安装失败，请检查服务器网络后重试"
  else
    $SUDO sh -c 'wget -qO- https://get.docker.com | sh' || fail "Docker 安装失败，请检查服务器网络后重试"
  fi
  $SUDO systemctl enable --now docker >/dev/null 2>&1 || true
fi

compose() {
  $SUDO docker compose -f "$COMPOSE_FILE" --env-file "$ENV_FILE" "$@"
}

$SUDO docker compose version >/dev/null 2>&1 ||
  fail "缺少 docker compose 插件，请先安装 docker-compose-plugin"

# ---------------------------------------------------------------------------
# 2. 访问令牌与数据库密码
# ---------------------------------------------------------------------------
if [ ! -f "$ENV_FILE" ]; then
  log "生成访问令牌与数据库密码：deploy/.env"
  cat >"$ENV_FILE" <<EOF
# 由 deploy/install.sh 生成，包含访问令牌与数据库密码，请勿提交到版本库。
ARBPILOT_API_TOKEN=$(random_hex 24)
POSTGRES_PASSWORD=$(random_hex 16)
EOF
  chmod 600 "$ENV_FILE"
else
  log "沿用已有的 deploy/.env（不会覆盖令牌）"
  chmod 600 "$ENV_FILE" || true
fi

# shellcheck disable=SC1090
set -a && . "$ENV_FILE" && set +a
[ -n "${ARBPILOT_API_TOKEN:-}" ] || fail "deploy/.env 缺少 ARBPILOT_API_TOKEN"

# ---------------------------------------------------------------------------
# 3. 构建并启动
# ---------------------------------------------------------------------------
log "构建并启动容器（首次需要编译 Rust，耗时较长）"
compose up -d --build

log "等待执行器就绪"
ready=""
for _ in $(seq 1 60); do
  if http_get "http://127.0.0.1:${EXECUTOR_PORT}/api/v1/health"; then
    ready="yes"
    break
  fi
  sleep 3
done
[ -n "$ready" ] || {
  compose logs --tail 40 executor || true
  fail "执行器未在预期时间内就绪，请查看上面的日志"
}

SERVER_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
SERVER_IP="${SERVER_IP:-<服务器IP>}"

cat <<EOF

执行器已启动。

  监听地址（仅本机）：http://127.0.0.1:${EXECUTOR_PORT}
  访问令牌：${ARBPILOT_API_TOKEN}
  配置文件：${ENV_FILE}

桌面端连接方式（推荐，端口不对公网暴露）：

  1) 在你自己的电脑上保持这条隧道：
     ssh -N -L ${EXECUTOR_PORT}:127.0.0.1:${EXECUTOR_PORT} root@${SERVER_IP}
  2) 桌面端「设置 → 连接」填写：
     执行器地址：http://127.0.0.1:${EXECUTOR_PORT}
     访问令牌：上面那串令牌
  3) 保存后左上角连接状态应变为可用。

访问令牌等同于执行器的完整控制权，请勿发到群里或提交到 Git。
EOF
