#!/usr/bin/env bash
# ArbPilot 官网（apps/site）部署：构建静态文件 → 发布到 Nginx → 可选签发 HTTPS 证书。
#
# 在服务器上的仓库根目录执行：
#   sudo DOMAIN=arbpilot.trade \
#        EMAIL=you@example.com \
#        VITE_PAYMENTS_URL=https://pay.example.com \
#        VITE_ACCOUNTS_URL=https://auth.example.com \
#        bash deploy/deploy-site.sh
#
# 环境变量：
#   DOMAIN            必填，站点域名（A 记录需已指向本机）
#   EMAIL             可选，设置后用 certbot 申请/续期 Let's Encrypt 证书并强制 HTTPS
#   WEB_ROOT          可选，静态文件目录，默认 /var/www/arbpilot-site
#   NGINX_NAME        可选，Nginx 站点名，默认 arbpilot-site
#   VITE_PAYMENTS_URL 可选，支付服务公网地址；缺省时价格用内置兜底值，收银台不可用
#   VITE_ACCOUNTS_URL 可选，账号服务公网地址；缺省时 /device 登录确认页不可用
#   SKIP_BUILD=1      可选，跳过构建，直接发布已有的 apps/site/dist
#   FORCE_NGINX=1     可选，重写 Nginx 配置（会清掉 certbot 写入的 HTTPS，随后自动补回）
#
# 可重复执行：重新构建、覆盖静态文件并 reload Nginx；已签发的证书会自动复用。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SITE_DIR="$ROOT_DIR/apps/site"
DIST_DIR="$SITE_DIR/dist"
TEMPLATE="$ROOT_DIR/deploy/nginx-site.conf"

DOMAIN="${DOMAIN:-}"
EMAIL="${EMAIL:-}"
WEB_ROOT="${WEB_ROOT:-/var/www/arbpilot-site}"
NGINX_NAME="${NGINX_NAME:-arbpilot-site}"
SKIP_BUILD="${SKIP_BUILD:-0}"
FORCE_NGINX="${FORCE_NGINX:-0}"
NODE_MAJOR="${NODE_MAJOR:-22}"

log() { printf '\033[1;36m[arbpilot-site]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[arbpilot-site]\033[0m %s\n' "$*"; }
fail() {
  printf '\033[1;31m[arbpilot-site]\033[0m %s\n' "$*" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# 1. 前置检查
# ---------------------------------------------------------------------------
[ "$(uname -s)" = "Linux" ] || fail "本脚本在服务器（Linux）上执行"
command -v apt-get >/dev/null 2>&1 || fail "目前只支持 Debian / Ubuntu（需要 apt-get）"
[ -n "$DOMAIN" ] || fail "缺少 DOMAIN，例如：sudo DOMAIN=arbpilot.trade bash deploy/deploy-site.sh"
[[ "$DOMAIN" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] ||
  fail "DOMAIN 不是合法域名：$DOMAIN"
case "$WEB_ROOT" in
  /?*) ;;
  *) fail "WEB_ROOT 必须是绝对路径：$WEB_ROOT" ;;
esac
case "$WEB_ROOT" in
  *[\&\|]*) fail "WEB_ROOT 不能包含 & 或 |：$WEB_ROOT" ;;
esac
[ -f "$SITE_DIR/package.json" ] || fail "找不到 $SITE_DIR/package.json，请在仓库根目录执行"
[ -f "$TEMPLATE" ] || fail "找不到 $TEMPLATE"

if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
else
  command -v sudo >/dev/null 2>&1 || fail "需要 root 权限，请用 sudo 运行"
  SUDO="sudo"
fi

# ---------------------------------------------------------------------------
# 2. 系统依赖：Nginx、rsync、Node.js、pnpm
# ---------------------------------------------------------------------------
log "检查系统依赖"
$SUDO apt-get update -qq
for pkg in nginx rsync curl ca-certificates; do
  if ! dpkg -s "$pkg" >/dev/null 2>&1; then
    log "安装 $pkg"
    $SUDO apt-get install -y "$pkg"
  fi
done

node_ready() {
  command -v node >/dev/null 2>&1 || return 1
  local version major minor
  version="$(node -p 'process.versions.node' 2>/dev/null || true)"
  major="${version%%.*}"
  minor="$(printf '%s' "$version" | cut -d. -f2)"
  [ -n "$major" ] || return 1
  [ "$major" -gt 20 ] || { [ "$major" -eq 20 ] && [ "${minor:-0}" -ge 19 ]; }
}

if ! node_ready; then
  log "安装 Node.js ${NODE_MAJOR}.x"
  curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | $SUDO bash -
  $SUDO apt-get install -y nodejs
fi

