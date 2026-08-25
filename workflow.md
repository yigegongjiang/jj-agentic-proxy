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
- `scripts/install-local.sh`: 预部署总入口; 调 `app/package.sh` -> 建 `~/.local/bin/jj-agentic-proxy` symlink 指向包体内 CLI
- `app/package.sh`: CLI + viewer 本机构建 -> 组装 bundle -> 签名 -> 装 `/Applications` (被上者调用, 也可单跑)

# 调试

- CLI 往返记录: `jj-agentic-proxy logs -n 20` 看摘要; 原始行 `jq` 直接读 `~/.config/jj-agentic-proxy/log/<日期>.jsonl`
- app 快编: `cd app && swift build` -> `./.build/debug/jj-agentic-proxy`
- app 界面自检 (不需录屏授权): `./.build/debug/jj-agentic-proxy --snapshot /tmp/app.png` -> 离屏渲染主窗口, 直接看 PNG
- 改完代理行为后先 `cargo build --release` + `./target/release/jj-agentic-proxy start` 再打真实请求验证: daemon 跟着这份二进制跑, 不碰 `/Applications`; 与已装版共用 pid 文件 + 端口, start 自带 restart -> 不会双实例

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` + `cargo build --release` + `swift build -c release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`./scripts/install-local.sh` (一条命令装 app + 终端入口)
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

一条命令装完 app (`/Applications`, CLI 在包体内) + 终端入口 (`~/.local/bin`):

```bash
./scripts/install-local.sh
```

- app: `cargo build --release` + `swift build -c release` -> 组装 bundle (viewer + `Contents/MacOS/jj-agentic-proxy-cli`) -> ad-hoc 签名 (DR 钉 identifier-only, CLI 由 codesign 按 nested code 自动密封) -> `pkill` 旧 viewer -> `ditto` 到 `/Applications`
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

push tag 即交付完成。tag 只做版本标记, 无 CI 触发, 无产物上传。

> tag 与 `Cargo.toml` version 不一致: 修版本 -> 删 tag (`git tag -d` + `git push origin :vX.Y.Z`) -> 重打重推。
> tag 已推送后 MUST NOT 重推同名 tag。
