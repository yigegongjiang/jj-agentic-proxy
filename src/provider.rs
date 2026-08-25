//! 两家上游的固定事实 (client_id / endpoint / 冒充参数)。
//!
//! 值来源: openai/codex `codex-rs/login`, Claude Code CLI OAuth 流程。
//! 会随上游演进的版本号一律可用 env 覆盖, 避免硬编码衰减。

use std::fmt;
use std::path::PathBuf;
use std::sync::OnceLock;

/// loopback 面: 必须绑到, 绑不上即判定端口被占。
pub const BIND_LOOPBACK: &str = "127.0.0.1";

/// 通配面: 让同局域网的其他主机能用本机 LAN IP 连 (best-effort)。
///
/// `::` 在先 — macOS 默认 dual-stack, 一个 socket 就覆盖 v4 与 v6;
/// dual-stack 被关掉时才轮到 `0.0.0.0` 补上 v4。BSD 按最具体地址派发,
/// 通配面与上面的 loopback socket 并存不冲突。
pub const BIND_WILDCARDS: [&str; 2] = ["::", "0.0.0.0"];

/// 打印 base url 用的本机地址 (局域网客户端换成本机 LAN IP)。
pub const HOST: &str = "127.0.0.1";

/// 客户端填的 api key。代理不校验它, 但 codex 协议面的部分客户端 (pi-ai 等) 会在发请求
/// 前把它当 JWT 本地解析, 取 `https://api.openai.com/auth` 下的 `chatgpt_account_id`
/// 当 `chatgpt-account-id` 头 -> 随便一个非空串会让这些客户端连请求都发不出去。
///
/// 这里给一份固定的合法 JWT 形态值, 三个协议面通用。上游身份一律用本机 OAuth 凭证,
/// 客户端送来的 authorization 与 chatgpt-account-id 都会被 `codex_headers` 覆盖 ->
/// 这个值只满足客户端本地解析, 不参与任何鉴权。
///
/// - 不带 `exp`: 永不过期, 客户端配一次就不用再动
/// - payload 明文: `{"https://api.openai.com/auth":{"chatgpt_account_id":"jj-agentic-proxy"}}`
/// - 三段 base64 都落在标准与 url-safe 字母表的交集 (无 `+` `/` `-` `_`) -> 两种解码器都能读
pub const CLIENT_API_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiamotYWdlbnRpYy1wcm94eSJ9fQ.jjagenticproxy";

/// Web 查看器端口。刻意不进 `Surface`: 它不是协议面, 没有上游与凭证映射,
/// 混进去会让透传 / 模型列表 / 错误信封到处长出「这个面不是代理」的分支。
pub const UI_PORT: u16 = 10020;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Provider {
    Anthropic,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Codex, Provider::Anthropic];

    pub fn key(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Codex => "codex",
        }
    }

    /// 接受官方名与常用别名。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" | "claude-code" | "claudecode" => Some(Provider::Anthropic),
            "codex" | "openai" | "chatgpt" => Some(Provider::Codex),
            _ => None,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// 一个端口 = 一个协议面。凭证按 provider 复用: 10011 / 10012 共用同一份 Anthropic 凭证。
///
/// 端口固定不可配置 -> 客户端 base url 永远可写死。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Surface {
    /// 10010: Codex 原生 (OpenAI Responses)
    Codex,
    /// 10011: Anthropic 原生 (Messages)
    ClaudeCode,
    /// 10012: Anthropic 官方 OpenAI 兼容层直通
    ClaudeOpenAI,
}

impl Surface {
    pub const ALL: [Surface; 3] = [Surface::Codex, Surface::ClaudeCode, Surface::ClaudeOpenAI];

    pub const fn key(self) -> &'static str {
        match self {
            Surface::Codex => "codex",
            Surface::ClaudeCode => "claude-code",
            Surface::ClaudeOpenAI => "claude-openai",
        }
    }

    pub const fn port(self) -> u16 {
        match self {
            Surface::Codex => 10010,
            Surface::ClaudeCode => 10011,
            Surface::ClaudeOpenAI => 10012,
        }
    }

    /// 该端口用哪份凭证 / 走哪家上游。
    pub const fn provider(self) -> Provider {
        match self {
            Surface::Codex => Provider::Codex,
            Surface::ClaudeCode | Surface::ClaudeOpenAI => Provider::Anthropic,
        }
    }
}

