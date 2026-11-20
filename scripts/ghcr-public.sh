#!/usr/bin/env bash
# 一次性把 GHCR 上的三个镜像包设为公开，避免部署服务器必须登录才能 pull。
#
# 需要已登录 GitHub CLI（gh auth login），且账号对 waytoweb3 组织的包有管理权限：
#   bash scripts/ghcr-public.sh [owner]
#
# 之后可用匿名拉取验证：
#   docker logout ghcr.io && docker pull ghcr.io/waytoweb3/arbpilot-executor:latest
set -euo pipefail

OWNER="${1:-waytoweb3}"
PACKAGES=(
  "arbpilot-executor"
  "arbpilot-payments"
  "arbpilot-accounts"
)

command -v gh >/dev/null 2>&1 || {
  printf '需要 GitHub CLI：https://cli.github.com/\n' >&2
  exit 1
}
gh auth status >/dev/null 2>&1 || {
  printf '请先执行 gh auth login\n' >&2
  exit 1
}

for package in "${PACKAGES[@]}"; do
  printf '设置 %s/%s 为公开…\n' "$OWNER" "$package"
  # 容器包属于容器注册表 API；组织与个人账号的端点不同，先试组织，失败再试用户。
  if ! gh api --method PATCH \
    -H "Accept: application/vnd.github+json" \
    "/orgs/${OWNER}/packages/container/${package}" \
    -f visibility=public >/dev/null 2>&1; then
    gh api --method PATCH \
      -H "Accept: application/vnd.github+json" \
      "/users/${OWNER}/packages/container/${package}" \
      -f visibility=public >/dev/null
  fi
done

printf '完成。建议执行 docker logout ghcr.io 后重新 pull 验证匿名访问：\n'
for package in "${PACKAGES[@]}"; do
  printf '  docker pull ghcr.io/%s/%s:latest\n' "$OWNER" "$package"
done
