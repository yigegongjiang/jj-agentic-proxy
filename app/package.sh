#!/usr/bin/env bash
# Release 构建 -> 组装 .app (viewer + CLI 同一 bundle) -> ad-hoc 签名。产物: app/build/jj-agentic-proxy.app
# 只构建, 不安装; 装机走 scripts/install.sh (本机预部署总入口 scripts/install-local.sh 把两步串起来)。
# 版本号取自根 Cargo.toml (CLI 与 app 同版本, 单一信源)。Debug 用 `swift build`。
# CLI 二进制随 bundle 分发 -> 升级 app 即升级 CLI; ~/.local/bin 那个入口只是指过来的 symlink。
#
# Usage: ./app/package.sh [--universal]
#   默认      本机架构 (开发/本机安装, 最快)
#   --universal  arm64 + x86_64 通用二进制 (分发用, 见 scripts/make-dist.sh)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
APP_NAME="jj-agentic-proxy"
CLI_NAME="$APP_NAME-cli"
UNIVERSAL=0

for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    -h|--help) echo "usage: $0 [--universal]"; exit 0 ;;
    *) echo "unknown option: $arg (usage: $0 [--universal])" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }

# Package.swift 的 .defaultIsolation 需要 Swift 6.2+; 版本不够会以难懂的 manifest 报错失败, 这里先说清楚
SWIFT_VER="$(swift -version 2>&1 | sed -n 's/.*Apple Swift version \([0-9][0-9.]*\).*/\1/p' | head -1)"
[ -n "$SWIFT_VER" ] || { echo "读不到 swift 版本 (swift -version)" >&2; exit 1; }
case "$(printf '%s\n6.2\n' "$SWIFT_VER" | sort -V | head -1)" in
  6.2) ;;
  *) echo "Swift $SWIFT_VER < 6.2: Package.swift 用了 swift-tools-version:6.2 + defaultIsolation, 需 Xcode 26+" >&2; exit 1 ;;
esac

# MACOSX_DEPLOYMENT_TARGET 与 Package.swift 的 .macOS(.v13) / Info.plist 的 LSMinimumSystemVersion 对齐
export MACOSX_DEPLOYMENT_TARGET=13.0

echo "==> [1/4] 构建 CLI (Release)"
if [ "$UNIVERSAL" = 1 ]; then
  CLI_BIN="$REPO/target/universal-apple-darwin/release/$APP_NAME"
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null 2>&1 || true
    ( cd "$REPO" && cargo build --release --target "$t" )
  done
  mkdir -p "$(dirname "$CLI_BIN")"
  lipo -create -output "$CLI_BIN" \
    "$REPO/target/aarch64-apple-darwin/release/$APP_NAME" \
    "$REPO/target/x86_64-apple-darwin/release/$APP_NAME"
else
  CLI_BIN="$REPO/target/release/$APP_NAME"
  ( cd "$REPO" && cargo build --release )
fi
[ -x "$CLI_BIN" ] || { echo "找不到 CLI 产物: $CLI_BIN" >&2; exit 1; }

echo "==> [2/4] 构建 viewer (Release)"
if [ "$UNIVERSAL" = 1 ]; then
  # --arch 双开走 XCBuild 路径, 直接产出 universal binary (不需要 lipo), 落点也和单架构不同
  ( cd "$ROOT" && swift build -c release --arch arm64 --arch x86_64 )
  APP_BIN="$ROOT/.build/apple/Products/Release/$APP_NAME"
else
  ( cd "$ROOT" && swift build -c release )
  APP_BIN="$(cd "$ROOT" && swift build -c release --show-bin-path)/$APP_NAME"
fi
[ -x "$APP_BIN" ] || { echo "找不到 App 产物: $APP_BIN" >&2; exit 1; }

echo "==> [3/4] 组装 .app bundle"
BUNDLE="$ROOT/build/$APP_NAME.app"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$APP_BIN" "$BUNDLE/Contents/MacOS/$APP_NAME"
cp "$CLI_BIN" "$BUNDLE/Contents/MacOS/$CLI_NAME"
cp "$ROOT/Resources/AppIcon.icns" "$BUNDLE/Contents/Resources/AppIcon.icns"
sed "s/@VERSION@/$VERSION/g" "$ROOT/Resources/Info.plist.in" > "$BUNDLE/Contents/Info.plist"
printf 'APPL????' > "$BUNDLE/Contents/PkgInfo"

echo "==> [4/4] ad-hoc 签名"
# 由内向外: Contents/MacOS 下的第二个 Mach-O 不算 codesign 认得的 nested code, 不先单独签则外层直接
# 报 "code object is not signed at all"; lipo 出来的 fat 二进制也会丢掉链接器给的 adhoc 签名, 必须重签。
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$BUNDLE/Contents/Info.plist")"
codesign --force --sign - "$BUNDLE/Contents/MacOS/$CLI_NAME"
# DR 钉 identifier-only (不含 cdhash): 重建不会让系统按新身份重新提示授权
codesign --force --sign - --identifier "$BUNDLE_ID" -r="designated => identifier \"$BUNDLE_ID\"" "$BUNDLE"
codesign --verify --deep --strict "$BUNDLE"

ARCHS="$(lipo -archs "$BUNDLE/Contents/MacOS/$APP_NAME")"
[ "$ARCHS" = "$(lipo -archs "$BUNDLE/Contents/MacOS/$CLI_NAME")" ] || { echo "viewer 与 CLI 架构不一致" >&2; exit 1; }
if [ "$UNIVERSAL" = 1 ]; then
  case " $ARCHS " in *" arm64 "*) ;; *) echo "universal 产物缺 arm64: $ARCHS" >&2; exit 1 ;; esac
  case " $ARCHS " in *" x86_64 "*) ;; *) echo "universal 产物缺 x86_64: $ARCHS" >&2; exit 1 ;; esac
fi

echo "==> 完成: $BUNDLE (version $VERSION, $ARCHS)"
