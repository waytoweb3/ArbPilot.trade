#!/usr/bin/env bash
# 打出自托管部署包与版本清单（manifest.json），供客户端「一键部署 / 更新」使用。
# 用法：bash deploy/package.sh [输出目录]
# 发布时把生成的 tar.gz 与 manifest.json 一并上传到 $ARBPILOT_RELEASE_BASE_URL。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-$ROOT_DIR/dist/release}"
BASE_URL="${ARBPILOT_RELEASE_BASE_URL:-https://download.arbpilot.trade/executor}"
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT_DIR/Cargo.toml" | head -n 1)"

[ -n "$VERSION" ] || {
  echo "无法从 Cargo.toml 读取版本号" >&2
  exit 1
}

NAME="arbpilot-executor-$VERSION"
TARBALL="$OUT_DIR/$NAME.tar.gz"
MANIFEST="$OUT_DIR/manifest.json"

mkdir -p "$OUT_DIR"
rm -f "$TARBALL" "$MANIFEST"

# 执行器镜像由 GitHub Actions 构建（.github/workflows/executor-image.yml），服务器只拉镜像，
# 因此部署包只需要 compose、安装脚本与文档，不再包含源码。
# macOS 的 bsdtar 会把扩展属性写成 `._` AppleDouble 成员，解包后会被 sqlx 当作迁移文件，必须排除。
COPYFILE_DISABLE=1 tar czf "$TARBALL" -C "$ROOT_DIR" \
  --exclude='._*' \
  --exclude='*.log' \
  deploy docs

if command -v sha256sum >/dev/null 2>&1; then
  SHA256="$(sha256sum "$TARBALL" | awk '{print $1}')"
else
  SHA256="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
fi
SIZE="$(wc -c <"$TARBALL" | tr -d ' ')"

cat >"$MANIFEST" <<EOF
{
  "version": "$VERSION",
  "bundle_url": "$BASE_URL/$NAME.tar.gz",
  "sha256": "$SHA256",
  "size": $SIZE,
  "released_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "notes": ""
}
EOF

# 服务器命令行安装入口，供 `curl -fsSL $BASE_URL/install.sh | sudo bash` 使用。
INSTALLER="$OUT_DIR/install.sh"
cp "$ROOT_DIR/deploy/bootstrap.sh" "$INSTALLER"
chmod 755 "$INSTALLER"

cat <<EOF
已生成：
  $TARBALL
  $MANIFEST
  $INSTALLER

版本：$VERSION
大小：$SIZE 字节
sha256：$SHA256

上传这三个文件到 $BASE_URL/ ，客户端即可检查到该版本；
服务器命令行安装 / 升级则直接使用：
  curl -fsSL $BASE_URL/install.sh | sudo bash
EOF
