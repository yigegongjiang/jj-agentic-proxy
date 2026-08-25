#!/usr/bin/env bash
# 分发打包: universal 构建 -> dist/ 下产出 tar.gz + dmg + install.sh + SHA256SUMS + RELEASE_NOTES.md。
# GitHub Actions 只是这个脚本的薄壳 -> 本机跑一遍即可复现 CI 结果。
#
# Usage: ./scripts/make-dist.sh [--expect-version X.Y.Z]
#   --expect-version  与 Cargo.toml 版本比对不上就失败 (CI 用 tag 名传入, 防版本/tag 错位)
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$PWD"
APP_NAME="jj-agentic-proxy"
DIST="$REPO/dist"
BASE="$APP_NAME-macos-universal"
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

./app/package.sh --universal
BUNDLE="$REPO/app/build/$APP_NAME.app"

echo "==> 组装分发目录"
rm -rf "$DIST"
mkdir -p "$DIST"
STAGE="$DIST/stage"
mkdir -p "$STAGE"
# ditto 而非 cp -R: 保留签名相关元数据
ditto "$BUNDLE" "$STAGE/$APP_NAME.app"
cp "$REPO/scripts/install.sh" "$STAGE/install.sh"
chmod +x "$STAGE/install.sh"

echo "==> 打 tar.gz"
# --no-mac-metadata --no-xattrs: 签名内嵌在 Mach-O 里, 不依赖 xattr; 去掉可避免 ._ 伴生文件与 quarantine 残留
tar --no-mac-metadata --no-xattrs -czf "$DIST/$BASE.tar.gz" -C "$STAGE" "$APP_NAME.app" install.sh

echo "==> 打 dmg"
# 拖拽安装用: 卷内放 .app + /Applications 快捷方式; HFS+ 保证老系统也能挂载
DMG_SRC="$DIST/dmg-src"
mkdir -p "$DMG_SRC"
ditto "$BUNDLE" "$DMG_SRC/$APP_NAME.app"
ln -s /Applications "$DMG_SRC/Applications"
cp "$REPO/scripts/install.sh" "$DMG_SRC/install.sh"
hdiutil create -quiet -volname "$APP_NAME $VERSION" -srcfolder "$DMG_SRC" \
  -fs HFS+ -format UDZO -ov "$DIST/$BASE.dmg"
rm -rf "$DMG_SRC" "$STAGE"

echo "==> 附带 install.sh (curl 一条命令安装的入口)"
cp "$REPO/scripts/install.sh" "$DIST/install.sh"

echo "==> 提取 release notes"
# 锚定 `## [X.Y.Z]` 到下一个 `## [`; CHANGELOG 顶部有 When Editing 说明块, 不能用「第一段」这种取法
awk -v v="$VERSION" '
  $0 ~ "^## \\[" v "\\]" { on = 1; next }
  on && /^## \[/ { exit }
  on { print }
' "$REPO/CHANGELOG.md" | sed '/./,$!d' > "$DIST/RELEASE_NOTES.md"
[ -s "$DIST/RELEASE_NOTES.md" ] || { echo "CHANGELOG.md 里没有 [$VERSION] 段落" >&2; exit 1; }

echo "==> 校验和"
( cd "$DIST" && shasum -a 256 "$BASE.tar.gz" "$BASE.dmg" install.sh > SHA256SUMS )

echo "==> 完成 (version $VERSION)"
ls -lh "$DIST"
