#!/usr/bin/env bash
# 预部署总入口: app bundle 装 /Applications (viewer + CLI 都在里面), 再把 ~/.local/bin/jj-agentic-proxy 指过去。
# Usage: ./scripts/install-local.sh   (在任意目录跑, 脚本自己 cd 到仓库根)
# 升级只重跑本脚本: CLI 是 bundle 内那份的 symlink, 不存在两份各自升级 / 版本错位。

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help) echo "usage: $0"; exit 0 ;;
    *) echo "unknown option: $arg (usage: $0)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

BIN_NAME="jj-agentic-proxy"
INSTALL_DIR="$HOME/.local/bin"
CLI_IN_APP="/Applications/${BIN_NAME}.app/Contents/MacOS/${BIN_NAME}-cli"

./app/package.sh

echo
echo "==> 链接 CLI: ${INSTALL_DIR}/${BIN_NAME} -> ${CLI_IN_APP}"
[ -x "$CLI_IN_APP" ] || { echo "bundle 内没有 CLI: $CLI_IN_APP" >&2; exit 1; }
mkdir -p "$INSTALL_DIR"
# symlink 而非拷贝: app 是唯一副本, 二进制换了 alias 自动跟随 (-f 覆盖旧的普通文件 / 旧链接)
ln -sfn "$CLI_IN_APP" "${INSTALL_DIR}/${BIN_NAME}"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "warning: add to PATH → export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

echo "==> 验证"
"${INSTALL_DIR}/${BIN_NAME}" --version

echo "==> 旧代理进程仍是上一版: 执行 ${BIN_NAME} start 切到新版本 (start 自带 restart)"
