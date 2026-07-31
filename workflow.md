```When Editing
本文档作用: 工程工作流程 (可用工具 / 发布); MUST NOT 写工程说明 (→ README.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 所有段落均为条件段, 根据工程实际决定保留或删除; 存在即为明确流程, MUST NOT 附加强度标记
- 发布内按顺序编号步骤; 顶部 TL;DR ≤ 5 行; 删除子段后重编号保持连续
- 风险点 / 不可逆操作用 `>` 引用块; 高危操作 MUST 标禁用条件
- 无 CI / 无二进制分发: MUST NOT 写 GitHub Actions / Releases 相关步骤
```

# 可用工具

- `gh`: 已登录
- `cargo`: 本机 toolchain; 未登录 crates.io -> 不发布 crate
- `swift`: 本机 Swift 6.2+ / Xcode 26+ (查看器 app 用 `defaultIsolation`)
- `scripts/install-local.sh`: 预部署总入口; CLI 装 `~/.local/bin` -> 续跑 `app/package.sh`; `--cli-only` 只装 CLI
- `app/package.sh`: app 本机构建 + 装入 `/Applications` (被上者调用, 也可单跑)

# 调试

- CLI 往返记录: `jj-agentic-proxy logs -n 20` 看摘要; 原始行 `jq` 直接读 `~/.config/jj-agentic-proxy/log/<日期>.jsonl`
- app 快编: `cd app && swift build` -> `./.build/debug/jj-agentic-proxy`
- app 界面自检 (不需录屏授权): `./.build/debug/jj-agentic-proxy --snapshot /tmp/app.png` -> 离屏渲染主窗口, 直接看 PNG
- 改完代理行为后先 `./scripts/install-local.sh --cli-only` + `jj-agentic-proxy start` 再打真实请求验证 (start 自带 restart)

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` + `cargo build --release` + `swift build -c release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`./scripts/install-local.sh` (一条命令装 CLI + app)
4. 发布：commit + annotated tag (`-a -m`) + push branch + tag

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

一条命令装完 CLI (`~/.local/bin`) + app (`/Applications`):

```bash
./scripts/install-local.sh
```

- CLI: `cargo build --release` -> 临时文件 -> `mv` 原子替换 `~/.local/bin/jj-agentic-proxy` -> `--version` 自检
- 只覆盖同名二进制, 不影响目录内其他 `jj-*` CLI
- app: 续跑 `app/package.sh` -> Release 构建 -> 组装 bundle -> ad-hoc 签名 (DR 钉 identifier-only) -> `pkill` 旧实例 -> `ditto` 到 `/Applications`
- 验证: 输出的两处版本 (CLI / app) 与本次 tag 一致

> MUST NOT 改回 `cp` 原地覆盖: macOS 会因代码签名缓存失效直接 `Killed: 9`。
> 旧进程仍在跑时: 装完执行 `jj-agentic-proxy start` 即切到新版本 (start 自带 restart)。

## 4. 发布

```bash
git add -A
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin master
git push origin vX.Y.Z
```

push tag 即交付完成。tag 只做版本标记, 无 CI 触发, 无产物上传。

> tag 与 `Cargo.toml` version 不一致: 修版本 -> 删 tag (`git tag -d` + `git push origin :vX.Y.Z`) -> 重打重推。
> tag 已推送后 MUST NOT 重推同名 tag。
