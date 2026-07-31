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

## [0.5.5] - 2026-07-31

### Removed

- 取消一键安装脚本与预编译二进制下载: 改为自行构建安装, 历史版本的下载页也已下线

## [0.5.4] - 2026-07-31

### Changed

- `models` 输出改为排序显示: 同系列模型聚在一起, 版本号按数值递增 (不再是上游返回的乱序)

## [0.5.3] - 2026-07-31

### Added

- 新命令 `models`: 列出两家订阅当前可用的 model 与对应端口, 不需要先启动服务

## [0.5.2] - 2026-07-31

### Fixed

- 客户端 system 提示词里带有 Claude Code 那句开场白时整条请求被拒 (报成 429 限流, 极易误判): 现在无论客户端怎么写 system, 10011 都会补足上游要求的开场白首块

## [0.5.1] - 2026-07-31

### Changed

- `start` 自带重启: 已在运行时先停旧进程再起新的, 不再直接返回「已在运行」
- 升级 / 换版本后只需一条 `start`, 无需先手动 `stop`

### Fixed

- 客户端带 `temperature` / `top_p` / `top_k` 时新模型 (Sonnet 5 / Opus 5 等) 整条请求 400 `` `temperature` is deprecated for this model ``: 三条 Claude 路径统一丢弃这些参数

## [0.5.0] - 2026-07-31

### Added

- 新端口 `127.0.0.1:10012`: 同一份 Claude 订阅, 直连 Anthropic 官方 OpenAI 兼容接口 (OpenAI SDK / 客户端可直接指过来)
- 10012 只提供 `/v1/chat/completions` 与模型列表; 需要原生 Anthropic 协议仍走 10011

### Changed

- `status` / 启动输出改列三个端口及其协议面

## [0.4.6] - 2026-07-31

### Added

- `start` / `stop`: 后台常驻启停, 关掉终端不影响运行; 重启 = `stop` + `start`
- `status` 增加运行状态 (运行中 + pid / 未运行) 与日志路径

### Changed

- 无参数运行 = `start` (后台常驻), 不再占用终端; 重复 start 不会起第二个实例
- 日志落 `~/.config/jj-agentic-proxy/daemon.log`, 单文件 8MB 满则轮转一份 (占用不随运行时长增长)

### Removed

- 去掉 `help` 子命令: 用 `-h`

## [0.4.5] - 2026-07-31

### Removed

- 去掉 `JJ_PROXY_API_KEY`: 客户端填的 api key 一律不校验 (只监听本机, 使用者即本人), 填任意非空值即可
- 至此无任何自定义配置项, 开箱即用

## [0.4.4] - 2026-07-31

### Fixed

- 10011 的 `/v1/chat/completions` 带 `temperature` / `top_p` / `top_k` 时不再报 400 (上游新模型已一律拒收): 这些参数改为不转发
- 10011 的 `reasoning_effort` 不再报 400: 改按上游现行的思考档位下发, 旧写法已被新模型移除

## [0.4.3] - 2026-07-31

### Removed

- 去掉 `JJ_PROXY_CODEX_CLI_VERSION`: 上报版本号一律跟随本机 codex CLI 自动更新, 无需手工覆盖
- 可配置项收敛为 `JJ_PROXY_API_KEY` + `RUST_LOG` 两个

## [0.4.2] - 2026-07-31

### Removed

- 去掉两个无实际用途的环境变量: `JJ_PROXY_CLAUDE_CLI_VERSION` (上游不校验该版本) 与 `JJ_PROXY_CONFIG_DIR` (凭证目录改用 `XDG_CONFIG_HOME`)

## [0.4.1] - 2026-07-31

### Fixed

- Claude Code / Claude Agent SDK 指向 10011 时全部报 400 (缓存块顺序冲突); 现在可正常使用
- 按官方写法调 codex 的 `store` / token 上限 / `metadata` 等参数不再被拒绝
- codex 上游的报错改用 OpenAI 官方错误信封, 官方 SDK 能正常读出错误原因
- 10011 的 `/v1/chat/completions` 补齐 `reasoning_effort` 与 `response_format`, 与 10010 行为一致
- 10011 模型列表的所属厂商改用官方大小写 `Anthropic`: 修复按厂商名过滤的客户端拉取后得到空列表

## [0.4.0] - 2026-07-31

### Changed

- 端口改为一个 provider 一个: codex `10010`、claude-code `10011`, 写死不可改; 原 `10000` / `10001` 全部废弃
- 请求走哪家订阅只看端口, 不再看 model 名; 路径走错端口时的报错会直接给出正确端口
- `/v1/models` 只列本端口 provider 的模型; `/health` 改为汇报本端口的 provider 与可用路径

### Removed

- `serve` 的 `--host` / `--port` / `--compat-port` 参数 (端口固定, 无需配置)

### Fixed

- 带 `max_tokens` 调 codex 不再被上游拒绝 (ChatGPT 订阅后端不接受 token 上限)

## [0.3.2] - 2026-07-31

### Fixed

- 上游流提前断开不再伪装成正常结束; 非流式返回明确错误, 流式不再发出假的 `stop`
- 上游连接连续 300s 无数据时结束等待, 压缩响应保留正确编码
- 并发请求不再重复刷新同一 token; 运行中 login/logout 或并发写凭证立即安全生效
- `Authorization` 的 Bearer scheme 改为大小写不敏感
- 安装时校验和缺失/下载失败立即中止; 二进制仍原子替换

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

[Unreleased]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.5.5...HEAD
[0.5.5]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.5.4...v0.5.5
[0.4.5]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yigegongjiang/jj-agentic-proxy/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.3.0
[0.2.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.2.0
[0.1.0]: https://github.com/yigegongjiang/jj-agentic-proxy/releases/tag/v0.1.0
