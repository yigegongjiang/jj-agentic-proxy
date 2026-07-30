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

## [0.3.1] - 2026-07-31

### Added

- 一键安装: `curl -fsSL https://raw.githubusercontent.com/yigegongjiang/jj-agentic-proxy/master/scripts/install.sh | bash`, 自动取最新版并校验完整性
- 每次发版随附 macOS arm64 / x64 二进制与校验和, 无需自行编译

## [0.3.0] - 2026-07-31

### Fixed

- 按 OpenAI 官方写法把 base url 配成 `.../v1` 时全部 404; 现在两家写法都通
- 用 Anthropic 客户端取模型列表拿到的是 OpenAI 形状; 现在按客户端各给官方形状
- 不要流式的 OpenAI Responses 请求会返回一串流; 现在返回官方非流式结果对象

### Added

- 允许跨域: 浏览器内的网页可直接连本代理

### Changed

- 代理自身的报错改用客户端所用协议的官方错误信封

## [0.2.0] - 2026-07-30

### Added

- 新增 `10001` 兼容端口: 业务照常用 api key 方式调用, 代理内部换成订阅渠道
- 支持 OpenAI `/v1/chat/completions`: 选 Claude 或 GPT 模型都走这一个端点, 流式 / 工具调用 / 图片 / 结构化输出均可用
- 该端口的 `/v1/models` 返回两家可用模型合并列表, 客户端可直接选模型
- 可选 `JJ_PROXY_API_KEY`: 设了才校验 api key, 默认接受任意值

## [0.1.0] - 2026-07-30

### Added

- 本机代理: 其他 app 指向 `127.0.0.1:10000` 即可用自有订阅调用 Anthropic Messages 与 OpenAI Codex Responses
- `login` / `logout` / `status`: 浏览器授权, 凭证存本机并可随时查看到期时间
- 凭证到期前自动续期, 上游拒绝时自动重试一次, 无需手工介入
- 流式回答逐块透传, 首字延迟与官方 CLI 一致

[Unreleased]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.3.0
[0.2.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.2.0
[0.1.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.1.0
