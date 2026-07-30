//! Web OAuth (PKCE) 登录 + token 换取 / 刷新。
//!
//! 两家都要求 redirect_uri 命中各自 client_id 的固定 allow-list, 所以回调端口写死。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::provider::{self, Provider};
use crate::store::{now, Credential};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn pkce() -> Pkce {
    let verifier = random_b64(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

pub fn random_b64(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 走完一次交互式登录, 返回可落盘的凭证。
pub async fn login(http: &reqwest::Client, p: Provider) -> Result<Credential> {
    let pkce = pkce();
    let state = random_b64(32);
    let (auth_url, port, path) = match p {
        Provider::Anthropic => (
            anthropic_authorize_url(&pkce, &state),
            provider::ANTHROPIC_CALLBACK_PORT,
            provider::ANTHROPIC_CALLBACK_PATH,
        ),
        Provider::Codex => (
            codex_authorize_url(&pkce, &state),
            provider::CODEX_CALLBACK_PORT,
            provider::CODEX_CALLBACK_PATH,
        ),
    };

    let listeners = bind_callback(port)?;
    println!("在浏览器中完成授权 (等待回调, 最多 5 分钟):\n{auth_url}\n");
    let _ = webbrowser::open(&auth_url);

    let expect_path = path.to_string();
    let cb = tokio::task::spawn_blocking(move || wait_for_callback(listeners, &expect_path))
        .await
        .map_err(|e| anyhow!("回调任务异常: {e}"))??;

    if cb.state != state {
        bail!("OAuth state 不匹配, 已中止 (可能是 CSRF 或并发登录)");
    }

    match p {
        Provider::Anthropic => anthropic_exchange(http, &cb.code, &state, &pkce.verifier).await,
        Provider::Codex => codex_exchange(http, &cb.code, &pkce.verifier).await,
    }
}

pub async fn refresh(http: &reqwest::Client, p: Provider, cred: &Credential) -> Result<Credential> {
    let fresh = match p {
        Provider::Anthropic => anthropic_refresh(http, &cred.refresh_token).await?,
        Provider::Codex => codex_refresh(http, &cred.refresh_token).await?,
    };
    // 刷新响应可能不带账号信息, 沿用旧值。
    Ok(Credential {
        account: fresh.account.or_else(|| cred.account.clone()),
        account_id: fresh.account_id.or_else(|| cred.account_id.clone()),
        plan: fresh.plan.or_else(|| cred.plan.clone()),
        ..fresh
    })
}

// ---------- Anthropic ----------

fn anthropic_authorize_url(pkce: &Pkce, state: &str) -> String {
    // Anthropic 的授权端点要求 scope 里的冒号保持未编码, 空格用 `+`。
    let scope = provider::ANTHROPIC_SCOPES.join("+");
    let redirect = urlencode(&provider::anthropic_redirect_uri());
    format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&code_challenge={}\
         &code_challenge_method=S256&state={}&scope={scope}",
        provider::ANTHROPIC_AUTHORIZE_URL,
        provider::ANTHROPIC_CLIENT_ID,
        redirect,
        urlencode(&pkce.challenge),
        urlencode(state),
    )
}

#[derive(Deserialize)]
struct AnthropicTokenResp {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    account: Option<AnthropicAccount>,
}

#[derive(Deserialize)]
struct AnthropicAccount {
    #[serde(default)]
    email_address: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
}

impl From<AnthropicTokenResp> for Credential {
    fn from(r: AnthropicTokenResp) -> Self {
        let (account, account_id) = match r.account {
            Some(a) => (a.email_address, a.uuid),
            None => (None, None),
        };
        Credential {
            access_token: r.access_token,
            refresh_token: r.refresh_token,
            expires_at: now().saturating_add(r.expires_in.unwrap_or(3600)),
            account,
            account_id,
            plan: None,
        }
    }
}

async fn anthropic_exchange(
    http: &reqwest::Client,
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<Credential> {
    // 回调有时把 code 与 state 用 `#` 拼在一起返回。
    let code = code.split('#').next().unwrap_or(code);
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": provider::anthropic_redirect_uri(),
        "client_id": provider::ANTHROPIC_CLIENT_ID,
        "code_verifier": verifier,
        "state": state,
    });
    let resp = http
        .post(provider::ANTHROPIC_TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("Anthropic token 换取请求失败")?;
    Ok(
        json_or_err::<AnthropicTokenResp>(resp, "Anthropic token 换取")
            .await?
            .into(),
    )
}

async fn anthropic_refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Credential> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": provider::ANTHROPIC_CLIENT_ID,
    });
    let resp = http
        .post(provider::ANTHROPIC_TOKEN_URL)
        .json(&body)
        .send()
        .await
        .context("Anthropic token 刷新请求失败")?;
    Ok(
        json_or_err::<AnthropicTokenResp>(resp, "Anthropic token 刷新")
            .await?
            .into(),
    )
}

// ---------- Codex ----------

fn codex_authorize_url(pkce: &Pkce, state: &str) -> String {
    let redirect = provider::codex_redirect_uri();
    let params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", provider::CODEX_CLIENT_ID),
        ("redirect_uri", redirect.as_str()),
        ("scope", provider::CODEX_SCOPE),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", provider::CODEX_ORIGINATOR),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{qs}", provider::codex_authorize_url())
}

