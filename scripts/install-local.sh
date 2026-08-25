#!/usr/bin/env bash
# 本机预部署总入口: 本机架构构建 -> 装 /Applications -> 链接终端命令 ~/.local/bin/jj-agentic-proxy。
# 只给开发/发版前自检用; 用户侧安装是「下 dmg 拖进应用程序 -> 打开 app 点『好』」, 不跑任何脚本。
# 分发打包走 scripts/make-dist.sh。
#
# Usage: ./scripts/install-local.sh   (在任意目录跑, 脚本自己 cd 到仓库根)
set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help) echo "usage: $0"; exit 0 ;;
    *) echo "unknown option: $arg (usage: $0)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

APP_NAME="jj-agentic-proxy"
CLI_NAME="$APP_NAME-cli"
BUNDLE="$PWD/app/build/$APP_NAME.app"
APP_DEST="/Applications/$APP_NAME.app"
INSTALL_DIR="$HOME/.local/bin"

./app/package.sh

[ -x "$BUNDLE/Contents/MacOS/$CLI_NAME" ] || { echo "bundle 内没有 CLI: $BUNDLE/Contents/MacOS/$CLI_NAME" >&2; exit 1; }

echo "==> 安装到 $APP_DEST"
# 不加 sudo: root 的 HOME 是 /var/root, 终端入口会建到那里, 当前用户反而找不到命令
[ -w /Applications ] || { echo "/Applications 不可写: 换管理员账号重跑 (别加 sudo, 终端入口会建到 root 名下)" >&2; exit 1; }
# 只关 viewer 实例 (-x 整条命令行精确匹配): 模糊匹配会连包体内 CLI 跑的 daemon 一起杀掉
pkill -f -x "$APP_DEST/Contents/MacOS/$APP_NAME" 2>/dev/null || true
rm -rf "$APP_DEST"
# ditto 而非 cp: 保留签名所需的元数据; 覆盖安装用 cp 会让 macOS 代码签名缓存失效直接 Killed: 9
ditto "$BUNDLE" "$APP_DEST"

echo "==> 链接 CLI: $INSTALL_DIR/$APP_NAME -> $APP_DEST/Contents/MacOS/$CLI_NAME"
mkdir -p "$INSTALL_DIR"
# symlink 而非拷贝: app 是唯一副本, 二进制换了入口自动跟随 (-f 覆盖旧的普通文件 / 旧链接)
ln -sfn "$APP_DEST/Contents/MacOS/$CLI_NAME" "$INSTALL_DIR/$APP_NAME"

echo "==> 验证"
"$INSTALL_DIR/$APP_NAME" --version

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "warning: $INSTALL_DIR 不在 PATH -> export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
echo "==> 旧代理进程仍是上一版: 执行 $APP_NAME start 切到新版本 (start 自带 restart)"
