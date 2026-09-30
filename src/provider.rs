//! 两家上游的固定事实 (client_id / endpoint / 冒充参数)。
//!
//! 值来源: openai/codex `codex-rs/login`, Claude Code CLI OAuth 流程。
//! 会随上游演进的 CLI 版本号跟随本机安装周期重读, 内置值只作下限, 避免硬编码衰减。

use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

/// 上游目前只校验客户端身份 (OAuth + system 前缀), 不校验 claude-cli 版本 (实测 2.1.88 可用);
/// 但 Codex 那边已出现按 CLI 版本 gate 新模型 -> 同样跟随本机 CLI, 内置值只作下限。
const CLAUDE_CLI_VERSION_FLOOR: &str = "2.1.88";

/// `claude-cli/<ver> (external, cli)`
pub fn claude_user_agent() -> String {
    static CACHE: VersionCache = VersionCache::new();
    let ver = CACHE.get(|| {
        newest([
            Some(CLAUDE_CLI_VERSION_FLOOR.to_string()),
            local_claude_version(),
        ])
    });
    format!("claude-cli/{ver} (external, cli)")
}

/// 原生安装器把每个版本解到 `~/.local/share/claude/versions/<ver>`; 取其中最新的。
/// npm 全局安装等其他方式找不到 -> 回落下限。
fn local_claude_version() -> Option<String> {
    let dir = PathBuf::from(std::env::var("HOME").ok()?).join(".local/share/claude/versions");
    let names = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok());
    let v = newest(names.map(Some));
    (!v.is_empty()).then_some(v)
}

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

/// 取 内置下限 / 本机 codex 各处自报版本 / Codex 最新发版号 中最新的一个, 按 `VERSION_TTL` 重读。
///
/// 实测坑: 启动时读一次就冻结 -> daemon 连跑数周后本机 CLI 早已升级, 上游按旧版本号拒新模型。
/// 本机没装 codex (只装了本 app) 时前两项都没有, 靠 `learn_codex_latest` 取到的最新发版号不掉队
/// (实测: 按 0.146.0 请求, 清单里 gpt-6 系列整批消失; gpt-6.1-sol 要 0.159.0 起)。
pub fn codex_cli_version() -> String {
    CODEX_VERSION.get(|| {
        let home = codex_home();
        let learned = LEARNED_CODEX_FLOOR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        newest([
            Some(CODEX_CLI_VERSION_FLOOR.to_string()),
            // 已装版本 (模型缓存按它请求上游); version.json 是「可升级到」的版本, 常落后于已装
            home.as_ref()
                .and_then(|h| json_str(&h.join("models_cache.json"), "client_version")),
            home.as_ref()
                .and_then(|h| json_str(&h.join("version.json"), "latest_version")),
            learned,
        ])
    })
}

static CODEX_VERSION: VersionCache = VersionCache::new();
static LEARNED_CODEX_FLOOR: Mutex<Option<String>> = Mutex::new(None);

/// 官方 CLI 升级检查用的同一个端点; tag 形如 `rust-v0.159.2`。
pub const CODEX_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/openai/codex/releases/latest";

/// 记下最新发版号并让缓存立即失效; 形状不对 (非 `X.Y.Z`) 不采纳。返回是否采纳。
pub fn learn_codex_latest(version: &str) -> bool {
    if semver_key(version).is_none() {
        return false;
    }
    *LEARNED_CODEX_FLOOR
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(version.to_string());
    CODEX_VERSION.clear();
    true
}

fn codex_home() -> Option<PathBuf> {
    match std::env::var("CODEX_HOME") {
        Ok(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => Some(PathBuf::from(std::env::var("HOME").ok()?).join(".codex")),
    }
}

fn json_str(path: &std::path::Path, key: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let v = value.get(key)?.as_str()?;
    (!v.is_empty()).then(|| v.to_string())
}

/// 本机 CLI 版本重读间隔: 升级后最迟这么久跟上, 又不至于每个请求都读盘。
const VERSION_TTL: Duration = Duration::from_secs(600);

/// 按 TTL 重算的版本号缓存。
struct VersionCache(Mutex<Option<(Instant, String)>>);

impl VersionCache {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn clear(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn get(&self, compute: impl FnOnce() -> String) -> String {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = slot.as_ref() {
            if at.elapsed() < VERSION_TTL {
                return v.clone();
            }
        }
        let v = compute();
        *slot = Some((Instant::now(), v.clone()));
        v
    }
}

/// `X.Y.Z` 按数值比较取最大 (字典序会把 0.99 排在 0.159 之后); 解析不了的忽略。
pub fn newest(candidates: impl IntoIterator<Item = Option<String>>) -> String {
    candidates
        .into_iter()
        .flatten()
        .filter_map(|v| semver_key(&v).map(|k| (k, v)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v)
        .unwrap_or_default()
}

fn semver_key(v: &str) -> Option<[u64; 3]> {
    let mut parts = v.trim().split('.');
    let mut key = [0u64; 3];
    for slot in &mut key {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(key)
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

    #[test]
    fn newest_compares_numerically_and_skips_garbage() {
        let v = newest([
            Some("0.99.0".into()),
            Some("0.159.2".into()),
            Some("latest".into()),
            None,
            Some("0.158.0".into()),
        ]);
        assert_eq!(v, "0.159.2");
    }

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