#[derive(Deserialize)]
struct CodexTokenResp {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl From<CodexTokenResp> for Credential {
    fn from(r: CodexTokenResp) -> Self {
        let claims = r.id_token.as_deref().and_then(jwt_claims);
        let auth = claims
            .as_ref()
            .and_then(|c| c.get("https://api.openai.com/auth"));
        let pick = |key: &str| -> Option<String> {
            auth.and_then(|a| a.get(key))
                .or_else(|| claims.as_ref().and_then(|c| c.get(key)))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        Credential {
            access_token: r.access_token,
            refresh_token: r.refresh_token,
            expires_at: now().saturating_add(r.expires_in.unwrap_or(3600)),
            account: claims
                .as_ref()
                .and_then(|c| c.get("email"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            account_id: pick("chatgpt_account_id"),
            plan: pick("chatgpt_plan_type"),
        }
    }
}

async fn codex_exchange(http: &reqwest::Client, code: &str, verifier: &str) -> Result<Credential> {
    // code 换 token 用 form-urlencoded (与官方 CLI 一致)。
    let redirect = provider::codex_redirect_uri();
    let form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect.as_str()),
        ("client_id", provider::CODEX_CLIENT_ID),
        ("code_verifier", verifier),
    ];
    let resp = http
        .post(provider::codex_token_url())
        .form(&form)
        .send()
        .await
        .context("Codex token 换取请求失败")?;
    Ok(json_or_err::<CodexTokenResp>(resp, "Codex token 换取")
        .await?
        .into())
}

async fn codex_refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Credential> {
    // 刷新用 JSON (与官方 CLI 一致)。
    let body = serde_json::json!({
        "client_id": provider::CODEX_CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });
    let resp = http
        .post(provider::codex_token_url())
        .json(&body)
        .send()
        .await
        .context("Codex token 刷新请求失败")?;
    Ok(json_or_err::<CodexTokenResp>(resp, "Codex token 刷新")
        .await?
        .into())
}

pub fn jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---------- 本机回调服务 ----------

struct Callback {
    code: String,
    state: String,
}

/// v4 + v6 都尝试绑定: 浏览器把 `localhost` 解析到哪个都能收到。
fn bind_callback(port: u16) -> Result<Vec<TcpListener>> {
    let mut listeners = Vec::new();
    let mut last_err = None;
    for addr in [
        SocketAddr::from(([127, 0, 0, 1], port)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
    ] {
        match TcpListener::bind(addr) {
            Ok(l) => listeners.push(l),
            Err(e) => last_err = Some(e),
        }
    }
    if listeners.is_empty() {
        let detail = last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".into());
        bail!(
            "无法监听回调端口 {port} ({detail}); 该端口被 OAuth 白名单写死, 请先关闭占用它的进程"
        );
    }
    Ok(listeners)
}

fn wait_for_callback(listeners: Vec<TcpListener>, expect_path: &str) -> Result<Callback> {
    let (tx, rx) = mpsc::channel::<TcpStream>();
    for l in listeners {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for stream in l.incoming().flatten() {
                if tx.send(stream).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("等待授权回调超时");
        }
        let mut stream = rx
            .recv_timeout(remaining)
            .map_err(|_| anyhow!("等待授权回调超时"))?;

        let Some(target) = read_request_target(&mut stream) else {
            continue;
        };
        let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
            respond(&mut stream, 400, "bad request");
            continue;
        };
        if url.path() != expect_path {
            respond(&mut stream, 404, "not found");
            continue;
        }
        let params: HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        if let Some(err) = params.get("error") {
            let desc = params
                .get("error_description")
                .cloned()
                .unwrap_or_else(|| err.clone());
            respond(&mut stream, 200, &format!("授权失败: {desc}"));
            bail!("授权被拒绝: {desc}");
        }
        let Some(code) = params.get("code").cloned() else {
            respond(&mut stream, 400, "回调缺少 code");
            continue;
        };
        respond(&mut stream, 200, "授权成功, 可以关闭此页面。");
        return Ok(Callback {
            code,
            state: params.get("state").cloned().unwrap_or_default(),
        });
    }
}

fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = String::new();
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    reader.read_line(&mut line).ok()?;
    line.split_whitespace().nth(1).map(str::to_string)
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>jj-agentic-proxy</title>\
         <body style=\"font:16px/1.6 system-ui;padding:3rem\">{message}</body>"
    );
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// ---------- 工具 ----------

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn json_or_err<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    what: &str,
) -> Result<T> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{what}失败 ({status}): {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("{what}响应解析失败: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let p = pkce();
        assert_eq!(
            p.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()))
        );
        assert!(p.verifier.len() >= 43 && p.verifier.len() <= 128);
    }

    #[test]
    fn anthropic_url_keeps_scope_colons_unencoded() {
        let url = anthropic_authorize_url(&pkce(), "st");
        assert!(url.contains("scope=org:create_api_key+user:profile+user:inference"));
        assert!(url.contains("code=true"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback"));
    }

    #[test]
    fn codex_url_has_cli_flags() {
        let url = codex_authorize_url(&pkce(), "st");
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("originator=codex_cli_rs"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
    }

    #[test]
    fn jwt_claims_reads_openai_auth_namespace() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_account_id":"acc-1","chatgpt_plan_type":"pro"}}"#,
        );
        let cred: Credential = CodexTokenResp {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            id_token: Some(format!("h.{payload}.s")),
            expires_in: Some(60),
        }
        .into();
        assert_eq!(cred.account.as_deref(), Some("a@b.c"));
        assert_eq!(cred.account_id.as_deref(), Some("acc-1"));
        assert_eq!(cred.plan.as_deref(), Some("pro"));
    }
}
