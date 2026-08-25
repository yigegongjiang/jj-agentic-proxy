```When Editing
本文档作用: 工程总览 (价值主张 / 使用 / 架构 / 结构); MUST NOT 写发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 首行一行价值主张, MUST NOT 带 LLM 提示
- 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 把自有订阅 (Claude Pro / ChatGPT Plus) 转成标准 API key 形式 -> 任意 OpenAI / Anthropic 兼容客户端填个 base url + key 就能用订阅额度跑, 不碰 API 计费.

经 Web OAuth 取 token, 以官方 CLI 身份请求上游, 协议差异在本地改写抹平: Codex `:10010`、Claude Code `:10011` (原生 Anthropic) 与 `:10012` (Anthropic 官方 OpenAI 兼容层); 全部端口监听全部网卡 -> 本机与同局域网主机都能连; 经手的 req/res 全量落本机文本日志, 配套查看器两份 (`:10020` 浏览器 + macOS app) 做绑定查看.

## 安装

- macOS only; 无预编译分发, 本机构建后装入 `~/.local/bin/jj-agentic-proxy` (见 [workflow.md](./workflow.md))
- `~/.local/bin` 需在 `PATH`: `export PATH="$HOME/.local/bin:$PATH"`
- 浏览器查看器无需安装: 随代理进程一起起来, 开 `http://127.0.0.1:10020`
- macOS 查看器 app 装入 `/Applications/jj-agentic-proxy.app` (同一份 [workflow.md](./workflow.md))

## 使用

