```When Editing
本文档作用: 面向使用者的发版记录; 只写用户感受得到的变化, MUST NOT 写技术细节
遵循 AGENTS.md 文档编写规范
- 写: 新功能 / 行为修复 / 体验 / 安全 / 命令迁移
- MUST NOT 写: 文件路径 / 函数名 / 组件名 / 依赖包名 / 重构细节
- 单条 ≤ 2 行, 单版本 ≤ 5 条; 段落: Added / Changed / Fixed / Removed / Security
- 无用户可感知变化 → 占位: `跟随版本同步发布`
- 本工程只维护本文件, MUST NOT 新建 CHANGELOG.dev.md
```

# Changelog

[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) + [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-07-30

### Added

- 本机代理: 其他 app 指向 `127.0.0.1:10000` 即可用自有订阅调用 Anthropic Messages 与 OpenAI Codex Responses
- `login` / `logout` / `status`: 浏览器授权, 凭证存本机并可随时查看到期时间
- 凭证到期前自动续期, 上游拒绝时自动重试一次, 无需手工介入
- 流式回答逐块透传, 首字延迟与官方 CLI 一致

[Unreleased]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.1.0
