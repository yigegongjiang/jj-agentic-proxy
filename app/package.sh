#!/usr/bin/env bash
# Release 构建 -> 组装 .app (viewer + CLI 同一 bundle) -> ad-hoc 签名。产物: app/build/jj-agentic-proxy.app
# 只构建, 不安装; 本机装机走 scripts/install-local.sh (构建 + 装 /Applications + 链接终端命令)。
# 版本号取自根 Cargo.toml (CLI 与 app 同版本, 单一信源)。Debug 用 `swift build`。
# CLI 二进制随 bundle 分发 -> 升级 app 即升级 CLI; ~/.local/bin 那个入口只是指过去的 symlink。
#
# Usage: ./app/package.sh [--arch arm64|x86_64]
#   默认取本机架构。单架构而非 universal: Apple Silicon 上的包体内不留 Intel 代码, macOS 26.4+ 才不会弹
#   「Support Ending for Intel-based Apps」; Rosetta 2 在 macOS 28 消失后那份 slice 也只是死重。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
APP_NAME="jj-agentic-proxy"
CLI_NAME="$APP_NAME-cli"
ARCH="$(uname -m)"

while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="${2:-}"; shift 2 ;;
    -h|--help) echo "usage: $0 [--arch arm64|x86_64]"; exit 0 ;;
    *) echo "unknown option: $1 (usage: $0 [--arch arm64|x86_64])" >&2; exit 2 ;;
  esac
done

case "$ARCH" in
  arm64) RUST_TARGET="aarch64-apple-darwin" ;;
  x86_64) RUST_TARGET="x86_64-apple-darwin" ;;
  *) echo "不支持的架构: $ARCH (只有 arm64 / x86_64)" >&2; exit 2 ;;
esac

VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }

# Package.swift 的 .defaultIsolation 需要 Swift 6.2+; 版本不够会以难懂的 manifest 报错失败, 这里先说清楚
SWIFT_VER="$(swift -version 2>&1 | sed -n 's/.*Apple Swift version \([0-9][0-9.]*\).*/\1/p' | head -1)"
[ -n "$SWIFT_VER" ] || { echo "读不到 swift 版本 (swift -version)" >&2; exit 1; }
case "$(printf '%s\n6.2\n' "$SWIFT_VER" | sort -V | head -1)" in
  6.2) ;;
  *) echo "Swift $SWIFT_VER < 6.2: Package.swift 用了 swift-tools-version:6.2 + defaultIsolation, 需 Xcode 26+" >&2; exit 1 ;;
esac

# 与 Package.swift 的 .macOS(.v13) / Info.plist 的 LSMinimumSystemVersion 对齐 (Rust 侧默认 11.0, 不设会错位)
export MACOSX_DEPLOYMENT_TARGET=13.0

echo "==> [1/4] 构建 CLI (Release, $ARCH)"
rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true
( cd "$REPO" && cargo build --release --target "$RUST_TARGET" )
CLI_BIN="$REPO/target/$RUST_TARGET/release/$APP_NAME"
[ -x "$CLI_BIN" ] || { echo "找不到 CLI 产物: $CLI_BIN" >&2; exit 1; }

echo "==> [2/4] 构建 viewer (Release, $ARCH)"
# 落点随本机/交叉编译而变, 一律问 --show-bin-path, MUST NOT 写死
( cd "$ROOT" && swift build -c release --arch "$ARCH" )
APP_BIN="$(cd "$ROOT" && swift build -c release --arch "$ARCH" --show-bin-path)/$APP_NAME"
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
# 报 "code object is not signed at all"
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$BUNDLE/Contents/Info.plist")"
codesign --force --sign - "$BUNDLE/Contents/MacOS/$CLI_NAME"
# DR 钉 identifier-only (不含 cdhash): 重建不会让系统按新身份重新提示授权
codesign --force --sign - --identifier "$BUNDLE_ID" -r="designated => identifier \"$BUNDLE_ID\"" "$BUNDLE"
codesign --verify --deep --strict "$BUNDLE"

# 单架构闸: 混进另一架构的 slice 会让 macOS 26.4+ 把整个 app 判为含 Intel 组件
for bin in "$APP_NAME" "$CLI_NAME"; do
  got="$(lipo -archs "$BUNDLE/Contents/MacOS/$bin")"
  [ "$got" = "$ARCH" ] || { echo "$bin 架构不对: 期望 $ARCH, 实为 $got" >&2; exit 1; }
done

echo "==> 完成: $BUNDLE (version $VERSION, $ARCH)"
