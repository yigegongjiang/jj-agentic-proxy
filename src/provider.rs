//! 两家上游的固定事实 (client_id / endpoint / 冒充参数)。
//!
//! 值来源: openai/codex `codex-rs/login`, Claude Code CLI OAuth 流程。
//! 会随上游演进的版本号一律可用 env 覆盖, 避免硬编码衰减。

use std::fmt;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 只监听 loopback: 代理持有个人订阅凭证, 不对外暴露。
pub const HOST: &str = "127.0.0.1";

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

    /// 一个 provider 一个固定端口, 不可配置 -> 客户端 base url 永远可写死。
    pub const fn port(self) -> u16 {
        match self {
            Provider::Codex => 10010,
            Provider::Anthropic => 10011,
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Provider::Anthropic => Provider::Codex,
            Provider::Codex => Provider::Anthropic,
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

/// 优先级: `JJ_PROXY_CODEX_CLI_VERSION` > 本机 codex CLI 版本 > 内置下限。
pub fn codex_cli_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            std::env::var("JJ_PROXY_CODEX_CLI_VERSION")
                .ok()
                .or_else(local_codex_version)
                .unwrap_or_else(|| CODEX_CLI_VERSION_FLOOR.to_string())
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
