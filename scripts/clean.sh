#!/usr/bin/env bash
# 清掉本机编译产物, 只保留还有用的那层。发布流程最后一步跑一次 -> target 不跨版本无限累积。
#
# 默认档只清 debug 层 (占比最大, 重建成本 = 一次 clippy + test):
#   target/debug  app/.build
# cargo 的 release 缓存留着, 下一次 install-local.sh / make-dist.sh 不必冷编 CLI。
# app/.build 整个删 -> Swift 侧 (含 release) 会冷编一次, 不值得为它做子目录级筛选。
# app/build 与 dist/ 不在清理表里: package.sh / make-dist.sh 每次自己 rm -rf, 已自管。
#
# Usage: ./scripts/clean.sh [--all] [--dry-run]
#   --all      连 release + 交叉编译 target + 包体 + dmg 一起清 (下一次发布要付一次冷 release 构建)
#   --dry-run  只报体积, 不删
set -euo pipefail

MODE=debug
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --all) MODE=all; shift ;;
    --dry-run|-n) DRY=1; shift ;;
    -h|--help) echo "usage: $0 [--all] [--dry-run]"; exit 0 ;;
    *) echo "unknown option: $1 (usage: $0 [--all] [--dry-run])" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

# 这个脚本除了 rm -rf 没别的动作: 认不出是本仓库就退出, MUST NOT 在错误的 cwd 上开删
grep -q '^name = "jj-agentic-proxy"$' Cargo.toml 2>/dev/null \
  || { echo "这里不是 jj-agentic-proxy 仓库根 ($PWD): 拒绝删除" >&2; exit 2; }

if [ "$MODE" = all ]; then
  PATHS=(target app/.build app/build dist)
else
  PATHS=(target/debug app/.build)
fi

# 逐项报体积只用于展示。实际回收量按 df 差值算 —— cargo 在 deps/ 与 incremental/ 之间打硬链接,
# 分目录 du 会把共享块重复计入, 加总出来的数字比真实回收量大。
echo "==> 待清理 (mode=$MODE)"
found=0
for p in "${PATHS[@]}"; do
  if [ -e "$p" ]; then
    printf '    %-56s %s\n' "$p" "$(du -sh "$p" 2>/dev/null | cut -f1)"
    found=1
  fi
done
[ "$found" = 1 ] || { echo "    (无产物)"; exit 0; }

if [ "$DRY" = 1 ]; then
  echo
  echo "==> dry-run, 未删除。仓库外的缓存 (本脚本不动, 需要时自己清):"
  for c in "$HOME/.cargo/registry" "$HOME/Library/Caches/org.swift.swiftpm"; do
    [ -d "$c" ] && printf '    %-56s %s\n' "${c/#$HOME/~}" "$(du -sh "$c" 2>/dev/null | cut -f1)"
  done
  exit 0
fi

before="$(df -k . | awk 'NR==2 {print $4}')"
for p in "${PATHS[@]}"; do
  rm -rf "$p"
done
after="$(df -k . | awk 'NR==2 {print $4}')"

freed=$(( (after - before) / 1024 ))
[ "$freed" -ge 0 ] || freed=0   # 清理期间别的进程在写盘, 差值可能为负
echo "==> 已回收 ${freed} MB (可用空间 $(( after / 1024 / 1024 )) GB)"
