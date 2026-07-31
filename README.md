```When Editing
本文档作用: 工程总览 (价值主张 / 使用 / 架构 / 结构); MUST NOT 写发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 首行一行价值主张, MUST NOT 带 LLM 提示
- 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 复用自有订阅 (Claude Pro / ChatGPT Plus), 经 Web OAuth 取 token, 以官方 CLI 身份把 Codex 能力暴露在 `127.0.0.1:10010`、Claude Code 能力暴露在 `127.0.0.1:10011` (原生 Anthropic) 与 `127.0.0.1:10012` (Anthropic 官方 OpenAI 兼容层).

## 安装

- macOS only; 无预编译分发, 本机构建后装入 `~/.local/bin/jj-agentic-proxy` (见 [workflow.md](./workflow.md))
- `~/.local/bin` 需在 `PATH`: `export PATH="$HOME/.local/bin:$PATH"`

## 使用

```bash
jj-agentic-proxy login anthropic  # 浏览器授权; token 落 ~/.config/jj-agentic-proxy/auth.json (0600)
jj-agentic-proxy login codex      # 同上
jj-agentic-proxy                  # = start, 后台常驻 (10010 + 10011 + 10012); 已在运行则先停再起
jj-agentic-proxy stop             # 停止
jj-agentic-proxy status           # 运行中/未运行 + 凭证账号 / 套餐 / 到期
jj-agentic-proxy models           # 两家订阅当前可用 model + 各自端口
jj-agentic-proxy logout all       # anthropic | codex | all
```

- 后台常驻: 脱离终端 (关掉 shell 不影响), 日志落 `~/.config/jj-agentic-proxy/daemon.log`
- `models` 直接问上游, 与后台是否在跑无关; 内容同 `GET /v1/models`, 打印时按名字自然序重排 (端点仍保上游原序)
- `start` = restart: 已在运行则先 stop 再起新进程 -> 升级 / 换版本后一条命令即生效, 永不出现两个实例
- 被外部程序占了固定端口时 start 直接失败并回显日志尾部

## 端口 (一个端口一个协议面)

<!-- prettier-ignore -->
| 端口 | 协议面 | 订阅 | 上游 | 协议 |
| --- | --- | --- | --- | --- |
| 10010 | codex | ChatGPT | `chatgpt.com/backend-api/codex` | OpenAI Responses + Chat Completions (本地转换) |
| 10011 | claude-code | Claude | `api.anthropic.com/v1/messages` | Anthropic Messages + Chat Completions (本地转换) |
| 10012 | claude-openai | Claude | `api.anthropic.com/v1/chat/completions` | OpenAI Chat Completions (上游官方兼容层) |

- 端口写死在二进制里, 无任何 host / port 参数: base url 一次写死, 换机器不用改
- 10011 与 10012 同一份 Claude 订阅凭证, 只是协议转换发生在本地还是上游
- 请求打到哪个端口就走哪家订阅, 与 model 名无关; 路径走错端口时 404 直接给出正确端口
- base url 两家官方写法都通: `http://127.0.0.1:10011` (Anthropic SDK 约定) 与 `http://127.0.0.1:10011/v1` (OpenAI SDK 约定)
- api key 填任意非空值即可 (官方 SDK 会本地校验非空); 代理不校验它, 上游身份一律用本机 OAuth 凭证
- 全放开 CORS: 浏览器页面可直连, 预检由代理直接应答

## 端点

<!-- prettier-ignore -->
| 端点 | 端口 | 上游 | 协议 |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | 全部 | 按端口定 | OpenAI Chat Completions |
| `GET /v1/models`, `/v1/models/{id}` | 全部 | 按端口定 | 见下 |
| `POST /v1/messages` | 10011 | `api.anthropic.com/v1/messages` | Anthropic Messages |
| `POST /v1/messages/count_tokens` | 10011 | `api.anthropic.com` 同路径 | Anthropic Messages |
| `POST /v1/responses` | 10010 | `chatgpt.com/backend-api/codex/responses` | OpenAI Responses |
| `POST /v1/responses/compact` | 10010 | 上游 `/responses/compact` | OpenAI Responses |
| `ANY /backend-api/codex/*` | 10010 | `chatgpt.com` 同路径 | Codex 原生逃生口 |
| `GET /health` | 全部 | — | 本端口协议面 + 登录状态 + 可用路径 |

