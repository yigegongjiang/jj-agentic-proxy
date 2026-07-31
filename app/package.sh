#!/usr/bin/env bash
# 本机架构 Release 构建 -> 组装 .app -> ad-hoc 签名 -> 装 /Applications。
# 版本号取自根 Cargo.toml (CLI 与 app 同版本, 单一信源)。Debug 用 `swift build`。
# 本机自建 .app 无 quarantine, Gatekeeper 不校验签名, 直接可跑。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="jj-agentic-proxy-app"
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/../Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }

echo "==> [1/4] 构建 (Release, $(uname -m))"
( cd "$ROOT" && swift build -c release )
APP_BIN="$(cd "$ROOT" && swift build -c release --show-bin-path)/$APP_NAME"
[ -x "$APP_BIN" ] || { echo "找不到 App 产物: $APP_BIN" >&2; exit 1; }

echo "==> [2/4] 组装 .app bundle"
BUNDLE="$ROOT/build/$APP_NAME.app"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$APP_BIN" "$BUNDLE/Contents/MacOS/$APP_NAME"
cp "$ROOT/Resources/AppIcon.icns" "$BUNDLE/Contents/Resources/AppIcon.icns"
sed "s/@VERSION@/$VERSION/g" "$ROOT/Resources/Info.plist.in" > "$BUNDLE/Contents/Info.plist"
printf 'APPL????' > "$BUNDLE/Contents/PkgInfo"

echo "==> [3/4] ad-hoc 签名"
# DR 钉 identifier-only (不含 cdhash): 重建不会让系统按新身份重新提示授权
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$BUNDLE/Contents/Info.plist")"
codesign --force --sign - --identifier "$BUNDLE_ID" -r="designated => identifier \"$BUNDLE_ID\"" "$BUNDLE"

echo "==> [4/4] 安装到 /Applications"
# 关旧实例, 否则占用二进制导致拷贝 / 运行异常
pkill -f "/$APP_NAME\.app/Contents/MacOS/$APP_NAME" 2>/dev/null || true
rm -rf "/Applications/$APP_NAME.app"
ditto "$BUNDLE" "/Applications/$APP_NAME.app"

echo "==> 完成: /Applications/$APP_NAME.app (version $VERSION, $(lipo -archs "$BUNDLE/Contents/MacOS/$APP_NAME" 2>/dev/null))"
