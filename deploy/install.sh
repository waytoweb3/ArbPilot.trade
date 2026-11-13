#!/usr/bin/env bash
# ArbPilot 执行器一键部署：在服务器上安装 Docker（若缺失）、生成访问令牌、构建并启动执行器与 PostgreSQL。
# 可重复执行：已存在的 deploy/.env 与数据卷不会被覆盖。
#
# 支持的发行版：Debian/Ubuntu（apt）、RHEL/CentOS/Rocky/Alma（dnf/yum）、Fedora、Alpine（apk）、openSUSE（zypper）。
# 服务器上没有 Docker 时优先用官方脚本安装；官方脚本失败或缺少 compose 插件时回退到发行版仓库
# 与 GitHub 发布的 compose 插件二进制，尽量做到一条命令跑通。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$ROOT_DIR/deploy/.env"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"
EXECUTOR_PORT="${ARBPILOT_EXECUTOR_PORT:-8787}"
# compose 的端口映射也读这些变量，需导出到子进程。
export ARBPILOT_EXECUTOR_PORT="$EXECUTOR_PORT"
# 默认只绑定回环地址；设为 0.0.0.0 时桌面端可直接用服务器 IP 连接（需自行放行防火墙）。
export ARBPILOT_EXECUTOR_HOST="${ARBPILOT_EXECUTOR_HOST:-127.0.0.1}"

log() { printf '\033[1;36m[arbpilot]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[arbpilot]\033[0m %s\n' "$*"; }
fail() {
  printf '\033[1;31m[arbpilot]\033[0m %s\n' "$*" >&2
  exit 1
}

have() { command -v "$1" >/dev/null 2>&1; }

random_hex() {
  if have openssl; then
    openssl rand -hex "$1"
  else
    head -c "$1" /dev/urandom | od -An -tx1 | tr -d ' \n'
  fi
}

http_get() {
  if have curl; then
    curl -fsS --max-time 3 "$1" >/dev/null 2>&1
  else
    wget -q -T 3 -O /dev/null "$1"
  fi
}

# 统一走 curl/wget，后续 Docker 与 compose 的下载都复用它。
download() {
  if have curl; then
    curl -fsSL --retry 3 --connect-timeout 20 -o "$2" "$1"
  else
    wget -q -O "$2" "$1"
  fi
}

[ -f "$COMPOSE_FILE" ] || fail "找不到 $COMPOSE_FILE，请在仓库根目录执行 bash deploy/install.sh"
[ "$(uname -s)" = "Linux" ] || warn "当前不是 Linux，脚本只适用于服务器环境"

if [ "$(id -u)" -ne 0 ]; then
  have sudo || fail "需要 root 权限，请用 sudo bash deploy/install.sh 运行"
  SUDO="sudo"
else
  SUDO=""
fi

as_root() { # 以 root 执行命令：root 直接跑，普通用户走 sudo。
  if [ -n "$SUDO" ]; then
    $SUDO "$@"
  else
    "$@"
  fi
}

docker_cmd() { as_root docker "$@"; }

# 按发行版安装系统包；不支持时返回非零，由调用方决定是否致命。
install_packages() {
  if have apt-get; then
    as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq &&
      as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@"
  elif have dnf; then
    as_root dnf install -y "$@"
  elif have yum; then
    as_root yum install -y "$@"
  elif have apk; then
    as_root apk add --no-cache "$@"
  elif have zypper; then
    as_root zypper -n install "$@"
  else
    return 1
  fi
}

# ---------------------------------------------------------------------------
# 1. 基础工具：curl/wget、tar、openssl
# ---------------------------------------------------------------------------
if ! have curl && ! have wget; then
  log "安装 curl（用于下载 Docker 安装脚本与健康检查）"
  install_packages curl ca-certificates ||
    fail "无法自动安装 curl，请先手动安装后重试：apt-get install -y curl"
fi
have tar || install_packages tar || fail "缺少 tar，无法解包部署文件"

# ---------------------------------------------------------------------------
# 2. Docker
# ---------------------------------------------------------------------------
if ! have docker; then
  log "未检测到 Docker，使用官方脚本安装"
  if have curl; then
    $SUDO sh -c 'curl -fsSL https://get.docker.com | sh'
  else
    $SUDO sh -c 'wget -qO- https://get.docker.com | sh'
  fi || {
    warn "官方脚本安装失败，回退到发行版仓库安装 docker"
    install_packages docker.io || install_packages docker || install_packages docker-ce ||
      fail "Docker 安装失败，请检查服务器网络或手动安装后重试"
  }
fi

have docker || fail "Docker 安装后仍未找到 docker 命令"

# 官方脚本通常会自动启用服务；容器/精简系统里可能没有 systemctl，失败不致命。
if have systemctl; then
  $SUDO systemctl enable --now docker >/dev/null 2>&1 || true
else
  $SUDO service docker start >/dev/null 2>&1 || true
fi

if ! docker_cmd info >/dev/null 2>&1; then
  # 刚装完 daemon 可能还没就绪，给一小段等待时间。
  for _ in $(seq 1 10); do
    docker_cmd info >/dev/null 2>&1 && break
    sleep 1
  done
fi
docker_cmd info >/dev/null 2>&1 ||
  fail "Docker 已安装但守护进程不可用，请检查 systemctl status docker"

# ---------------------------------------------------------------------------
# 3. Docker Compose v2 插件
# ---------------------------------------------------------------------------
compose_ready() { docker_cmd compose version >/dev/null 2>&1; }

if ! compose_ready; then
  log "缺少 docker compose 插件，尝试安装"
  install_packages docker-compose-plugin ||
    install_packages docker-compose-v2 ||
    true
fi

if ! compose_ready; then
  # 发行版仓库没有 compose 插件时，直接从 GitHub 下载 CLI 插件二进制（系统级路径，sudo 可见）。
  case "$(uname -m)" in
    x86_64 | amd64) COMPOSE_ARCH="x86_64" ;;
    aarch64 | arm64) COMPOSE_ARCH="aarch64" ;;
    armv7l) COMPOSE_ARCH="armv7" ;;
    *) fail "不支持的 CPU 架构：$(uname -m)，请手动安装 docker compose 插件" ;;
  esac
  log "从 GitHub 下载 docker compose 插件（linux-${COMPOSE_ARCH}）"
  TMP_COMPOSE="$(mktemp)"
  trap 'rm -f "$TMP_COMPOSE"' EXIT
  download "https://github.com/docker/compose/releases/latest/download/docker-compose-linux-${COMPOSE_ARCH}" \
    "$TMP_COMPOSE" || fail "下载 docker compose 插件失败，请检查服务器网络"
  chmod +x "$TMP_COMPOSE"
  as_root mkdir -p /usr/local/lib/docker/cli-plugins
  as_root install -m 0755 "$TMP_COMPOSE" /usr/local/lib/docker/cli-plugins/docker-compose
  rm -f "$TMP_COMPOSE"
  trap - EXIT
fi

compose_ready ||
  fail "缺少 docker compose 插件，请先安装 docker-compose-plugin 后重试"

compose() {
  docker_cmd compose -f "$COMPOSE_FILE" --env-file "$ENV_FILE" "$@"
}

# ---------------------------------------------------------------------------
# 4. 访问令牌与数据库密码
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
# 5. 拉取镜像并启动
# ---------------------------------------------------------------------------
log "拉取执行器镜像（由 GitHub Actions 构建并推送到 GHCR）"
compose pull ||
  fail "镜像拉取失败：若镜像仍是私有包，请先在服务器执行 docker login ghcr.io 后重试"
log "启动容器"
compose up -d

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