impl fmt::Display for Surface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

// ---------- Anthropic (Claude Code CLI) ----------

pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const ANTHROPIC_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const ANTHROPIC_CALLBACK_PORT: u16 = 54545;
pub const ANTHROPIC_CALLBACK_PATH: &str = "/callback";
pub const ANTHROPIC_SCOPES: [&str; 3] = ["org:create_api_key", "user:profile", "user:inference"];
pub const ANTHROPIC_UPSTREAM: &str = "https://api.anthropic.com";
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
/// OAuth 凭证只被授权用于 Claude Code, system 第一块必须带此前缀, 否则上游 403。
pub const CLAUDE_CODE_SYSTEM_PREFIX: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
/// Anthropic 官方 OpenAI 兼容层 (同域, 由上游自己做协议转换)。
pub const ANTHROPIC_OPENAI_PATH: &str = "/v1/chat/completions";

pub fn anthropic_redirect_uri() -> String {
    format!("http://localhost:{ANTHROPIC_CALLBACK_PORT}{ANTHROPIC_CALLBACK_PATH}")
}

/// 上游只校验客户端身份 (OAuth + system 前缀), 不校验 claude-cli 版本 -> 固定值即可。
pub const CLAUDE_USER_AGENT: &str = "claude-cli/2.1.88 (external, cli)";

// ---------- OpenAI Codex CLI ----------

pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_ISSUER: &str = "https://auth.openai.com";
pub const CODEX_CALLBACK_PORT: u16 = 1455;
pub const CODEX_CALLBACK_PATH: &str = "/auth/callback";
pub const CODEX_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
pub const CODEX_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex";
pub const CODEX_ORIGINATOR: &str = "codex_cli_rs";

pub fn codex_authorize_url() -> String {
    format!("{CODEX_ISSUER}/oauth/authorize")
}

pub fn codex_token_url() -> String {
    format!("{CODEX_ISSUER}/oauth/token")
}

pub fn codex_redirect_uri() -> String {
    format!("http://localhost:{CODEX_CALLBACK_PORT}{CODEX_CALLBACK_PATH}")
}

/// 版本号过旧时上游拒绝新模型 ("requires a newer version of Codex")。
/// 兜底常量只是下限, 会随时间失效, 因此优先跟随本机 codex CLI 自报的最新版本。
const CODEX_CLI_VERSION_FLOOR: &str = "0.146.0";

/// 优先级: 本机 codex CLI 版本 > 内置下限 (仅 version.json 读取失败时兜底)。
pub fn codex_cli_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            local_codex_version().unwrap_or_else(|| CODEX_CLI_VERSION_FLOOR.to_string())
        })
        .clone()
}

/// 本机 codex CLI 把最新版本号写进 `$CODEX_HOME/version.json`, 随其自动更新。
fn local_codex_version() -> Option<String> {
    let home = match std::env::var("CODEX_HOME") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(std::env::var("HOME").ok()?).join(".codex"),
    };
    let bytes = std::fs::read(home.join("version.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let version = value.get("latest_version")?.as_str()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// `codex_cli_rs/<ver> (<os>; <arch>)`
pub fn codex_user_agent() -> String {
    let os = std::env::consts::OS; // macos / linux / windows
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => other,
    };
    format!("{CODEX_ORIGINATOR}/{} ({os}; {arch})", codex_cli_version())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine as _;

    /// 客户端拿这个值当 JWT 解析 -> 改坏了只会在客户端侧报错, 这里守住形状。
    #[test]
    fn client_api_key_carries_chatgpt_account_id() {
        let parts: Vec<&str> = CLIENT_API_KEY.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT 必须三段");
        assert!(
            !CLIENT_API_KEY.contains(['+', '/', '-', '_']),
            "标准与 url-safe base64 解码器都要能读"
        );
        let payload = STANDARD_NO_PAD.decode(parts[1]).expect("payload 应可解码");
        let value: serde_json::Value = serde_json::from_slice(&payload).expect("payload 应是 JSON");
        assert_eq!(
            value["https://api.openai.com/auth"]["chatgpt_account_id"],
            "jj-agentic-proxy"
        );
        assert!(value.get("exp").is_none(), "MUST NOT 带时效值");
    }
}
