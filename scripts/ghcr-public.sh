#!/usr/bin/env bash
# 一次性把 GHCR 上的三个镜像包设为公开，避免部署服务器必须登录才能 pull。
#
# 注意：GitHub 只允许 classic PAT 修改包可见性（网页授权的 OAuth 令牌会返回 404）。
# 先在 https://github.com/settings/tokens/new?scopes=read:packages,write:packages,repo
# 生成 classic PAT，然后：
#   GH_TOKEN=<classic-pat> bash scripts/ghcr-public.sh [owner]
# 未提供 GH_TOKEN 时脚本会使用 gh 当前登录身份，可能因令牌类型不符而失败。
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
  # 容器包属于容器注册表 API；组织与个人账号的端点不同，先探测再修改。
  endpoint=""
  for candidate in \
    "/orgs/${OWNER}/packages/container/${package}" \
    "/users/${OWNER}/packages/container/${package}"; do
    if gh api "$candidate" >/dev/null 2>&1; then
      endpoint="$candidate"
      break
    fi
  done
  if [ -z "$endpoint" ]; then
    printf '  跳过：%s 尚未发布过镜像（首次推送后再执行本脚本即可）\n' "$package"
    continue
  fi
  gh api --method PATCH \
    -H "Accept: application/vnd.github+json" \
    "$endpoint" \
    -f visibility=public >/dev/null
  printf '  已设为公开\n'
done

printf '完成。建议执行 docker logout ghcr.io 后重新 pull 验证匿名访问：\n'
for package in "${PACKAGES[@]}"; do
  printf '  docker pull ghcr.io/%s/%s:latest\n' "$OWNER" "$package"
done
