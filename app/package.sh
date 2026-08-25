#!/usr/bin/env bash
# 本机架构 Release 构建 -> 组装 .app (viewer + CLI 同一 bundle) -> ad-hoc 签名 -> 装 /Applications。
# 版本号取自根 Cargo.toml (CLI 与 app 同版本, 单一信源)。Debug 用 `swift build`。
# CLI 二进制随 bundle 分发 -> 升级 app 即升级 CLI; ~/.local/bin 那个入口只是指过来的 symlink (见 scripts/install-local.sh)。
# 本机自建 .app 无 quarantine, Gatekeeper 不校验签名, 直接可跑。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="jj-agentic-proxy"
CLI_NAME="$APP_NAME-cli"
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/../Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }

echo "==> [1/5] 构建 CLI (Release)"
( cd "$ROOT/.." && cargo build --release )
CLI_BIN="$ROOT/../target/release/$APP_NAME"
[ -x "$CLI_BIN" ] || { echo "找不到 CLI 产物: $CLI_BIN" >&2; exit 1; }

echo "==> [2/5] 构建 viewer (Release, $(uname -m))"
( cd "$ROOT" && swift build -c release )
APP_BIN="$(cd "$ROOT" && swift build -c release --show-bin-path)/$APP_NAME"
[ -x "$APP_BIN" ] || { echo "找不到 App 产物: $APP_BIN" >&2; exit 1; }

echo "==> [3/5] 组装 .app bundle"
BUNDLE="$ROOT/build/$APP_NAME.app"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$APP_BIN" "$BUNDLE/Contents/MacOS/$APP_NAME"
cp "$CLI_BIN" "$BUNDLE/Contents/MacOS/$CLI_NAME"
cp "$ROOT/Resources/AppIcon.icns" "$BUNDLE/Contents/Resources/AppIcon.icns"
sed "s/@VERSION@/$VERSION/g" "$ROOT/Resources/Info.plist.in" > "$BUNDLE/Contents/Info.plist"
printf 'APPL????' > "$BUNDLE/Contents/PkgInfo"

echo "==> [4/5] ad-hoc 签名"
# DR 钉 identifier-only (不含 cdhash): 重建不会让系统按新身份重新提示授权
# Contents/MacOS 下的 CLI 由 codesign 自动按 nested code 一并签名 + 密封, 无需单独签
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$BUNDLE/Contents/Info.plist")"
codesign --force --sign - --identifier "$BUNDLE_ID" -r="designated => identifier \"$BUNDLE_ID\"" "$BUNDLE"

echo "==> [5/5] 安装到 /Applications"
# 只关 viewer 实例 (-x 整条命令行精确匹配): 模糊匹配会连 CLI daemon 一起杀掉
pkill -f -x "/Applications/$APP_NAME.app/Contents/MacOS/$APP_NAME" 2>/dev/null || true
rm -rf "/Applications/$APP_NAME.app"
ditto "$BUNDLE" "/Applications/$APP_NAME.app"

echo "==> 完成: /Applications/$APP_NAME.app (version $VERSION, $(lipo -archs "$BUNDLE/Contents/MacOS/$APP_NAME" 2>/dev/null))"
echo "    内含 CLI: Contents/MacOS/$CLI_NAME"
