#!/usr/bin/env bash
# ArbPilot 执行器远程安装 / 升级脚本（发布为 https://download.arbpilot.trade/executor/install.sh）。
#
#   安装：curl -fsSL https://download.arbpilot.trade/executor/install.sh | sudo bash
#   更新：sudo arbpilot update
#
# 每次执行都会读取发布清单、下载对应版本、校验 sha256、解包到安装目录，再调用
# deploy/install.sh 启动执行器与 PostgreSQL。deploy/.env 与 Docker 数据卷会保留。
set -euo pipefail

RELEASE_BASE_URL="${ARBPILOT_RELEASE_BASE_URL:-https://download.arbpilot.trade/executor}"
INSTALL_DIR="${ARBPILOT_INSTALL_DIR:-/opt/arbpilot}"

log() { printf '\033[1;36m[arbpilot]\033[0m %s\n' "$*"; }
fail() {
  printf '\033[1;31m[arbpilot]\033[0m %s\n' "$*" >&2
  exit 1
}

case "$INSTALL_DIR" in
  /?*) ;;
  *) fail "安装目录必须是绝对路径：$INSTALL_DIR" ;;
esac
case "$INSTALL_DIR" in
  *..*) fail "安装目录不能包含 ..：$INSTALL_DIR" ;;
esac
INSTALL_DIR="${INSTALL_DIR%/}"

[ "$(id -u)" -eq 0 ] ||
  fail "需要 root 权限，请用：curl -fsSL $RELEASE_BASE_URL/install.sh | sudo bash"

download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --connect-timeout 20 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    fail "缺少 curl 或 wget，请先安装：apt-get update && apt-get install -y curl"
  fi
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

# ---------------------------------------------------------------------------
# 1. 读取发布清单
# ---------------------------------------------------------------------------
log "读取发布清单 $RELEASE_BASE_URL/manifest.json"
download "$RELEASE_BASE_URL/manifest.json" "$TMP_DIR/manifest.json" ||
  fail "无法下载发布清单，请检查服务器网络或发布源地址"

json_str() {
  sed -n 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TMP_DIR/manifest.json" | head -n 1
}

VERSION="$(json_str version)"
BUNDLE_URL="$(json_str bundle_url)"
SHA256="$(json_str sha256)"

[ -n "$VERSION" ] || fail "发布清单缺少 version 字段"
case "$BUNDLE_URL" in
  https://*) ;;
  *) fail "部署包地址必须是 https 链接" ;;
esac
[ "$(printf '%s' "$SHA256" | wc -c | tr -d ' ')" -eq 64 ] ||
  fail "发布清单里的 sha256 格式不正确"

if [ -f "$INSTALL_DIR/.arbpilot-version" ]; then
  log "当前版本 $(cat "$INSTALL_DIR/.arbpilot-version")，目标版本 $VERSION"
else
  log "首次安装，目标版本 $VERSION"
fi

# ---------------------------------------------------------------------------
# 2. 下载并校验
# ---------------------------------------------------------------------------
log "下载部署包 $BUNDLE_URL"
download "$BUNDLE_URL" "$TMP_DIR/bundle.tgz" || fail "部署包下载失败"

if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL="$(sha256sum "$TMP_DIR/bundle.tgz" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL="$(shasum -a 256 "$TMP_DIR/bundle.tgz" | awk '{print $1}')"
else
  fail "缺少 sha256sum 或 shasum，无法校验部署包"
fi
[ "$ACTUAL" = "$SHA256" ] ||
  fail "部署包校验失败（期望 $SHA256，实际 $ACTUAL），请重试或联系发布者"

# ---------------------------------------------------------------------------
# 3. 解包（保留 deploy/.env）
# ---------------------------------------------------------------------------
log "解包到 $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
if [ -f "$INSTALL_DIR/deploy/.env" ]; then
  cp -p "$INSTALL_DIR/deploy/.env" "$TMP_DIR/env.bak"
fi
# 数据都在 Docker 卷里，这里只替换源码与部署脚本。
find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
tar xzf "$TMP_DIR/bundle.tgz" -C "$INSTALL_DIR"
if [ -f "$TMP_DIR/env.bak" ]; then
  install -m 600 "$TMP_DIR/env.bak" "$INSTALL_DIR/deploy/.env"
fi
printf '%s\n' "$VERSION" >"$INSTALL_DIR/.arbpilot-version"

if [ -f "$INSTALL_DIR/deploy/arbpilot" ]; then
  install -m 0755 "$INSTALL_DIR/deploy/arbpilot" /usr/local/bin/arbpilot
  log "已安装管理命令：arbpilot（update / status / logs / token / restart / version）"
fi

# ---------------------------------------------------------------------------
# 4. 启动执行器与 PostgreSQL
# ---------------------------------------------------------------------------
log "安装并启动执行器与 PostgreSQL"
bash "$INSTALL_DIR/deploy/install.sh"

cat <<EOF

后续维护（不需要记路径）：

  sudo arbpilot update     升级到最新发布版本
  sudo arbpilot status     查看容器状态与健康检查
  sudo arbpilot token      打印执行器访问令牌
  sudo arbpilot logs -f    跟随执行器日志
EOF
