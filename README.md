```When Editing
本文档作用: 工程总览 (价值主张 / 使用 / 架构 / 结构); MUST NOT 写发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 首行一行价值主张, MUST NOT 带 LLM 提示
- 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 复用自有订阅 (Claude Pro / ChatGPT Plus), 经 Web OAuth 取 token, 以官方 CLI 身份把 Anthropic / OpenAI Codex 能力暴露在 `127.0.0.1`.

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/yigegongjiang/jj-agentic-proxy/master/scripts/install.sh | bash
```

- macOS only (arm64 / x64); 取 GitHub Releases 最新版, 校验 SHA256, 落 `~/.local/bin/jj-agentic-proxy`
- `~/.local/bin` 不在 `PATH` 时脚本会提示补 `export PATH="$HOME/.local/bin:$PATH"`

## 使用

```bash
jj-agentic-proxy login anthropic  # 浏览器授权; token 落 ~/.config/jj-agentic-proxy/auth.json (0600)
jj-agentic-proxy login codex      # 同上
jj-agentic-proxy                  # = serve, 默认原生 10000 + 兼容 10001; --host/--port/--compat-port 可改
jj-agentic-proxy status           # 凭证账号 / 套餐 / 到期
jj-agentic-proxy logout all       # anthropic | codex | all
```

## 两个端口

<!-- prettier-ignore -->
| 端口 | 面向 | 请求体 | api key |
| --- | --- | --- | --- |
| 10000 原生 | 已按官方协议编写的客户端 | 原样透传 | 不需要 |
| 10001 兼容 | 按 api key 方式调用的业务 | 按 model 转换 | 任意值 |

- 兼容端口 = 原生端口的超集: 多了 `/v1/chat/completions` 与合并模型列表, 其余路径行为一致 -> 业务 base url 全指 `http://127.0.0.1:10001` 即可
- base url 两家官方写法都通: `http://127.0.0.1:10001` (Anthropic SDK 约定) 与 `http://127.0.0.1:10001/v1` (OpenAI SDK 约定)
- api key 填任意非空值即可 (官方 SDK 会本地校验非空); 设了 `JJ_PROXY_API_KEY` 才真的比对
- 全放开 CORS: 浏览器页面可直连, 预检由代理直接应答
- `--compat-port 0` 关闭兼容端口

## 端点

<!-- prettier-ignore -->
| 端点 | 端口 | 上游 | 协议 |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | 10001 | 按 model 定 | OpenAI Chat Completions |
| `GET /v1/models` | 两者 | 见下 | Anthropic 原样 / OpenAI 合并列表 |
| `POST /v1/messages` | 两者 | `api.anthropic.com/v1/messages` | Anthropic Messages |
| `POST /v1/messages/count_tokens` | 两者 | `api.anthropic.com` 同路径 | Anthropic Messages |
| `POST /v1/responses` | 两者 | `chatgpt.com/backend-api/codex/responses` | OpenAI Responses |
| `POST /v1/responses/compact` | 两者 | 上游 `/responses/compact` | OpenAI Responses |
| `ANY /backend-api/codex/*` | 两者 | `chatgpt.com` 同路径 | Codex 原生逃生口 |
| `GET /health` | 两者 | — | 本机版本 + 登录状态 |

- Chat Completions 的 model 决定上游: `claude*` -> Anthropic Messages, 其余 -> Codex Responses; 允许 `anthropic/`、`openai/` 前缀
- 覆盖: 流式 / 非流式、tools + 工具结果回传、图片 (url 与 data URI)、`response_format`、`reasoning_effort`; 思考内容出 `reasoning_content`
- `/v1/models` 在 10001 按客户端方言给形状: 带 `x-api-key` / `anthropic-version` -> Anthropic 官方原样; 否则 -> OpenAI 列表 (两家合并); 10000 恒为 Anthropic 原样
- 代理自产的错误也按方言裹官方信封 (`{"type":"error",...}` / `{"error":{...}}`)
- 额度查 `/backend-api/codex/usage`

## 架构

- 单进程 axum, 双端口共用一份凭证与连接池; 只监听 loopback, 无云端中转, 无请求落盘
- 认证: OAuth PKCE (S256); 回调端口被上游 client_id allow-list 写死 (Anthropic 54545 / Codex 1455), 登录时须空闲
- 凭证: `auth.json` 原子写 + 0600; 到期前 300s 主动刷新, 每 provider 单飞锁; 上游 401 时强制续期并重试一次
- 原生端口: 注入 Bearer 与官方 CLI header, body 仅补齐上游硬要求 (Anthropic 的 Claude Code system 前缀 / Codex 的 `stream`+`store`+`instructions`), 显式传值不被覆盖
- 兼容端口: Chat Completions 双向转换; 上游一律 SSE, 客户端要非流式时本层聚合 -> 只维护一条解析路径
- CLI 渠道与官方 api key 渠道的差异由代理抹平: 上游硬拒 `stream:false` 与字符串 `input`, 代理补齐后再把 SSE 聚合成官方非流式对象
- 响应逐块转发不缓冲 -> SSE 首字延迟与官方 CLI 一致; 请求体无大小上限
- 上游按 Codex CLI 版本 gate 新模型: 版本号跟随本机 `~/.codex/version.json` 自动更新, 内置常量只作下限
- env: `JJ_PROXY_API_KEY` 开启 key 校验, `JJ_PROXY_CODEX_CLI_VERSION` / `JJ_PROXY_CLAUDE_CLI_VERSION` 覆盖冒充版本, `JJ_PROXY_CONFIG_DIR` 改凭证目录, `RUST_LOG` 调日志

## 结构

```
src/main.rs               CLI (serve / login / logout / status) + 双端口 + 优雅退出
src/proxy.rs              原生透传: 路由 + header 注入 + body 规范化 + 带凭证请求上游
src/compat.rs             兼容端口: api key 校验 + Chat Completions + 合并模型列表
src/convert/mod.rs        Chat Completions 增量模型 + 流式回传 / 非流式聚合
src/convert/codex.rs      Chat Completions <-> OpenAI Responses
src/convert/anthropic.rs  Chat Completions <-> Anthropic Messages
src/sse.rs                SSE 解码
src/auth.rs               凭证内存态 + 到期预判 + 单飞刷新
src/oauth.rs              PKCE + 本机回调服务 + 两家 token 换取/刷新
src/provider.rs           两家上游常量 (client_id / endpoint / CLI 冒充参数)
src/store.rs              auth.json 读写 (原子 + 0600)
scripts/install.sh        使用者一键安装 (GitHub Releases -> ~/.local/bin)
scripts/install-local.sh  本机 release 构建 + 安装 (预部署)
.github/workflows/release.yml  tag 触发: 双架构构建 + checksums + GitHub Release
```