- Chat Completions 的上游由端口决定, `model` 只取模型名 (允许 `anthropic/`、`openai/` 前缀)
- 10010 / 10011 本地转换覆盖: 流式 / 非流式、tools + 工具结果回传、图片 (url 与 data URI)、`response_format`、`reasoning_effort`; 思考内容出 `reasoning_content`
- 采样参数 (`temperature` / `top_p` / `top_k`) 在所有 Anthropic 面 (10011 原生 + 10011/10012 Chat Completions) 一律丢弃: 上游新模型按「键是否存在」硬拒 (400 `` `temperature` is deprecated ``), 与取值无关, 且受限名单随新模型扩张 -> 不做模型名判断
- 10011 的 Chat Completions 把 `reasoning_effort` 映射成上游现行思考档位
- 10012 只有 Chat Completions 与模型列表 (原生 Messages 走 10011), 字段支持度以[上游兼容层](https://platform.claude.com/docs/en/api/openai-sdk)为准: 无 `reasoning_content`, `response_format` / `reasoning_effort` / `seed` 等被上游静默忽略
- `/v1/models` 形状: 10011 带 `x-api-key` / `anthropic-version` -> Anthropic 官方原样; 其余 (含 10010 / 10012) -> OpenAI 列表, 只含本端口订阅的模型
- 错误一律按方言裹官方信封 (`{"type":"error",...}` / `{"error":{...}}`); 上游已给官方形状则原样透传, 保留 `request-id` 等头
- 额度查 `10010/backend-api/codex/usage`

## 架构

- 单进程 axum, 三端口共用凭证与连接池; 建连超时 20s / 读取空闲超时 300s; 只监听 loopback, 无云端中转, 无请求落盘
- 启动时先 bind 三个端口再对外服务: 端口被占用立刻整体失败, 不留「只有一部分能用」的中间态
- 单实例: `daemon.pid` 独占文件锁, 锁由内核在进程退出时释放 -> 判活不受 pid 复用 / 残留 pid 文件影响
- `start` 拉起后台进程后等它写下就绪标记才报成功 (不靠「端口通」, 避免把别人占的端口认成自己); `stop` = SIGTERM -> 等锁释放 -> 5s 未退则 SIGKILL
- 后台日志单文件 8MB 上限, 满则轮转一份 -> 磁盘占用恒定, 不随运行时长增长
- 认证: OAuth PKCE (S256); 回调端口被上游 client_id allow-list 写死 (Anthropic 54545 / Codex 1455), 登录时须空闲
- 凭证: `auth.json` 进程间串行 + 原子写 + 0600; login/logout 热更新; 到期前 300s 主动刷新, 每 provider 单飞锁; 上游 401 时强制续期并重试一次
- 透传路径: 注入 Bearer 与官方 CLI header, body 只做上游硬要求的最小改写
  - OAuth 凭证的 system 闸门 (实测): 上游只认 system **首块**且要求与 Claude Code 前缀**逐字节全等**; 前缀与正文同块、多一个尾随换行、前缀排在后面的块里, 一律被拒 —— 且报成 429 `rate_limit_error`, 极易误判为限流
  - Anthropic 原生: 首块不合规就在最前面补一块纯前缀 (不带 `cache_control`, 不占客户端的缓存断点、不打乱 ttl 顺序); 首块之后不受限制 -> 客户端 system 原样保留
  - Anthropic 兼容层: 上游会把所有 system / developer 消息拼成**单块** system, 与全等要求天然冲突 -> 客户端 system 文本挪进对话首条 user 消息 (`<system-instructions>` 包裹), system 通道只留前缀
  - Codex: 补 `stream`+`instructions`, 强制 `store:false`, 丢弃上游不认的纯标注参数 (`metadata` / `user` / `safety_identifier` / token 上限)
  - 有语义的参数 (`temperature` / `previous_response_id` / `background` / ...) 不静默丢弃, 由上游报错并归一成官方信封
- Chat Completions: 双向转换; 上游一律 SSE, 客户端要非流式时本层聚合 -> 只维护一条解析路径
- CLI 渠道与官方 api key 渠道的差异由代理抹平: 上游硬拒 `stream:false` 与字符串 `input`, 代理补齐后再把 SSE 聚合成官方非流式对象
- 响应逐块转发不缓冲 -> SSE 首字延迟与官方 CLI 一致; 请求体无大小上限
- 上游按 Codex CLI 版本 gate 新模型: 版本号跟随本机 `~/.codex/version.json` 自动更新, 内置常量只作下限
- 零配置: 无任何自定义 env / 参数 (端口、路径、身份全部内置); 只认标准 `RUST_LOG` 调日志

## 结构

```
src/main.rs               CLI (start / stop / login / logout / status) + 三个固定端口 + 优雅退出
src/daemon.rs             后台常驻: 单实例锁 + 探活 + 启停 + 日志封顶
src/server.rs             端口层: Chat Completions + 模型列表, 其余落透传
src/proxy.rs              透传: 上游由端口定 + header 注入 + body 规范化 + 带凭证请求上游
src/convert/mod.rs        Chat Completions 增量模型 + 流式回传 / 非流式聚合
src/convert/codex.rs      Chat Completions <-> OpenAI Responses
src/convert/anthropic.rs  Chat Completions <-> Anthropic Messages
src/sse.rs                SSE 解码
src/auth.rs               凭证内存态 + 到期预判 + 单飞刷新
src/oauth.rs              PKCE + 本机回调服务 + 两家 token 换取/刷新
src/provider.rs           协议面 <-> 端口 / 订阅映射 + 两家上游常量 (client_id / endpoint / CLI 冒充参数)
src/store.rs              auth.json 读写 (原子 + 0600)
scripts/install-local.sh  本机 release 构建 + 安装 (预部署)
```