PNPM_VERSION="$(node -p "require('$ROOT_DIR/package.json').packageManager || ''" 2>/dev/null | sed 's/^pnpm@//')"
PNPM_VERSION="${PNPM_VERSION:-12.16.1}"
if ! command -v pnpm >/dev/null 2>&1; then
  log "启用 pnpm ${PNPM_VERSION}"
  if command -v corepack >/dev/null 2>&1; then
    $SUDO corepack enable >/dev/null 2>&1 || true
    corepack prepare "pnpm@${PNPM_VERSION}" --activate ||
      $SUDO npm install -g "pnpm@${PNPM_VERSION}"
  else
    $SUDO npm install -g "pnpm@${PNPM_VERSION}"
  fi
fi

# ---------------------------------------------------------------------------
# 3. 构建站点
# ---------------------------------------------------------------------------
if [ -n "${VITE_PAYMENTS_URL:-}" ]; then
  export VITE_PAYMENTS_URL
else
  warn "未设置 VITE_PAYMENTS_URL：价格页使用内置兜底价，购买按钮会失败"
fi
if [ -n "${VITE_ACCOUNTS_URL:-}" ]; then
  export VITE_ACCOUNTS_URL
else
  warn "未设置 VITE_ACCOUNTS_URL：桌面端 /device 登录确认页会失败"
fi

if [ "$SKIP_BUILD" = "1" ]; then
  log "SKIP_BUILD=1，跳过构建"
else
  log "安装站点依赖"
  (cd "$ROOT_DIR" && pnpm install --filter @arbpilot/site --frozen-lockfile)
  log "构建站点"
  (cd "$ROOT_DIR" && pnpm --filter @arbpilot/site build)
fi
[ -f "$DIST_DIR/index.html" ] || fail "构建产物不存在：$DIST_DIR/index.html"

# ---------------------------------------------------------------------------
# 4. 发布静态文件
# ---------------------------------------------------------------------------
log "发布到 $WEB_ROOT"
$SUDO install -d -m 0755 "$WEB_ROOT"
$SUDO rsync -a --delete "$DIST_DIR/" "$WEB_ROOT/"
$SUDO chmod -R a+rX "$WEB_ROOT"

# ---------------------------------------------------------------------------
# 5. Nginx 站点
# ---------------------------------------------------------------------------
CONF="/etc/nginx/sites-available/${NGINX_NAME}"
CERT_DIR="/etc/letsencrypt/live/${DOMAIN}"
FRESH_CONF=0

# certbot 会把 HTTPS 配置直接写进这个文件，因此默认只在缺失时生成，避免覆盖。
if [ ! -f "$CONF" ] || [ "$FORCE_NGINX" = "1" ]; then
  log "写入 Nginx 配置 $NGINX_NAME"
  sed -e "s|__DOMAIN__|${DOMAIN}|g" -e "s|__WEB_ROOT__|${WEB_ROOT}|g" "$TEMPLATE" |
    $SUDO tee "$CONF" >/dev/null
  FRESH_CONF=1
else
  log "沿用已有 Nginx 配置（需要重置请加 FORCE_NGINX=1）"
fi
$SUDO ln -sfn "$CONF" "/etc/nginx/sites-enabled/${NGINX_NAME}"
$SUDO nginx -t
if ! $SUDO systemctl reload nginx >/dev/null 2>&1; then
  $SUDO service nginx reload
fi

# ---------------------------------------------------------------------------
# 6. HTTPS（可选）
# ---------------------------------------------------------------------------
if [ -n "$EMAIL" ]; then
  if ! command -v certbot >/dev/null 2>&1; then
    log "安装 certbot"
    $SUDO apt-get install -y certbot python3-certbot-nginx
  fi
  if [ -d "$CERT_DIR" ]; then
    if [ "$FRESH_CONF" = "1" ]; then
      log "复用已有证书并写回 HTTPS 配置"
      $SUDO certbot --nginx -d "$DOMAIN" --reinstall \
        --non-interactive --agree-tos -m "$EMAIL" --redirect ||
        warn "HTTPS 配置写回失败，请手动执行 sudo certbot --nginx -d $DOMAIN"
    else
      log "证书已存在，续期由 certbot 定时任务负责"
    fi
  else
    log "申请 Let's Encrypt 证书"
    $SUDO certbot --nginx -d "$DOMAIN" \
      --non-interactive --agree-tos -m "$EMAIL" --redirect ||
      warn "证书签发失败：确认 DNS 已指向本机、80/443 已放行后重试"
  fi
  $SUDO nginx -t
  if ! $SUDO systemctl reload nginx >/dev/null 2>&1; then
    $SUDO service nginx reload
  fi
else
  warn "未设置 EMAIL，跳过 HTTPS；稍后可执行 sudo certbot --nginx -d $DOMAIN"
fi

SCHEME="http"
if [ -d "$CERT_DIR" ]; then
  SCHEME="https"
fi

cat <<EOF

站点已发布：${SCHEME}://${DOMAIN}

  静态目录：${WEB_ROOT}
  Nginx 配置：/etc/nginx/sites-available/${NGINX_NAME}

更新方式：在服务器上 git pull 后重新执行本脚本。
EOF
