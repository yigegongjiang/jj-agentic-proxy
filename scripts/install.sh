#!/usr/bin/env bash
# 安装 jj-agentic-proxy: .app 装 /Applications, 终端命令 ~/.local/bin/jj-agentic-proxy 指向包体内的 CLI。
# 一份副本, 升级只重跑本脚本; 卸载 = 删 app + 删 symlink。
#
# Usage:
#   curl -fsSL https://github.com/yigegongjiang/jj-agentic-proxy/releases/latest/download/install.sh | bash
#   ./install.sh                 # release 包内: 装同目录那个 .app
#   ./scripts/install.sh <.app>  # 本机构建后: 装指定 bundle
set -euo pipefail

REPO_SLUG="yigegongjiang/jj-agentic-proxy"
APP_NAME="jj-agentic-proxy"
CLI_NAME="$APP_NAME-cli"
APP_DEST="/Applications/$APP_NAME.app"
INSTALL_DIR="$HOME/.local/bin"

[ "$(uname -s)" = "Darwin" ] || { echo "只支持 macOS" >&2; exit 1; }
OS_VER="$(sw_vers -productVersion)"
[ "$(printf '%s\n13.0\n' "$OS_VER" | sort -V | head -1)" = "13.0" ] || { echo "需要 macOS 13+, 当前 $OS_VER" >&2; exit 1; }

# 在 Rosetta 终端里 uname -m 会谎报 x86_64 -> 用 proc_translated 校正, 否则 Apple Silicon 会装到 Intel 包
if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
  HOST_ARCH="arm64"
else
  HOST_ARCH="$(uname -m)"
fi

BUNDLE="${1:-}"
# 管道执行 (curl | bash) 时 BASH_SOURCE 不是真实文件 -> 落到远程分支
if [ -z "$BUNDLE" ] && [ -f "${BASH_SOURCE[0]:-}" ]; then
  SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  [ -d "$SELF_DIR/$APP_NAME.app" ] && BUNDLE="$SELF_DIR/$APP_NAME.app"
fi

if [ -z "$BUNDLE" ]; then
  echo "==> 下载最新 release ($HOST_ARCH)"
  # curl 拉的文件不带 com.apple.quarantine, tar 解压也不会继承 -> 装完直接能跑, 不用去系统设置点「仍要打开」
  ASSET="$APP_NAME-macos-$HOST_ARCH.tar.gz"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fL --proto '=https' --tlsv1.2 --progress-bar \
    -o "$TMP/$ASSET" "https://github.com/$REPO_SLUG/releases/latest/download/$ASSET"
  tar -xzf "$TMP/$ASSET" -C "$TMP"
  BUNDLE="$TMP/$APP_NAME.app"
fi

[ -d "$BUNDLE" ] || { echo "找不到 bundle: $BUNDLE" >&2; exit 1; }
[ -x "$BUNDLE/Contents/MacOS/$CLI_NAME" ] || { echo "bundle 内没有 CLI: $BUNDLE/Contents/MacOS/$CLI_NAME" >&2; exit 1; }

BUNDLE_ARCH="$(lipo -archs "$BUNDLE/Contents/MacOS/$CLI_NAME")"
case " $BUNDLE_ARCH " in
  *" $HOST_ARCH "*) ;;
  *)
    # arm64 机器跑 Intel 包只是慢 + 会被系统提示淘汰; Intel 机器拿 arm64 包则根本起不来
    [ "$HOST_ARCH" = "arm64" ] || { echo "包是 $BUNDLE_ARCH, 本机是 $HOST_ARCH: 跑不起来, 请下 $HOST_ARCH 那份" >&2; exit 1; }
    echo "warning: 包是 $BUNDLE_ARCH, 本机是 arm64 -> 会经 Rosetta 运行且系统会弹 Intel 淘汰提示; 建议换 arm64 那份" >&2
    ;;
esac

echo "==> 校验签名"
# ad-hoc 签名验不了来源, 但能验完整性: 传输 / 解压损坏在这里就暴露, 而不是运行时被系统 kill
codesign --verify --deep --strict "$BUNDLE" || { echo "签名校验失败, 包可能已损坏, 请重新下载" >&2; exit 1; }

echo "==> 安装到 $APP_DEST"
# 不建议 sudo: root 的 HOME 是 /var/root, 终端入口会建到那里, 当前用户反而找不到命令
[ -w /Applications ] || { echo "/Applications 不可写: 换管理员账号重跑 (别加 sudo, 终端入口会建到 root 名下)" >&2; exit 1; }
# 只关 viewer 实例 (-x 整条命令行精确匹配): 模糊匹配会连 CLI daemon 一起杀掉
pkill -f -x "$APP_DEST/Contents/MacOS/$APP_NAME" 2>/dev/null || true
rm -rf "$APP_DEST"
# ditto 而非 cp: 保留签名所需的元数据; 覆盖安装用 cp 会让 macOS 代码签名缓存失效直接 Killed: 9
ditto "$BUNDLE" "$APP_DEST"
# 从 DMG 拖进来 / 浏览器下载 zip 解压的场景会带 quarantine, 无签名会被 Gatekeeper 拦 -> 就地摘掉
xattr -dr com.apple.quarantine "$APP_DEST" 2>/dev/null || true

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
