#!/usr/bin/env bash
# 重建执行器 sidecar 并重启本地开发环境（desktop + executor）。
# 用法：bash deploy/restart-dev.sh
#
# 为什么需要它：执行器是 Tauri sidecar，tauri dev 只监听 Rust 源码与前端文件，
# 不会重建 apps/desktop/src-tauri/binaries 下的 sidecar 二进制。
# 因此改动 apps/executor 或 crates/ 之后，必须重新拷贝 sidecar 并重启桌面端。
#
# 请在你自己的终端里执行（不要在 AI 沙箱里跑）：
# 沙箱会拦截应用数据目录的写入，执行器会回退成内存模式，重启后策略列表就会是空的。
set -euo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> 1/3 重建执行器 sidecar（debug）"
bash deploy/prepare-sidecar.sh debug

echo "==> 2/3 停止旧的桌面端 / 执行器 / tauri dev"
pkill -f "target/debug/arbpilot-desktop" 2>/dev/null || true
pkill -f "src-tauri/binaries/arbpilot-executor" 2>/dev/null || true
pkill -f "tauri.js dev" 2>/dev/null || true

# 等 1420 端口释放，避免新 vite 换端口导致 devUrl 对不上
for _ in $(seq 1 20); do
  lsof -nP -iTCP:1420 -sTCP:LISTEN >/dev/null 2>&1 || break
  sleep 0.5
done
PORT_PIDS="$(lsof -nP -tiTCP:1420 -sTCP:LISTEN 2>/dev/null || true)"
if [ -n "$PORT_PIDS" ]; then
  kill $PORT_PIDS 2>/dev/null || true
  sleep 1
fi

echo "==> 3/3 启动 tauri dev（Ctrl+C 结束）"
exec pnpm --filter @arbpilot/desktop tauri dev
