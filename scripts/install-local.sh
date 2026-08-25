#!/usr/bin/env bash
# 本机预部署总入口: 本机架构构建 -> 装 /Applications + 链接终端命令。
# Usage: ./scripts/install-local.sh   (在任意目录跑, 脚本自己 cd 到仓库根)
# 分发包 (universal + tar.gz/dmg) 走 scripts/make-dist.sh, 不走这里。

set -euo pipefail

for arg in "$@"; do
  case "$arg" in
    -h|--help) echo "usage: $0"; exit 0 ;;
    *) echo "unknown option: $arg (usage: $0)" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

./app/package.sh
./scripts/install.sh "app/build/jj-agentic-proxy.app"
