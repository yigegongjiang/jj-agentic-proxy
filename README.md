```When Editing
本文档作用: 对外入口 (价值主张 / 安装 / 上手 / 接入所需的最小事实); 读者 = 陌生开发者, MUST 一屏读完 + 3 分钟跑通
MUST NOT 写内部实现 (→ ARCHITECTURE.md) / 发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 只留「不知道就跑不起来 / 会踩坑」的事实; 实现理由 · 字段清单 · 代码地图一律外链
- 首行一行价值主张; 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 把自有订阅 (Claude Pro / ChatGPT Plus) 转成标准 API key 形式 -> 任意 OpenAI / Anthropic 兼容客户端填个 base url + key 就能用订阅额度跑, 不碰 API 计费.

经 Web OAuth 取 token, 以官方 CLI 身份请求上游, 协议差异在本地改写抹平: Codex `:10010`、Claude Code `:10011` (原生 Anthropic) 与 `:10012` (Anthropic 官方 OpenAI 兼容层); 全部端口监听全部网卡 -> 本机与同局域网主机都能连; 经手的 req/res 全量落本机文本日志, 配套查看器两份 (`:10020` 浏览器 + macOS app) 做绑定查看.

## 安装

macOS 13+ (Apple Silicon / Intel 各一份包)。一条命令装最新版, 自动认架构 (升级 = 重跑同一条):

```bash
curl -fsSL https://github.com/yigegongjiang/jj-agentic-proxy/releases/latest/download/install.sh | bash
```

- 手动装: [Releases](https://github.com/yigegongjiang/jj-agentic-proxy/releases/latest) 按机器架构取 `-arm64` / `-x86_64` 那份 (`uname -m` 一看便知), `.tar.gz` 解压 / `.dmg` 挂载后都跑里面的 `install.sh` (只有它会建终端命令的链接); 校验用同页 `SHA256SUMS` 对 `shasum -a 256`
- 无 Apple 开发者签名 (ad-hoc): curl / tar 路径不带 quarantine 直接可跑; 浏览器下的 `.dmg` 会带, `install.sh` 装完就地摘掉 -> 全程不用去系统设置点「仍要打开」
- 一个 bundle 三合一: CLI 在包体内 (`Contents/MacOS/jj-agentic-proxy-cli`), macOS 查看器 = 同 bundle 主程序 (双击即用), `~/.local/bin/jj-agentic-proxy` 只是指过去的 symlink -> 升级只装 app, 永不版本错位; 删掉 app 则 CLI 一起没了, 安装中途失败留断链 -> 重跑安装即恢复
- `~/.local/bin` 需在 `PATH`: `export PATH="$HOME/.local/bin:$PATH"`

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
- `start` = restart -> 升级 / 换版本后一条命令即生效, 永不出现两个实例
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
- 无鉴权 -> 能连到这些端口的主机即可用本机订阅、读全部往返记录 (含原样授权头); 仅限可信内网, 端口 MUST NOT 经路由器映射到公网
- macOS 应用防火墙开着时首次启动会弹「是否允许接受传入网络连接」, 选允许; 静默拒绝会表现为局域网连不上而本机正常 (二进制换到新路径后会再问一次, 比如首次改用 app 包体内那份)
- 请求打到哪个端口就走哪家订阅, 与 model 名无关 (10011 / 10012 共用同一份 Claude 凭证, 只是协议转换发生在本地还是上游); 路径走错端口时 404 直接给出正确端口
- base url 带不带 `/v1` 后缀都通, 三个协议端口一律如此: `http://<host>:10010` (Anthropic SDK 约定) 与 `http://<host>:10010/v1` (OpenAI SDK 约定, 也是多数客户端 placeholder 的写法) 等价; 客户端重复拼出的 `/v1/v1/...` 一并归一
- api key 用 `start` / `status` 打印的那份固定值 (三面通用, 永不过期, 直接复制): 代理不校验它, 上游身份一律用本机 OAuth 凭证 -> 任意非空串通常也行, 但 codex 面的部分客户端 (pi-ai 等) 会在发请求前把 key 当 JWT 本地解析取 `chatgpt_account_id`, 只有这份固定值过得去
- 全放开 CORS: 浏览器页面可直连, 预检由代理直接应答

## 接入

- 每个协议端口都收 `POST /v1/chat/completions` + `GET /v1/models`; 10011 另收原生 `POST /v1/messages` (含 `count_tokens`), 10010 另收 `POST /v1/responses` (含 `compact`) 与 Codex 私有端点透传
- 客户端只需填 base url (`http://<host>:<端口>`) + 上面那份固定 api key, 模型名从 `jj-agentic-proxy models` 里挑
- 每次往返 (客户端 <-> 代理 <-> 上游) 全量落 `~/.config/jj-agentic-proxy/log/<日期>.jsonl`; 浏览器查看器随代理进程一起起来 (开 `http://127.0.0.1:10020`, 无需安装), macOS app 双击即看; SSE 碎帧已重建成完整对话
- 端点字段支持 / 记录格式 / 查看器细节 / 架构 / 代码结构 -> [ARCHITECTURE.md](./ARCHITECTURE.md)
