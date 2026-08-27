```When Editing
本文档作用: 工程工作流程 (可用工具 / 发布); MUST NOT 写工程说明 (→ README.md / ARCHITECTURE.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 所有段落均为条件段, 根据工程实际决定保留或删除; 存在即为明确流程, MUST NOT 附加强度标记
- 发布内按顺序编号步骤; 顶部 TL;DR ≤ 5 行; 删除子段后重编号保持连续
- 风险点 / 不可逆操作用 `>` 引用块; 高危操作 MUST 标禁用条件
- CI 只承担发布: 写触发条件 + 验收点即可, 打包细节属脚本, MUST NOT 在此复述
```

# 可用工具

- `gh`: 已登录
- `cargo`: 本机 toolchain; 未登录 crates.io -> 不发布 crate
- `swift`: 本机 Swift 6.2+ / Xcode 26+ (查看器 app 用 `defaultIsolation`)
- `scripts/install-local.sh`: 本机预部署总入口 = `app/package.sh` + 装 `/Applications` + 链接 `~/.local/bin` + `--version` 自检; 只给开发用 (用户侧安装 = 拖 dmg + 开 app 点「好」, 不跑脚本)
- `app/package.sh [--arch arm64|x86_64]`: CLI + viewer 单架构构建 -> 组装 bundle -> ad-hoc 签名 -> `app/build/jj-agentic-proxy.app`; 只构建, 不装机
- `scripts/make-dist.sh [--expect-version X.Y.Z]`: 分发打包 -> `dist/` (arm64 / x86_64 各一个 dmg, 外加 SHA256SUMS + RELEASE_NOTES.md); CI 跑的就是它, 本机原样可复现
- `scripts/clean.sh [--all] [--dry-run]`: 清本机编译产物; 默认只清 debug 层 (`target/debug` + `app/.build`), 留 cargo release 缓存

# 调试

- CLI 往返记录: `jj-agentic-proxy logs -n 20` 看摘要; 原始行 `jq` 直接读 `~/.config/jj-agentic-proxy/log/<日期>.jsonl`
- app 快编: `cd app && swift build` -> `./.build/debug/jj-agentic-proxy`
- app 界面自检 (不需录屏授权): `./.build/debug/jj-agentic-proxy --snapshot /tmp/app.png` -> 离屏渲染主窗口, 直接看 PNG
- 磁盘吃紧时 `./scripts/clean.sh` (先 `--dry-run` 看体积)
- 改完代理行为后先 `cargo build --release` + `./target/release/jj-agentic-proxy start` 再打真实请求验证: daemon 跟着这份二进制跑, 不碰 `/Applications`; 与已装版共用 pid 文件 + 端口, start 自带 restart -> 不会双实例

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` + `cargo build --release` + `swift build -c release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`./scripts/install-local.sh` (一条命令装 app + 终端入口)
4. 发布：commit + annotated tag (`-a -m`) + push branch + tag
5. 验收 + 清理：GHA `release` 转绿 + Release 资产齐全 -> `./scripts/clean.sh`

