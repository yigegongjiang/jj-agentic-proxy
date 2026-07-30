```When Editing
本文档作用: 工程总览 (价值主张 / 使用 / 架构 / 结构); MUST NOT 写发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 首行一行价值主张, MUST NOT 带 LLM 提示
- 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 复用自有订阅 (Claude Pro / ChatGPT Plus), 经 Web OAuth 取 token, 以官方 CLI 身份把 Anthropic / OpenAI Codex 能力暴露在 `127.0.0.1`.

## 使用

```bash
jj-agentic-proxy login anthropic  # 浏览器授权; token 落 ~/.config/jj-agentic-proxy/auth.json (0600)
jj-agentic-proxy login codex      # 同上
jj-agentic-proxy                  # = serve, 默认 127.0.0.1:10000; --host/--port 可改
jj-agentic-proxy status           # 凭证账号 / 套餐 / 到期
jj-agentic-proxy logout all       # anthropic | codex | all
```

其他 app 把 base url 指向 `http://127.0.0.1:10000`; 无需 api key (客户端发了也被忽略).

## 端点

选哪个 model 就走哪家官方协议; 本代理不做格式转换, 请求体与响应体原样透传.

<!-- prettier-ignore -->
| 本机端点 | 上游 | 协议 |
| --- | --- | --- |
| `POST /v1/messages` | `api.anthropic.com/v1/messages` | Anthropic Messages |
| `POST /v1/messages/count_tokens` | `api.anthropic.com` 同路径 | Anthropic Messages |
| `GET /v1/models` | `api.anthropic.com/v1/models` | Anthropic |
| `POST /v1/responses` | `chatgpt.com/backend-api/codex/responses` | OpenAI Responses |
| `POST /v1/responses/compact` | 上游 `/responses/compact` | OpenAI Responses |
| `ANY /backend-api/codex/*` | `chatgpt.com` 同路径 | Codex 原生逃生口 |
| `GET /health` | — | 本机版本 + 登录状态 |

- OpenAI 侧只认 Responses API: `/v1/chat/completions` 需格式转换, 不在本工程职责内
- `/v1/models` 是 Anthropic 的; Codex 可用模型查 `/backend-api/codex/models?client_version=0.146.0`, 额度查 `/backend-api/codex/usage`

## 架构

- 单进程 axum, 只监听 loopback; 无云端中转, 无请求落盘
- 认证: OAuth PKCE (S256); 回调端口被上游 client_id allow-list 写死 (Anthropic 54545 / Codex 1455), 登录时须空闲
- 凭证: `auth.json` 原子写 + 0600; 到期前 300s 主动刷新, 每 provider 单飞锁; 上游 401 时强制续期并重试一次
- 透传: 注入 Bearer 与官方 CLI header; body 仅补齐上游硬要求 (Anthropic 的 Claude Code system 前缀 / Codex 的 `stream`+`store`+`instructions`), 显式传值不被覆盖
- 响应逐块转发不缓冲 -> SSE 首字延迟与官方 CLI 一致; 请求体无大小上限
- 上游按 Codex CLI 版本 gate 新模型: 版本号跟随本机 `~/.codex/version.json` 自动更新, 内置常量只作下限
- env: `JJ_PROXY_CODEX_CLI_VERSION` / `JJ_PROXY_CLAUDE_CLI_VERSION` 覆盖冒充版本, `JJ_PROXY_CONFIG_DIR` 改凭证目录, `RUST_LOG` 调日志

## 结构

```
src/main.rs      CLI (serve / login / logout / status) + 优雅退出
src/proxy.rs     路由 + header 注入 + body 规范化 + 流式回传
src/auth.rs      凭证内存态 + 到期预判 + 单飞刷新
src/oauth.rs     PKCE + 本机回调服务 + 两家 token 换取/刷新
src/provider.rs  两家上游常量 (client_id / endpoint / CLI 冒充参数)
src/store.rs     auth.json 读写 (原子 + 0600)
```