```bash
jj-agentic-proxy login anthropic  # 浏览器授权; token 落 ~/.config/jj-agentic-proxy/auth.json (0600)
jj-agentic-proxy login codex      # 同上
jj-agentic-proxy                  # = start, 后台常驻 (10010 + 10011 + 10012 + 10020); 已在运行则先停再起
jj-agentic-proxy stop             # 停止
jj-agentic-proxy status           # 运行中/未运行 + 凭证账号 / 套餐 / 到期
jj-agentic-proxy models           # 两家订阅当前可用 model + 各自端口
jj-agentic-proxy logs -n 20       # 最近 20 条往返摘要 + 记录目录
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
| 10020 | web-ui | — | — | 浏览器查看器 (非协议面, 不代理任何请求) |

- 端口写死在二进制里, 无任何 host / port 参数: base url 一次写死, 换机器不用改
- 监听 v4 + v6 全网卡: 本机用 `127.0.0.1` / `localhost`, 局域网其他主机用本机 LAN IP (同端口); `start` / `status` 直接打印该 LAN 入口
- 四个端口一起 bind 且早于就绪标记: 任一被占即整体启动失败, 不留「代理能用但看不见流量」的半残状态
- 无鉴权 -> 能连到这些端口的主机即可用本机订阅、读全部往返记录 (含原样授权头); 仅限可信内网, 端口 MUST NOT 经路由器映射到公网
- macOS 应用防火墙开着时首次启动会弹「是否允许接受传入网络连接」, 选允许; 静默拒绝会表现为局域网连不上而本机正常
- 10011 与 10012 同一份 Claude 订阅凭证, 只是协议转换发生在本地还是上游
- 请求打到哪个端口就走哪家订阅, 与 model 名无关; 路径走错端口时 404 直接给出正确端口
- base url 带不带 `/v1` 后缀都通, 三个协议端口一律如此: `http://<host>:10010` (Anthropic SDK 约定) 与 `http://<host>:10010/v1` (OpenAI SDK 约定, 也是多数客户端 placeholder 的写法) 等价; 客户端重复拼出的 `/v1/v1/...` 一并归一
- api key 用 `start` / `status` 打印的那份固定值 (三面通用, 永不过期, 直接复制): 代理不校验它, 上游身份一律用本机 OAuth 凭证 -> 任意非空串通常也行, 但 codex 面的部分客户端 (pi-ai 等) 会在发请求前把 key 当 JWT 本地解析取 `chatgpt_account_id`, 只有这份固定值过得去
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
| `ANY /backend-api/codex/*` | 10010 | `chatgpt.com` 同路径 | 原样透传 (见[透传口](#codex-透传口)) |
| `GET /health` | 全部 | — | 本端口协议面 + 登录状态 + 可用路径 |

- Chat Completions 的上游由端口决定, `model` 只取模型名 (允许 `anthropic/`、`openai/` 前缀)
- 10010 / 10011 本地转换覆盖: 流式 / 非流式、tools + 工具结果回传、图片 (url 与 data URI)、`response_format`、`reasoning_effort`; 思考内容出 `reasoning_content`
- 采样参数 (`temperature` / `top_p` / `top_k`) 在所有 Anthropic 面 (10011 原生 + 10011/10012 Chat Completions) 一律丢弃: 上游新模型按「键是否存在」硬拒 (400 `` `temperature` is deprecated ``), 与取值无关, 且受限名单随新模型扩张 -> 不做模型名判断
- 10011 的 Chat Completions 把 `reasoning_effort` 映射成上游现行思考档位
- 10012 只有 Chat Completions 与模型列表 (原生 Messages 走 10011), 字段支持度以[上游兼容层](https://platform.claude.com/docs/en/api/openai-sdk)为准: 无 `reasoning_content`, `response_format` / `reasoning_effort` / `seed` 等被上游静默忽略
- `/v1/models` 形状: 10011 带 `x-api-key` / `anthropic-version` -> Anthropic 官方原样; 其余 (含 10010 / 10012) -> OpenAI 列表, 只含本端口订阅的模型
- 错误一律按方言裹官方信封 (`{"type":"error",...}` / `{"error":{...}}`); 上游已给官方形状则原样透传, 保留 `request-id` 等头

## Codex 透传口

`ANY :10010/backend-api/codex/*` -> `chatgpt.com/backend-api/codex/` 同路径. 其余端点都做协议转换, 这个不做: **body 原样不动, 只换上游身份** (本机 OAuth token + Codex CLI header). 用途 = 直接打本地没建模的上游私有端点.

```bash
curl http://127.0.0.1:10010/backend-api/codex/usage  # 订阅额度: plan_type + rate_limit 窗口 + used_percent
```

- 与其余端点一样吃 `/v1` 前缀: base url 按 OpenAI SDK 约定写成 `.../v1` 时拼出的 `/v1/backend-api/codex/*` 同样命中
- 不需要 api key: 与其余端点一样, 代理不校验客户端凭证, 上游身份一律用本机 OAuth
- 已验证的子路径只有 `/usage`; `/responses`、`/models` 走上表正式端点即可 (带本地转换 + 方言归一)
- 上游是 Codex CLI 私有后端, 无公开文档: 路径与响应形状随时可能变, 本口只保证转发, 不保证上游长期可用

## 往返记录

经三端口的每次 req/res 落 `~/.config/jj-agentic-proxy/log/<日期>.jsonl`, 一行一次往返 (JSON Lines, `rg` / `jq` 直接可读), **两条腿都记**: 客户端 <-> 代理, 代理 <-> 上游.

<!-- prettier-ignore -->
| 字段 | 说明 |
| --- | --- |
| `ts` | 记录时刻, 固定 JST (`+09:00`); 文件名取同一日期 |
| `surface` / `method` / `path` | 哪个端口 + 客户端请求行 (`path` 含 query) |
| `status` | 客户端拿到的状态码; `0` = 没等到响应 |
| `stream` / `elapsed_ms` | 客户端是否要流式 + 从收到请求到响应结束的耗时 |
| `req_bytes` / `res_bytes` / `model` | 两侧 body 字节数 + 请求里的 model |
| `incomplete` | 仅异常时出现: `客户端断开` / `上游流中断: ...` |
| `req_headers` / `req` | 客户端发来的 header 与 body 原样 |
| `res_headers` / `res` | 回给客户端的 header 与 body (SSE 存整段原文) |
| `upstream` | 上游那一腿: `method` / `url` / `status` / `req_headers` (注入 CLI 身份后的实际值) / `res_headers` (含 `request-id`、限流头) / `req_body` (仅当本层改写过, 未改写即等同 `req`) |

- 摘要标量全在 `req_headers` 之前 -> 截到该键即得一条摘要, 不必碰 header 与大 body
- header 原样记录, 含 `authorization`: 本机自用, 抹掉就查不了「上游为什么拒」; 记录文件 0600 (同 `auth.json`)
- 按天一个文件, 永不自动清理 (无天数 / 体积上限, 清理由人类自行决定) -> body 一律全量, 不截断
- 不记 `/health` (本机探活, 无上游往返); 上游响应体不单独记 (透传面与客户端那份相同)
- 流式响应逐块 tee 落盘, 不缓冲转发 -> 记录不影响 SSE 首字延迟

## 查看器 (浏览器 + macOS app 两份)

唯一职责是把上面的记录读给人看, 代理能力一概不实现. 两份业务功能对齐, 读同一份 `.jsonl`, 同一套视图与排版:

- 左列表 (新 -> 旧) + 右上下面板: 选中一条即绑定展示它的 Request / Response
- 两组切换互不干扰: `Client ↔ Proxy` / `Proxy ↔ Upstream` 选哪条腿, `核心内容` / `原始报文` 选哪种读法 (⌘D)
- 核心内容 (默认): 见下「核心内容视图」
- 原始报文: 起始行 + header (按名排序) + 空行 + body; JSON 缩进展示 (对象键按字典序, 数组保持线上原序), SSE / 文本原样
- 每面板可 Copy (复制当前视图的文本)
- 顶栏 Follow 自动读入新记录 (选中行不跳走), 日期下拉切换历史, 过滤框按 path / model / status / surface 多词 AND
- 数据只读: 记录由代理写, 查看器从不写回

差异只在服务操作上:

- 浏览器版 (`:10020`): 操作直接调进程内的同一套实现, 不 exec CLI; `Console` 面板跑 Status / Models / Login / Logout / Stop 并回显。**`start` 例外** —— 页面由代理进程本身提供, 进程没起来时页面也不存在, 只能在终端执行
- macOS app (`app/`, SwiftPM + AppKit, 零第三方依赖): 全部转调 CLI 子进程 (`Console…` 面板实时回显), 因此 Start 也能点; app 不复刻任何判断
- Login 的授权回调端口写死在上游 client_id allow-list -> 浏览器只会开在跑着代理的那台机器上, 与从哪台机器点的无关

### 核心内容视图

SSE 原文是几百帧被切碎的 `event:` / `data:`, 人读不了 -> 先重建成完整消息再排纯文本. 请求侧同理: 从嵌套 JSON 里抽出对话轮次.

- 请求: `model` / `stream` / token 上限 / 采样参数一行, `tools` 名字一行; 之后 system (或 `instructions`) + 逐轮消息, 轮内的工具调用与结果缩进挂在该轮下
- 响应: 方言 + 帧数 + 是否收到收尾事件一行, `model` / `id` / `stop` 一行, usage 一行 (只留非 0 计数), 帧种类计数一行; 之后按产出顺序排 text / thinking / tool_call 段
- 工具参数逐帧拼回完整 JSON 再缩进展示; 流被截断没拼全就原样给
- 认三种方言 (与三个协议面一致): Anthropic Messages / OpenAI Chat Completions / OpenAI Responses; 流式与非流式同一套渲染
- 认不出方言: 每帧压成一行列出; 非对话请求 (`/v1/models`、`count_tokens`) 回退成原 JSON -> 永远给得出东西
- 「空」与「拿不到」分开说: 上游只回签名不回思考正文时标注加密, 不显示成空内容
- 单段超 128KB / 全文超 4MB 截断展示, 提示切「原始报文」看全文 (日志文件里一律全量)

## 架构

- 单进程 axum, 三个协议面共用凭证与连接池; 建连超时 20s / 读取空闲超时 300s; 直连上游无云端中转; req/res 只落本机日志目录
- 启动时先 bind 全部端口 (三协议面 + 查看器) 再对外服务: 任一被占立刻整体失败, 不留「只有一部分能用」的中间态
- 查看器端口不进协议面枚举: 它没有上游与凭证映射, 混进去会让透传 / 模型列表 / 错误信封处处长出「这个面不是代理」的分支
- 查看器的文件读取走 `spawn_blocking`: 单日记录可以是 GB 级 (body 全量且永不清理), 同步读会阻塞 runtime 拖累 SSE 首字延迟
- 查看器只吐摘要行与整行原文, 语义渲染 (SSE 重建 / 三方言归一) 在浏览器里做 -> 代理侧零解析负担
- 每个端口绑 `127.0.0.1` + 通配面 (`::` 优先, dual-stack 关掉时才用得上 `0.0.0.0`): loopback 绑不上即判定端口被占并整体失败 (语义与只监听本机时一致), 通配面失败只降级为「局域网连不上」并记 warn; BSD 按最具体地址派发, 两类 socket 并存不冲突
- 单实例: `daemon.pid` 独占文件锁, 锁由内核在进程退出时释放 -> 判活不受 pid 复用 / 残留 pid 文件影响
- `start` 拉起后台进程后等它写下就绪标记才报成功 (不靠「端口通」, 避免把别人占的端口认成自己); `stop` = SIGTERM -> 等锁释放 -> 5s 未退则 SIGKILL
- 后台日志单文件 8MB 上限, 满则轮转一份 -> 磁盘占用恒定, 不随运行时长增长
- 认证: OAuth PKCE (S256); 回调端口被上游 client_id allow-list 写死 (Anthropic 54545 / Codex 1455), 登录时须空闲
- 凭证: `auth.json` 进程间串行 + 原子写 + 0600; login/logout 热更新; 到期前 300s 主动刷新, 每 provider 单飞锁; 上游 401 时强制续期并重试一次
- 请求体带 `content-encoding: zstd` (pi-ai 等客户端会压) 一律在入口解开: body 规范化与往返记录都要读明文, 转发给上游恒为未压缩; 其余非 identity 编码直接 400, 好过把压缩字节冒充明文送上去
- 只提供 HTTP: codex 面客户端先试 WebSocket 时 (pi-ai `transport: auto`) 拿到 405 并自动回落 SSE; 客户端直接配 `transport: sse` 可省掉这次试探
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
src/main.rs               CLI (start / stop / login / logout / status / models / logs) + 四个固定端口 + 优雅退出
src/daemon.rs             后台常驻: 单实例锁 + 探活 + 启停 + 日志封顶
src/reqlog.rs             往返记录: 一行一次 req/res + 按天分文件 (不清理) + logs 摘要
src/server.rs             端口层: Chat Completions + 模型列表 + 往返记录挂点, 其余落透传
src/proxy.rs              透传: 上游由端口定 + header 注入 + body 规范化 + 带凭证请求上游
src/convert/mod.rs        Chat Completions 增量模型 + 流式回传 / 非流式聚合
src/convert/codex.rs      Chat Completions <-> OpenAI Responses
src/convert/anthropic.rs  Chat Completions <-> Anthropic Messages
src/sse.rs                SSE 解码
src/auth.rs               凭证内存态 + 到期预判 + 单飞刷新
src/oauth.rs              PKCE + 本机回调服务 + 两家 token 换取/刷新
src/provider.rs           协议面 <-> 端口 / 订阅映射 + 两家上游常量 (client_id / endpoint / CLI 冒充参数)
src/store.rs              auth.json 读写 (原子 + 0600)
src/webui.rs              浏览器查看器后端: 只读接口 (日期 / 增量索引 / 整行原文) + 状态 + 服务操作
src/webui/app.html        浏览器查看器前端: 单文件 (include_str! 进二进制), 零外部资源
scripts/install-local.sh  预部署总入口: CLI release 构建 + 安装 -> 续跑 app/package.sh (`--cli-only` 只装 CLI)

app/Package.swift         macOS 查看器 app: SwiftPM executable (macOS 13+, AppKit)
app/package.sh            Release 构建 + 组装 .app + ad-hoc 签名 + 装 /Applications (版本取自 Cargo.toml; 被 install-local.sh 调用)
app/Resources/            bundle 模板 (Info.plist.in, `@VERSION@` 占位)
app/Sources/jj-agentic-proxy/
  main.swift              入口 + `--snapshot <png>` 界面自检
  AppDelegate.swift       主窗口 + 尺寸持久化
  MainMenu.swift          程序化主菜单
  MainViewController.swift 列表 + 过滤 + follow + 详情绑定
  BodyPane.swift          单个 body 面板 (等宽只读 + Copy)
  CoreContent.swift       核心内容视图: SSE 重建 + 三方言归一 + 纯文本排版
  TrafficRecord.swift     一行记录的摘要模型 + 行首摘要解析
  TrafficReader.swift     日期枚举 + 增量索引 + 按 (offset, length) 现取全文 (原始报文 + 核心内容各一份)
  ConsoleWindowController.swift  CLI 控制台面板
  CommandRunner.swift     跑 CLI 子命令 + 输出实时回吐
  ProxyPaths.swift        CLI / 日志目录定位
```
