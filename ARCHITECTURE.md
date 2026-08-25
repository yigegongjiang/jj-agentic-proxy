```When Editing
本文档作用: 内部实现 (端点字段支持 / 透传口 / 往返记录 / 查看器 / 架构 / 代码结构); 读者 = 维护本工程的 AI
MUST NOT 写安装 · 上手 (→ README.md) / 发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 每条须是事实 (行为 / 阈值 / 实测结论), MUST NOT 写动机式空话
- 反劣化理由 (实测坑 / 为什么这么选) MUST 留, 删掉等于给未来的 agent 留回退空间
```

# jj-agentic-proxy 内部实现

对外用法见 [README.md](./README.md); 本文只写实现事实。

## 端点

<!-- prettier-ignore -->
| 端点 | 端口 | 上游 | 协议 |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | 全部 | 按端口定 | OpenAI Chat Completions |
| `GET /v1/models`, `/v1/models/{id}` | 全部 | 按端口定 | OpenAI 列表 (仅本端口订阅) |
| `POST /v1/messages` | 10011 | `api.anthropic.com/v1/messages` | Anthropic Messages |
| `POST /v1/messages/count_tokens` | 10011 | `api.anthropic.com` 同路径 | Anthropic Messages |
| `POST /v1/responses` | 10010 | `chatgpt.com/backend-api/codex/responses` | OpenAI Responses |
| `POST /v1/responses/*` | 10010 | 上游 `/responses` 同子路径 (含 `/compact`) | OpenAI Responses |
| `ANY /backend-api/codex/*` | 10010 | `chatgpt.com/backend-api/codex` 同路径 | 上游替身入口 ([透传口](#codex-透传口)) |
| `POST /v1/complete` | 10011 | `api.anthropic.com/v1/complete` | Anthropic legacy Text Completions |
| `GET /health` | 全部 | — | 本端口协议面 + 登录状态 + 可用路径 |

- 端口层只截 Chat Completions 与模型列表, 其余路径一律落原生透传 (`proxy::resolve`): 按端口前缀白名单放行 -> 上游同路径, body 不改协议只换身份
  - 10011: `/messages` / `/models` / `/complete` 及其子路径 -> `api.anthropic.com/v1{path}`
  - 10010: `/responses` 及其子路径 + `/backend-api/codex/*` -> `chatgpt.com/backend-api/codex{...}`
  - 10012: 只有 `/chat/completions` (交给上游官方兼容层), 别的一概 404
  - 不在白名单的路径不猜: 404 里列出本端口可用路径 (`proxy::endpoints`, 与 `/health` 同一份)
  - `endpoints` 列表未含 `/v1/complete`, 而 `resolve` 白名单含 -> 该路径实际转发但不出现在 404 提示与 `/health` 里 (代码不一致, 未改行为)
- Chat Completions 的上游由端口决定, `model` 只取模型名 (允许 `anthropic/`、`openai/` 前缀)
- 10010 / 10011 本地转换覆盖: 流式 / 非流式、tools + 工具结果回传、图片 (url 与 data URI)、`response_format`、`reasoning_effort`; 思考内容出 `reasoning_content`
- 采样参数 (`temperature` / `top_p` / `top_k`) 在所有 Anthropic 面 (10011 原生 + 10011/10012 Chat Completions) 一律丢弃: 上游新模型按「键是否存在」硬拒 (400 `` `temperature` is deprecated ``), 与取值无关, 且受限名单随新模型扩张 -> 不做模型名判断
- 10011 的 Chat Completions 把 `reasoning_effort` 映射成上游现行思考档位
- 10012 只有 Chat Completions 与模型列表 (原生 Messages 走 10011), 字段支持度以[上游兼容层](https://platform.claude.com/docs/en/api/openai-sdk)为准: 无 `reasoning_content`, `response_format` / `reasoning_effort` / `seed` 等被上游静默忽略
- `/v1/models` 只在 10011 且请求带 `x-api-key` / `anthropic-version` 时给 Anthropic 官方原样, 其余一律 OpenAI 列表
- 错误一律按方言裹官方信封 (`{"type":"error",...}` / `{"error":{...}}`); 上游已给官方形状则原样透传, 保留 `request-id` 等头

## Codex 透传口

`ANY :10010/backend-api/codex/*` -> `chatgpt.com/backend-api/codex` 同路径 = 把上游域名换成本机端口的**替身入口**, 只换上游身份 (本机 OAuth token + Codex CLI header). 用途 = 官方 CLI 那套私有路径直接可打, 不受本地端点清单限制.

```bash
curl http://127.0.0.1:10010/backend-api/codex/usage  # 订阅额度: plan_type + rate_limit 窗口 + used_percent
```

- 替身入口不等于零改写: `resolve` 把 `/backend-api/codex` 之后的部分当 `upstream_path` -> `/backend-api/codex/responses` 与 `/v1/responses` 拿到同一个 `upstream_path == "/responses"`, 走同一份 `normalize_codex` + 强制 SSE + 非流式聚合 (完全等价的两个入口, 非两条链路); 只有 `/responses` 之外的子路径 (`/usage` 等) 才是 body 原样出去
- 零改写不止这个口: body 改写按 `upstream_path` 精确门控 (`/v1/messages*` 注 CLI 前缀 + 丢采样参数; 10012 hoist system; Codex 仅 `upstream_path == "/responses"` 才 normalize), 所以 `/responses/compact`、`/v1/complete` 也是 body 原样出去 —— `/v1/complete` 连 Claude Code system 前缀都不注入 (上游是否受理未实测)
- 与其余端点同规: 吃 `/v1` 前缀 (base url 按 OpenAI SDK 约定写成 `.../v1` 时拼出的 `/v1/backend-api/codex/*` 同样命中) + 不需要 api key (代理不校验客户端凭证, 上游身份一律用本机 OAuth)
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
- 核心内容 (默认): 几百帧碎 SSE 先重建成完整消息再排纯文本, 请求侧从嵌套 JSON 抽出对话轮次 (system 或 `instructions` + 逐轮消息, 轮内工具调用与结果缩进挂在该轮下), 响应给方言 / 帧数 / 是否收到收尾事件 / usage 摘要后按产出顺序排 text / thinking / tool_call 段; 工具参数逐帧拼回完整 JSON, 流被截断没拼全就原样给
- 核心内容认三种方言 (与三个协议面一致): Anthropic Messages / OpenAI Chat Completions / OpenAI Responses, 流式与非流式同一套渲染; 认不出就每帧压成一行, 非对话请求 (`/v1/models`、`count_tokens`) 回退成原 JSON -> 永远给得出东西
- 「空」与「拿不到」分开说: 上游只回签名不回思考正文时标注加密, 不显示成空内容
- 原始报文: 起始行 + header (按名排序) + 空行 + body; JSON 缩进展示 (对象键按字典序, 数组保持线上原序), SSE / 文本原样
- 核心内容单段超 128KB / 全文超 4MB 截断展示, 提示切「原始报文」看全文 (日志文件里一律全量)
- 每面板可 Copy (复制当前视图的文本)
- 顶栏 Follow 自动读入新记录 (选中行不跳走), 日期下拉切换历史, 过滤框按 path / model / status / surface 多词 AND
- 数据只读: 记录由代理写, 查看器从不写回

差异只在服务操作上:

- 浏览器版 (`:10020`): 操作直接调进程内的同一套实现, 不 exec CLI; `Console` 面板跑 Status / Models / Login / Logout / Stop 并回显。**`start` 例外** —— 页面由代理进程本身提供, 进程没起来时页面也不存在, 只能在终端执行
- macOS app (`app/`, SwiftPM + AppKit, 零第三方依赖): 全部转调 CLI 子进程 (`Console…` 面板实时回显), 因此 Start 也能点; app 不复刻任何判断
- Login 的授权回调端口写死在上游 client_id allow-list -> 浏览器只会开在跑着代理的那台机器上, 与从哪台机器点的无关

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
src/main.rs                    CLI (start / stop / login / logout / status / models / logs) + 四个固定端口 + 优雅退出
src/daemon.rs                  后台常驻: 单实例锁 + 探活 + 启停 + 日志封顶
src/server.rs                  端口层: Chat Completions + 模型列表 + 往返记录挂点, 其余落透传
src/proxy.rs                   透传: 上游由端口定 + header 注入 + body 规范化 + 带凭证请求上游
src/convert/ + sse.rs          Chat Completions <-> OpenAI Responses (codex.rs) / Anthropic Messages (anthropic.rs); mod.rs 增量模型 + 流式回传 / 非流式聚合; sse.rs 解码
src/reqlog.rs                  往返记录: 一行一次 req/res + 按天分文件 (不清理) + logs 摘要
src/{auth,oauth,store}.rs      凭证内存态 / 到期预判 / 单飞刷新; PKCE + 本机回调 + 两家 token 换取·刷新; auth.json 原子写 0600
src/provider.rs                协议面 <-> 端口 / 订阅映射 + 两家上游常量 (client_id / endpoint / CLI 冒充参数)
src/webui.rs + webui/app.html  浏览器查看器: 只读接口 (日期 / 增量索引 / 整行原文) + 状态 + 服务操作; 前端单文件 include_str! 进二进制, 零外部资源
scripts/install-local.sh       本机预部署总入口 = 本机架构构建 + 装 /Applications + 链接终端命令 (→ workflow.md); 只给开发用, 用户侧不跑脚本
scripts/make-dist.sh           分发打包: arm64 / x86_64 各构建一套 -> dist/ 下 dmg + SHA256SUMS + release notes
.github/workflows/release.yml  打 tag 即发版: 验证 + 跑 make-dist.sh + 产物传 GitHub Release (CI 是脚本的薄壳, 本机可复现)

app/                           macOS 查看器 app: SwiftPM + AppKit (macOS 13+, 零第三方依赖); package.sh 组装 bundle, Resources/ 存 Info.plist.in (`@VERSION@` 占位)
app/Sources/jj-agentic-proxy/
  MainViewController.swift + BodyPane.swift            列表 / 过滤 / follow / 详情绑定 + 单 body 面板 (等宽只读 + Copy)
  CoreContent.swift                                    核心内容视图: SSE 重建 + 三方言归一 + 纯文本排版
  TrafficRecord.swift + TrafficReader.swift            行首摘要解析 + 日期枚举 / 增量索引 / 按 (offset, length) 现取全文
  ConsoleWindowController.swift + CommandRunner.swift  CLI 控制台面板 + 子进程输出实时回吐
  main.swift + AppDelegate.swift + MainMenu.swift      入口 (`--snapshot <png>` 界面自检) / 主窗口 + 尺寸持久化 / 主菜单
  CLIInstall.swift                                     终端命令入口: 打开 app 时检查 ~/.local/bin symlink, 缺则弹窗一键建 + 摘 quarantine + `--version` 自检
  ProxyPaths.swift                                     CLI 定位 (首选自身同目录那份) / 日志目录
```
