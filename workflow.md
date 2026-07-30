```When Editing
本文档作用: 工程工作流程 (可用工具 / 发布); MUST NOT 写工程说明 (→ README.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 所有段落均为条件段, 根据工程实际决定保留或删除; 存在即为明确流程, MUST NOT 附加强度标记
- 发布内按顺序编号步骤; 顶部 TL;DR ≤ 5 行; 删除子段后重编号保持连续
- 风险点 / 不可逆操作用 `>` 引用块; 高危操作 MUST 标禁用条件
- GHA 结果不监听: MUST NOT 写 `gh run watch` / `gh run list` / `gh release view` 等等待·轮询·验证 CI 的步骤
```

# 可用工具

- `gh`: 已登录
- `cargo`: 本机 toolchain; 未登录 crates.io -> 不发布 crate
- `scripts/install-local.sh`: 本机构建 + 装入 `~/.local/bin` (预部署用)
- `scripts/install.sh`: 面向使用者, 从 GitHub Releases 下载二进制 (AI 不执行)

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` + `cargo build --release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`./scripts/install-local.sh` (装入 `~/.local/bin`)
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

本机构建 + 安装二进制到 `~/.local/bin`, 末尾自动跑 `--version` 自检:

```bash
./scripts/install-local.sh
```

- 内容: `cargo build --release` -> 临时文件 -> `mv` 原子替换 `~/.local/bin/jj-agentic-proxy`
- 只覆盖同名二进制, 不影响目录内其他 `jj-*` CLI
- 验证: 脚本输出的 `--version` 与本次 tag 一致

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

push tag 即交付完成。tag 自动触发 `.github/workflows/release.yml` (校验 tag == `Cargo.toml` version -> 双架构 darwin 构建 -> `checksums.txt` -> GitHub Release), 结果不监听不等待。

> tag 与 `Cargo.toml` version 不一致会让 GHA 失败: 修版本 -> 删 tag (`git tag -d` + `git push origin :vX.Y.Z`) -> 重打重推。
> 版本已被使用者装走后 MUST NOT 重推同名 tag。
