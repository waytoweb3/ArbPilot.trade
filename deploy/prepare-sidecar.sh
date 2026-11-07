#!/usr/bin/env bash
# 把执行器二进制按 Tauri sidecar 命名规则放进 src-tauri/binaries。
# 用法：bash deploy/prepare-sidecar.sh [debug|release]
# 桌面端打包与开发前都需要先执行，Tauri 构建时会校验该文件存在。
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-debug}"
TARGET_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"

[ -n "$TARGET_TRIPLE" ] || {
  echo "无法读取目标三元组（rustc -vV）" >&2
  exit 1
}

if [ "$PROFILE" = "release" ]; then
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p arbpilot-executor --release
  SOURCE="$ROOT_DIR/target/release/arbpilot-executor"
else
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p arbpilot-executor
  SOURCE="$ROOT_DIR/target/debug/arbpilot-executor"
fi

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) SOURCE="$SOURCE.exe" ;;
esac

[ -f "$SOURCE" ] || {
  echo "未找到执行器二进制：$SOURCE" >&2
  exit 1
}

BIN_DIR="$ROOT_DIR/apps/desktop/src-tauri/binaries"
mkdir -p "$BIN_DIR"
DEST="$BIN_DIR/arbpilot-executor-$TARGET_TRIPLE"
case "$SOURCE" in
  *.exe) DEST="$DEST.exe" ;;
esac

cp "$SOURCE" "$DEST"
chmod +x "$DEST" 2>/dev/null || true

echo "已就绪 sidecar：$DEST"
