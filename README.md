```When Editing
本文档作用: 工程总览 (价值主张 / 使用 / 架构 / 结构); MUST NOT 写发布流程 (→ workflow.md) / LLM 约束 (→ AGENTS.md)
遵循 AGENTS.md 文档编写规范
- 章节按需增删, 只留项目真有的; 首行一行价值主张, MUST NOT 带 LLM 提示
- 短并列项用表格; 可执行步骤 fenced + `#` 注释同行
- NEVER 写「开发」段 (VibeCoding 不向人类解释 dev 命令)
```

# jj-agentic-proxy

本机 agentic proxy: 复用用户自有订阅 (Claude Pro / ChatGPT Plus 等), 经 Web OAuth 取 token, 为 Claude Code / Codex 提供本地代理端点.

## 使用

1. 启动代理
2. 用 CLI 自带 login 命令完成 OAuth 授权 (目前支持 codex / claude-code), token 落本地
3. 将 Claude Code / Codex 的 API base 指向本机代理地址

## 架构

- 本机 HTTP 服务, 仅监听 loopback; 不做云端中转
- 认证: OAuth (PKCE) 换 token, 由 CLI login 命令驱动, 本地持久化
- 转发: 按上游 (Anthropic / OpenAI) 分路由, 注入凭证, 流式 (SSE) 透传
