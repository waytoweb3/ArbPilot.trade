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
# 与 docker-compose.yml 的默认值保持一致，回滚时重新打回同一个标签。
EXECUTOR_IMAGE="${ARBPILOT_EXECUTOR_IMAGE:-ghcr.io/waytoweb3/arbpilot-executor:latest}"
export ARBPILOT_EXECUTOR_IMAGE="$EXECUTOR_IMAGE"
BACKUP_DIR="${ARBPILOT_BACKUP_DIR:-$ROOT_DIR/deploy/backups}"
# 升级前保留最近几份数据库备份；失败时自动回滚到上一镜像。
KEEP_BACKUPS=5
SKIP_BACKUP=0
RESTORE_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-backup) SKIP_BACKUP=1 ;;
    --restore)
      shift
      [ $# -gt 0 ] || { printf '用法：bash deploy/install.sh --restore <备份文件>\n' >&2; exit 2; }
      RESTORE_FILE="$1"
      ;;
    -h | --help)
      cat <<'USAGE'
用法：bash deploy/install.sh [--no-backup] [--restore <备份文件>]

  --no-backup        跳过升级前的数据库备份（不建议）
  --restore <文件>   先启动 PostgreSQL 并从 .sql.gz 备份恢复，再升级执行器
USAGE
      exit 0
      ;;
    *) printf '未知参数：%s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

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

# 启用内置 TLS 后健康检查必须走 https，且允许自签证书。
health_ok() {
  local url
  if [ -n "${ARBPILOT_TLS_CERT_FILE:-}" ]; then
    url="https://127.0.0.1:${EXECUTOR_PORT}/api/v1/health"
    if have curl; then
      curl -fsSk --max-time 3 "$url" >/dev/null 2>&1
    else
      wget -q -T 3 --no-check-certificate -O /dev/null "$url"
    fi
  else
    http_get "http://127.0.0.1:${EXECUTOR_PORT}/api/v1/health"
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
postgres_running() {
  local id
  id="$(compose ps -q postgres 2>/dev/null || true)"
  [ -n "$id" ] &&
    [ "$(docker_cmd inspect -f '{{.State.Running}}' "$id" 2>/dev/null || true)" = "true" ]
}

wait_healthy() {
  local attempts="${1:-60}"
  local i
  for i in $(seq 1 "$attempts"); do
    if health_ok; then
      return 0
    fi
    sleep 3
  done
  return 1
}

# 升级前的数据库备份：保留最近 KEEP_BACKUPS 份，失败时直接终止，避免带着无备份的数据升级。
backup_database() {
  [ "$SKIP_BACKUP" = "1" ] && {
    warn "已按 --no-backup 跳过数据库备份"
    return 0
  }
  postgres_running || {
    log "未检测到运行中的 PostgreSQL（首次安装），跳过备份"
    return 0
  }
  mkdir -p "$BACKUP_DIR"
  local file
  file="$BACKUP_DIR/arbpilot-$(date +%Y%m%d-%H%M%S).sql.gz"
  log "升级前备份数据库：$file"
  if compose exec -T postgres pg_dump -U arbpilot -d arbpilot | gzip >"$file"; then
    # 只保留最近 KEEP_BACKUPS 份，避免长期运行把磁盘写满。
    ls -1t "$BACKUP_DIR"/arbpilot-*.sql.gz 2>/dev/null |
      tail -n +"$((KEEP_BACKUPS + 1))" | xargs -r rm -f
    log "备份完成（保留最近 $KEEP_BACKUPS 份）"
  else
    rm -f "$file"
    fail "数据库备份失败；确认 PostgreSQL 正常后重试，或显式加 --no-backup"
  fi
}

restore_database() {
  [ -n "$RESTORE_FILE" ] || return 0
  [ -f "$RESTORE_FILE" ] || fail "找不到备份文件：$RESTORE_FILE"
  log "启动 PostgreSQL 并恢复 $RESTORE_FILE"
  compose up -d postgres
  local i
  for i in $(seq 1 30); do
    compose exec -T postgres pg_isready -U arbpilot -d arbpilot >/dev/null 2>&1 && break
    sleep 2
  done
  gunzip -c "$RESTORE_FILE" |
    compose exec -T postgres psql -v ON_ERROR_STOP=1 -U arbpilot -d arbpilot >/dev/null ||
    fail "恢复失败；数据库可能已被部分写入，请检查备份文件与容器日志"
  log "恢复完成"
}

# 匿名拉取失败时用凭据重试一次：GHCR 包若误设为私有，部署会卡在 pull。
pull_images() {
  if compose pull; then
    return 0
  fi
  if [ -n "${ARBPILOT_GHCR_TOKEN:-}" ]; then
    warn "匿名拉取失败，使用 ARBPILOT_GHCR_USER/ARBPILOT_GHCR_TOKEN 登录后重试"
    printf '%s' "$ARBPILOT_GHCR_TOKEN" |
      docker_cmd login ghcr.io -u "${ARBPILOT_GHCR_USER:-arbpilot}" --password-stdin &&
      compose pull && return 0
  fi
  return 1
}

# 记录当前执行器镜像 ID，健康检查失败时用它回滚。
PREVIOUS_IMAGE_ID=""
if [ -n "$(compose ps -q executor 2>/dev/null || true)" ]; then
  PREVIOUS_IMAGE_ID="$(docker_cmd inspect -f '{{.Image}}' "$(compose ps -q executor)" 2>/dev/null || true)"
fi

backup_database
restore_database

# 容器内的健康检查也要跟随 TLS 开关，否则启用证书后容器会一直被标记为不健康。
if [ -n "${ARBPILOT_TLS_CERT_FILE:-}" ]; then
  export ARBPILOT_HEALTHCHECK_URL="https://127.0.0.1:8787/api/v1/health"
else
  export ARBPILOT_HEALTHCHECK_URL="http://127.0.0.1:8787/api/v1/health"
fi

log "拉取执行器镜像（由 GitHub Actions 构建并推送到 GHCR）"
pull_images ||
  fail "镜像拉取失败：若镜像仍是私有包，请在 deploy/.env 配置 ARBPILOT_GHCR_USER/ARBPILOT_GHCR_TOKEN，或执行 docker login ghcr.io 后重试"
log "启动容器"
compose up -d

log "等待执行器就绪"
if ! wait_healthy 60; then
  compose logs --tail 40 executor || true
  if [ -n "$PREVIOUS_IMAGE_ID" ]; then
    warn "新版本未在预期时间内就绪，回滚到升级前的镜像"
    docker_cmd tag "$PREVIOUS_IMAGE_ID" "$EXECUTOR_IMAGE" || true
    compose up -d --force-recreate executor || true
    if wait_healthy 40; then
      fail "升级失败，已回滚到上一版本并恢复服务；请检查上方日志"
    fi
    fail "升级失败且回滚后仍未就绪，请人工检查 compose logs executor"
  fi
  fail "执行器未在预期时间内就绪，请查看上面的日志"
fi

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