## 1. 验证

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
( cd app && swift build -c release )   # app 编译通过 = 交付闸
```

## 2. 写版本

- 版本号: 默认递增 PATCH (第三位); 超大功能更新/调整 → MINOR; 禁止 → MAJOR（除非人类主动要求）.
- 同步编辑 (与 tag 一致):
  - `Cargo.toml` -> `[package] version`
  - `Cargo.lock` -> 改完 `Cargo.toml` 跑 `cargo build` 自动同步, 一并提交
  - `CHANGELOG.md` (Unreleased 段落转为正式版本条目)
- app 版本无需单独维护: `app/package.sh` 从 `Cargo.toml` 读取并注入 `Info.plist`

## 3. 预部署

一条命令装完 app (`/Applications`, CLI 在包体内) + 终端入口 (`~/.local/bin`):

```bash
./scripts/install-local.sh
```

- app: `cargo build --release` + `swift build -c release` -> 组装 bundle (viewer + `Contents/MacOS/jj-agentic-proxy-cli`) -> 由内向外 ad-hoc 签名 (先 CLI 后 bundle, DR 钉 identifier-only) -> `pkill` 旧 viewer -> `ditto` 到 `/Applications`
- 终端入口: `ln -sfn` 把 `~/.local/bin/jj-agentic-proxy` 指到包体内那份 -> `--version` 自检
- 只覆盖同名 app / 同名入口, 不影响 `~/.local/bin` 里其他 `jj-*` CLI
- 验证: 输出的版本与本次 tag 一致

> MUST NOT 在已装 bundle 内原地替换 CLI 后重签: 改密封资源会连带重写 viewer 的嵌入签名, 代码签名缓存失效 -> `Killed: 9`。整包重建 (`package.sh`) 是唯一升级路径。
> `pkill` MUST 保持 `-f -x` 精确匹配 viewer 完整路径: 模糊匹配会把包体内 CLI 跑的 daemon 一起杀掉。
> 旧进程仍在跑时: 装完执行 `jj-agentic-proxy start` 即切到新版本 (start 自带 restart)。

## 4. 发布

```bash
git add -A
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin master
git push origin vX.Y.Z
```

push tag 即触发 GHA `release`: macOS runner 上跑 `scripts/make-dist.sh` (两个架构各出一套) -> 建 Release 并传产物。

> tag 与 `Cargo.toml` version 不一致: CI 在打包前就失败 (`--expect-version`)。修版本 -> 删 tag (`git tag -d` + `git push origin :vX.Y.Z`) -> 重打重推。
> tag 已推送且 CI 已出 Release 后 MUST NOT 重推同名 tag。

## 5. 验收

```bash
gh run watch "$(gh run list --workflow release --limit 1 --json databaseId --jq '.[0].databaseId')"
gh release view vX.Y.Z   # 资产: arm64/x86_64 各一个 dmg, 外加 SHA256SUMS
```

- CI 失败即交付未完成: 修因 -> 删 tag 重推 (Release 尚未建出时无残留)
- 无 Apple 开发者签名 (ad-hoc + 无 notary ticket): 用户从 dmg 拖进来首次打开必被 Gatekeeper 拦, 提示「未打开 / Apple 无法验证」-> 系统设置 → 隐私与安全性 → 仍要打开 (README 安装段已说明)。想去掉这一步只能买 Developer ID 做公证
- 终端命令入口由 app 自己建 (`CLIInstall.swift`: 打开时弹窗 -> `~/.local/bin` symlink + 摘 quarantine): 发布资产内 MUST NOT 再夹带安装脚本
- MUST NOT 改回 universal 包: 包体内混进另一架构的 slice 会让 macOS 26.4+ 弹「Support Ending for Intel-based Apps」
- 干跑不发版: Actions 页手动触发 `release` (workflow_dispatch) -> 只出 workflow artifact

## 6. 清理

```bash
./scripts/clean.sh               # 清 target/debug + app/.build, 留 cargo release 缓存
./scripts/clean.sh --dry-run     # 只报体积, 含仓库外的 cargo / swiftpm 缓存
./scripts/clean.sh --all         # 连 release + 交叉编译 target + 包体 + dmg 一起清
```

必须排在步骤 3 之后: 提前清会让预部署付一次冷 release 构建。

> 默认档删掉整个 `app/.build`, Swift 侧 (含 release) 一并冷编; cargo release 缓存留着。
> `--all` 之后下一次 `install-local.sh` / `make-dist.sh` 全冷启, 双架构 dmg 尤其久。
> 脚本认不出仓库根 (`Cargo.toml` 里没有 `name = "jj-agentic-proxy"`) 直接 exit 2, 不接受路径参数 —— 里面全是 `rm -rf`。
