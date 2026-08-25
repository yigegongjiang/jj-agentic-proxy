#!/usr/bin/env bash
# 分发打包: 按架构各出一个 dmg (arm64 / x86_64) -> dist/ 下 dmg + SHA256SUMS + RELEASE_NOTES.md。
# GitHub Actions 只是这个脚本的薄壳 -> 本机跑一遍即可复现 CI 结果。
# 只发 dmg: 用户下载 -> 拖进「应用程序」-> 打开 app 点「好」装终端命令 (CLIInstall.swift), 不需要任何安装脚本。
# 不发 universal: 包体内混进 Intel slice 会让 macOS 26.4+ 弹「Support Ending for Intel-based Apps」。
#
# Usage: ./scripts/make-dist.sh [--expect-version X.Y.Z]
#   --expect-version  与 Cargo.toml 版本比对不上就失败 (CI 用 tag 名传入, 防版本/tag 错位)
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$PWD"
APP_NAME="jj-agentic-proxy"
DIST="$REPO/dist"
ARCHS="arm64 x86_64"
EXPECT_VERSION=""

while [ $# -gt 0 ]; do
  case "$1" in
    --expect-version) EXPECT_VERSION="${2:-}"; shift 2 ;;
    -h|--help) echo "usage: $0 [--expect-version X.Y.Z]"; exit 0 ;;
    *) echo "unknown option: $1 (usage: $0 [--expect-version X.Y.Z])" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "读不到 Cargo.toml 版本号" >&2; exit 1; }
if [ -n "$EXPECT_VERSION" ] && [ "$EXPECT_VERSION" != "$VERSION" ]; then
  echo "版本错位: 期望 $EXPECT_VERSION, Cargo.toml 是 $VERSION" >&2
  exit 1
fi

rm -rf "$DIST"
mkdir -p "$DIST"

for ARCH in $ARCHS; do
  echo
  echo "########## $ARCH ##########"
  ./app/package.sh --arch "$ARCH"
  BUNDLE="$REPO/app/build/$APP_NAME.app"
  BASE="$APP_NAME-macos-$ARCH"

  echo "==> 打 $BASE.dmg"
  # 卷内只放 .app + /Applications 快捷方式 -> 打开就是一次拖拽, 没有第二个可点的东西
  DMG_SRC="$DIST/dmg-src"
  rm -rf "$DMG_SRC"
  mkdir -p "$DMG_SRC"
  # ditto 而非 cp -R: 保留签名相关元数据
  ditto "$BUNDLE" "$DMG_SRC/$APP_NAME.app"
  ln -s /Applications "$DMG_SRC/Applications"
  hdiutil create -quiet -volname "$APP_NAME $VERSION" -srcfolder "$DMG_SRC" \
    -fs HFS+ -format UDZO -ov "$DIST/$BASE.dmg"
  rm -rf "$DMG_SRC"
done

echo "==> 提取 release notes"
# 锚定 `## [X.Y.Z]` 到下一个 `## [`; CHANGELOG 顶部有 When Editing 说明块, 不能用「第一段」这种取法
awk -v v="$VERSION" '
  $0 ~ "^## \\[" v "\\]" { on = 1; next }
  on && /^## \[/ { exit }
  on { print }
' "$REPO/CHANGELOG.md" | sed '/./,$!d' > "$DIST/RELEASE_NOTES.md"
[ -s "$DIST/RELEASE_NOTES.md" ] || { echo "CHANGELOG.md 里没有 [$VERSION] 段落" >&2; exit 1; }

echo "==> 校验和"
( cd "$DIST" && shasum -a 256 ./*.dmg | sed 's| \./| |' > SHA256SUMS )

echo "==> 完成 (version $VERSION)"
ls -lh "$DIST"
