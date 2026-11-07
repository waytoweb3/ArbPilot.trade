#!/usr/bin/env bash
# 打包桌面客户端更新包，并生成 latest.json（客户端「设置 → 检查更新」读取）。
# 用法：
#   bash deploy/package-desktop.sh [输出目录]
#   ARBPILOT_DESKTOP_NOTES="修复下单确认" bash deploy/package-desktop.sh
# 依赖签名私钥：默认读取 ~/.tauri/arbpilot.key，可用 TAURI_SIGNING_PRIVATE_KEY 覆盖。
set -euo pipefail

# GUI 启动的终端常常缺少 cargo，这里补上 rustup 的默认安装路径。
export PATH="$HOME/.cargo/bin:$PATH"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP_DIR="$ROOT_DIR/apps/desktop"
OUT_DIR="${1:-$ROOT_DIR/dist/desktop}"
BASE_URL="${ARBPILOT_DESKTOP_BASE_URL:-https://download.arbpilot.trade/desktop}"
KEY="${TAURI_SIGNING_PRIVATE_KEY:-$HOME/.tauri/arbpilot.key}"

[ -f "$KEY" ] || {
  echo "缺少签名私钥：$KEY" >&2
  echo "先生成一次：cd apps/desktop && pnpm tauri signer generate -w ~/.tauri/arbpilot.key" >&2
  exit 1
}

VERSION="$(sed -n 's/^  "version": "\([^"]*\)",$/\1/p' "$DESKTOP_DIR/src-tauri/tauri.conf.json" | head -n 1)"
[ -n "$VERSION" ] || {
  echo "无法从 tauri.conf.json 读取版本号" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin)
    if [ "$(uname -m)" = "arm64" ]; then PLATFORM="darwin-aarch64"; else PLATFORM="darwin-x86_64"; fi
    ;;
  MINGW* | MSYS* | CYGWIN*) PLATFORM="windows-x86_64" ;;
  Linux) PLATFORM="linux-x86_64" ;;
  *)
    echo "不支持的构建平台：$(uname -s)" >&2
    exit 1
    ;;
esac

echo "构建 ArbPilot ${VERSION}（${PLATFORM}）…"
# 先把执行器编译成 sidecar，安装包内随桌面端一起分发并在启动时拉起。
bash "$ROOT_DIR/deploy/prepare-sidecar.sh" release
(
  cd "$DESKTOP_DIR"
  TAURI_SIGNING_PRIVATE_KEY="$KEY" \
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" \
    pnpm tauri build
)

BUNDLE_DIR="$DESKTOP_DIR/src-tauri/target/release/bundle"
# Cargo workspace 统一把产物放在仓库根目录的 target 下。
[ -d "$BUNDLE_DIR" ] || BUNDLE_DIR="$ROOT_DIR/target/release/bundle"
SIG="$(find "$BUNDLE_DIR" -name '*.sig' -type f -exec ls -t {} + | head -n 1)"
[ -n "$SIG" ] || {
  echo "没有找到更新包签名，请确认 tauri.conf.json 的 bundle.createUpdaterArtifacts 为 true" >&2
  exit 1
}

ARTIFACT="${SIG%.sig}"
FILENAME="$(basename "$ARTIFACT")"
mkdir -p "$OUT_DIR"
cp "$ARTIFACT" "$OUT_DIR/$FILENAME"

node - "$OUT_DIR/latest.json" "$VERSION" "$PLATFORM" "$BASE_URL/$FILENAME" "$SIG" <<'NODE'
const fs = require("fs");
const [file, version, platform, url, sigPath] = process.argv.slice(2);
const signature = fs.readFileSync(sigPath, "utf8").trim();

let platforms = {};
if (fs.existsSync(file)) {
  try {
    const old = JSON.parse(fs.readFileSync(file, "utf8"));
    // 只有同一个版本才合并，避免把旧包地址带到新版本里。
    if (old.version === version && old.platforms) platforms = old.platforms;
  } catch {
    platforms = {};
  }
}
platforms[platform] = { signature, url };

const manifest = {
  version,
  notes: process.env.ARBPILOT_DESKTOP_NOTES || "",
  pub_date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
  platforms,
};
fs.writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`);
NODE

cat <<EOF

已生成：
  $OUT_DIR/$FILENAME
  $OUT_DIR/latest.json

版本：$VERSION
平台：$PLATFORM

上传这两个文件到 $BASE_URL/ ，客户端「设置 → 检查更新」即可发现新版本。
本脚本只负责当前系统；Windows 安装包由 GitHub Actions 的 desktop-windows 工作流构建，
其 latest.json 会合并线上同一版本的其它平台条目。
EOF
