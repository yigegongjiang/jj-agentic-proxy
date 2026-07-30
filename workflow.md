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
- `scripts/install-local.sh`: 本机构建 + 装入 `~/.local/bin` (预部署用)
- `scripts/install.sh`: 面向使用者, 从 GitHub Releases 下载二进制 (AI 不执行)

# 发布

代码变更完成后立即执行（= 需求交付的最后环节）。交付 = 预部署 + push。

## TL;DR

依序执行：

1. 验证：`cargo fmt --check` + `cargo clippy -- -D warnings` + `cargo test` + `cargo build --release`
2. 写版本：`Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` 同步 (与 tag 一致)
3. 预部署：`./scripts/install-local.sh` (装入 `~/.local/bin`)
4. 发布：commit + annotated tag (`-a -m`) + push branch + tag
5. 分发：push tag 触发 GHA 出 Release 二进制, 确认绿灯

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
./scripts/install-local.sh            # 默认落 ~/.local/bin
INSTALL_DIR=/some/dir ./scripts/install-local.sh   # 改目标目录
```

- 内容: `cargo build --release` -> 临时文件 -> `mv` 原子替换 `~/.local/bin/jj-agentic-proxy`
- 只覆盖同名二进制, 不影响目录内其他 `jj-*` CLI

> MUST NOT 改回 `cp` 原地覆盖: macOS 会因代码签名缓存失效直接 `Killed: 9`。
> 正在运行的旧进程需自行停止后再验证新版本。

验证: 脚本输出的 `--version` 与本次 tag 一致。

## 4. 发布

```bash
git add -A
git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin master
git push origin vX.Y.Z
```

## 5. 分发 (GHA)

push tag 自动触发 `.github/workflows/release.yml`: 校验 tag == `Cargo.toml` version -> 交叉编译 `x86_64/aarch64-apple-darwin` -> `shasum -a 256` -> 建 GitHub Release (二进制 + `checksums.txt`)。

```bash
gh run watch                    # 跟到结束
gh release view vX.Y.Z          # 确认 jj-agentic-proxy-darwin-{x64,arm64} + checksums.txt 齐全
```

> 失败多因 tag 与 `Cargo.toml` version 不一致: 修版本 -> 删 tag (`git tag -d` + `git push origin :vX.Y.Z`) -> 重打重推。
> 版本已被使用者装走后 MUST NOT 重推同名 tag。

使用者安装入口 (AI 不执行):

```bash
curl -fsSL https://raw.githubusercontent.com/yigegongjiang/jj-agentic-proxy/master/scripts/install.sh | bash
```
