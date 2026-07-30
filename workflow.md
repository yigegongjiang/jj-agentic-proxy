```When Editing
本文档作用: 工程工作流程 (可用工具 / 调试 / 发布); MUST NOT 写工程说明 (→ README.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 所有段落均为条件段, 根据工程实际决定保留或删除; 存在即为明确流程, MUST NOT 附加强度标记
- 发布内按顺序编号步骤; 顶部 TL;DR ≤ 5 行; 删除子段后重编号保持连续
- 风险点 / 不可逆操作用 `>` 引用块; 高危操作 MUST 标禁用条件
```

# 可用工具

- `gh`: 已登录
- `cargo`: 本机 toolchain (未登录 crates.io -> 不发布 crate)

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy -- -D warnings` + `cargo test` + `cargo build --release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`cargo install --path . --locked --force --root ~/.local` (装入 `~/.local/bin`)
4. 发布：commit + annotated tag (`-a -m`) + push branch + tag

## 1. 验证

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## 2. 写版本

- 版本号: 默认递增 PATCH (第三位); 超大功能更新/调整 → MINOR; 禁止 → MAJOR（除非人类主动要求）.
- 同步编辑 (与 tag 一致):
  - `Cargo.toml` -> `[package] version`
  - `Cargo.lock` -> 改完 `Cargo.toml` 跑 `cargo build` 自动同步, 一并提交
  - `CHANGELOG.md` (Unreleased 段落转为正式版本条目)

## 3. 预部署

本机安装可执行二进制到 `~/.local/bin`:

```bash
cargo install --path . --locked --force --root ~/.local
```

> `--root ~/.local` 使产物落 `~/.local/bin/jj-agentic-proxy`, MUST NOT 用默认 `~/.cargo/bin`。
> `--force` 只覆盖同名二进制, 不影响目录内其他 `jj-*` CLI; 正在运行的旧进程需自行停止后再验证新版本。

验证: `jj-agentic-proxy --version` 输出与本次 tag 一致。

## 4. 发布

```bash
git add -A
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin master
git push origin vX.Y.Z
```
