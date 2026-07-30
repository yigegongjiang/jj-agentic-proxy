//! 透传层: 按 path 定上游 -> 注入官方 CLI 凭证与 header -> 流式回传。
//!
//! 只做上游协议硬要求的最小改写, 不做任何格式转换。

use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
    TRANSFER_ENCODING, UPGRADE, USER_AGENT,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::{json, Map, Value};

use crate::auth::AuthManager;
use crate::provider::{self, Provider};

pub struct App {
    pub auth: Arc<AuthManager>,
    pub http: reqwest::Client,
    /// 单进程一个 session id, 形似一次 CLI 会话。
    pub session_id: String,
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/health", any(health))
        .fallback(any(handle))
        .layer(DefaultBodyLimit::disable())
        .with_state(app)
}

pub(crate) async fn health(State(app): State<Arc<App>>) -> Response {
    let mut providers = Map::new();
    for p in Provider::ALL {
        let entry = match app.auth.snapshot(p).await {
            Some(c) => json!({
                "logged_in": true,
                "account": c.account,
                "plan": c.plan,
                "expires_at": c.expires_at,
            }),
            None => json!({ "logged_in": false }),
        };
        providers.insert(p.key().to_string(), entry);
    }
    json_body(
        StatusCode::OK,
        json!({
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "endpoints": {
                "anthropic": ["/v1/messages", "/v1/messages/count_tokens", "/v1/models"],
                "codex": ["/v1/responses", "/v1/responses/compact"],
                "compat_only": ["/v1/chat/completions", "/v1/models"],
            },
            "providers": providers,
        }),
    )
}

pub(crate) async fn handle(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let Some(target) = resolve(&path, uri.query()) else {
        return json_body(
            StatusCode::NOT_FOUND,
            json!({
                "error": {
                    "type": "not_found",
                    "message": format!("未支持的路径 {path}; 可用: /v1/messages, /v1/messages/count_tokens, /v1/models (Anthropic), /v1/responses (Codex), /v1/chat/completions (仅兼容端口)"),
                }
            }),
        );
    };

    let (body, stream) = prepare_body(&target, body);
    let started = Instant::now();

    let resp = match upstream(
        &app,
        target.provider,
        method.clone(),
        &target.url,
        &headers,
        body,
        stream,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e,
    };
    tracing::info!(
        provider = %target.provider,
        %method,
        path = %path,
        status = resp.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "proxied"
    );
    relay(resp)
}

/// 带凭证发起上游请求: 到期预判刷新 + 401 强制续期重试一次。
///
/// `Err` 已是可直接返回给客户端的错误响应。
pub(crate) async fn upstream(
    app: &App,
    provider: Provider,
    method: Method,
    url: &str,
    client: &HeaderMap,
    body: Bytes,
    stream: bool,
) -> Result<reqwest::Response, Response> {
    let mut force_refresh = false;
    for attempt in 0..2u8 {
        let cred = match app.auth.token(provider, force_refresh).await {
            Ok(c) => c,
            Err(e) => {
                return Err(json_body(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": { "type": "authentication_error", "message": e.to_string() } }),
                ))
            }
        };

        let upstream_headers = match provider {
            Provider::Anthropic => anthropic_headers(&cred.access_token, client, stream),
            Provider::Codex => codex_headers(app, &cred, client, stream),
        };

        match app
            .http
            .request(method.clone(), url)
            .headers(upstream_headers)
            .body(body.clone())
            .send()
            .await
        {
            Ok(resp) => {
                // 401 = token 失效: 强制续期后重试一次。
                if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                    tracing::warn!(provider = %provider, "上游 401, 强制刷新 token 重试");
                    force_refresh = true;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                tracing::error!(provider = %provider, error = %e, "上游请求失败");
                return Err(json_body(
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": { "type": "upstream_error", "message": e.to_string() } }),
                ));
            }
        }
    }

    Err(json_body(
        StatusCode::UNAUTHORIZED,
        json!({ "error": { "type": "authentication_error", "message": "上游持续返回 401, 请重新执行 login" } }),
    ))
}

// ---------- 路由 ----------

#[derive(Debug, PartialEq, Eq)]
struct Target {
    provider: Provider,
    url: String,
    /// 上游路径 (不含 host / query), 用于判断是否需要 body 规范化。
    upstream_path: String,
}

/// path 决定协议与上游: 客户端选 model -> 决定协议 -> 决定 path。
fn resolve(path: &str, query: Option<&str>) -> Option<Target> {
    let (provider, base, upstream_path) =
        if matches_prefix(path, "/v1/messages") || matches_prefix(path, "/v1/models") {
            (
                Provider::Anthropic,
                provider::ANTHROPIC_UPSTREAM,
                path.to_string(),
            )
        } else if let Some(rest) = sub_path(path, "/v1/responses") {
            (
                Provider::Codex,
                provider::CODEX_UPSTREAM,
                format!("/responses{rest}"),
            )
        } else {
            let rest = sub_path(path, "/backend-api/codex")?;
            (Provider::Codex, provider::CODEX_UPSTREAM, rest.to_string())
        };

    let mut url = format!("{base}{upstream_path}");
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    Some(Target {
        provider,
        url,
        upstream_path,
    })
}

fn matches_prefix(path: &str, prefix: &str) -> bool {
    sub_path(path, prefix).is_some()
}

/// 只在 `prefix` 后是路径边界时命中, 避免 `/v1/messagesfoo` 误匹配。
fn sub_path<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

// ---------- body: 上游协议硬要求 ----------

/// 返回 (待发送 body, 是否 SSE)。非 JSON body 原样透传。
fn prepare_body(target: &Target, body: Bytes) -> (Bytes, bool) {
    if body.is_empty() {
        return (body, false);
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return (body, false);
    };
    let Some(obj) = value.as_object_mut() else {
        return (body, false);
    };

    let changed = match target.provider {
        // OAuth 凭证只被授权用于 Claude Code, system 必须带 CLI 前缀。
        Provider::Anthropic if target.upstream_path.starts_with("/v1/messages") => {
            inject_claude_code_prefix(obj)
        }
        // /codex/responses 强制 SSE + 不落库 + instructions 必填; compact 子资源另有更窄的 envelope。
        Provider::Codex if target.upstream_path == "/responses" => normalize_codex(obj),
        _ => false,
    };

    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if changed {
        match serde_json::to_vec(&value) {
            Ok(bytes) => (Bytes::from(bytes), stream),
            Err(_) => (body, stream),
        }
    } else {
        (body, stream)
    }
}

pub(crate) fn inject_claude_code_prefix(obj: &mut Map<String, Value>) -> bool {
    let prefix = json!({
        "type": "text",
        "text": provider::CLAUDE_CODE_SYSTEM_PREFIX,
        "cache_control": { "type": "ephemeral" },
    });
    let has_prefix = |v: &Value| {
        v.get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains(provider::CLAUDE_CODE_SYSTEM_PREFIX))
    };

    match obj.remove("system") {
        None => {
            obj.insert("system".into(), json!([prefix]));
            true
        }
        Some(Value::String(s)) => {
            if s.contains(provider::CLAUDE_CODE_SYSTEM_PREFIX) {
                obj.insert("system".into(), Value::String(s));
                false
            } else {
                obj.insert(
                    "system".into(),
                    json!([prefix, { "type": "text", "text": s }]),
                );
                true
            }
        }
        Some(Value::Array(mut blocks)) => {
            let hit = blocks.iter().any(has_prefix);
            if !hit {
                blocks.insert(0, prefix);
            }
            obj.insert("system".into(), Value::Array(blocks));
            !hit
        }
        Some(other) => {
            obj.insert("system".into(), other);
            false
        }
    }
}

fn normalize_codex(obj: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    for (key, default) in [
        ("stream", Value::Bool(true)),
        ("store", Value::Bool(false)),
        ("instructions", Value::String(String::new())),
    ] {
        if !obj.contains_key(key) {
            obj.insert(key.into(), default);
            changed = true;
        }
    }
    changed
}

// ---------- header 注入 ----------

fn anthropic_headers(token: &str, client: &HeaderMap, stream: bool) -> HeaderMap {
    let mut h = HeaderMap::new();
    set(&mut h, AUTHORIZATION, &format!("Bearer {token}"));
    set(&mut h, USER_AGENT, &provider::claude_user_agent());
    set(&mut h, CONTENT_TYPE, "application/json");
    set(&mut h, ACCEPT, accept_for(stream));
    set_name(&mut h, "anthropic-version", provider::ANTHROPIC_API_VERSION);
    set_name(&mut h, "x-app", "cli");
    set_name(&mut h, "anthropic-dangerous-direct-browser-access", "true");

    // 客户端自带的 anthropic-* 语义头透传 (beta / version / 其他能力开关)。
    for (name, value) in client {
        if name.as_str().starts_with("anthropic-") {
            h.insert(name.clone(), value.clone());
        }
    }
    ensure_oauth_beta(&mut h);
    h
}

/// `anthropic-beta` 必须含 oauth-2025-04-20, 否则 CLI 凭证不被接受。
fn ensure_oauth_beta(h: &mut HeaderMap) {
    let current = h
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut betas: Vec<&str> = Vec::new();
    if !current.contains(provider::ANTHROPIC_OAUTH_BETA) {
        betas.push(provider::ANTHROPIC_OAUTH_BETA);
    }
    for b in current.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !betas.contains(&b) {
            betas.push(b);
        }
    }
    set_name(h, "anthropic-beta", &betas.join(","));
}

fn codex_headers(
    app: &App,
    cred: &crate::store::Credential,
    client: &HeaderMap,
    stream: bool,
) -> HeaderMap {
    let mut h = HeaderMap::new();
    set(
        &mut h,
        AUTHORIZATION,
        &format!("Bearer {}", cred.access_token),
    );
    set(&mut h, USER_AGENT, &provider::codex_user_agent());
    set(&mut h, CONTENT_TYPE, "application/json");
    set(&mut h, ACCEPT, accept_for(stream));
    set_name(&mut h, "originator", provider::CODEX_ORIGINATOR);
    set_name(&mut h, "version", &provider::codex_cli_version());
    if let Some(id) = cred.account_id.as_deref() {
        set_name(&mut h, "chatgpt-account-id", id);
    }
    set_name(&mut h, "session_id", &app.session_id);

    for name in ["session_id", "conversation_id", "openai-beta"] {
        if let Some(v) = client.get(name) {
            h.insert(HeaderName::from_static(name), v.clone());
        }
    }
    h
}

fn accept_for(stream: bool) -> &'static str {
    if stream {
        "text/event-stream"
    } else {
        "application/json"
    }
}

fn set(h: &mut HeaderMap, name: HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        h.insert(name, v);
    }
}

fn set_name(h: &mut HeaderMap, name: &'static str, value: &str) {
    set(h, HeaderName::from_static(name), value);
}

// ---------- 回传 ----------

/// 逐块转发上游字节流, 不缓冲 -> SSE 首字延迟与官方 CLI 一致。
fn relay(resp: reqwest::Response) -> Response {
    let mut builder = Response::builder().status(resp.status());
    for (name, value) in resp.headers() {
        if is_hop_by_hop(name) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|e| {
            json_body(
                StatusCode::BAD_GATEWAY,
                json!({ "error": { "type": "upstream_error", "message": e.to_string() } }),
            )
        })
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    name == CONNECTION
        || name == TRANSFER_ENCODING
        || name == UPGRADE
        || name == CONTENT_LENGTH
        || name == CONTENT_ENCODING
        || name == "keep-alive"
        || name == "proxy-authenticate"
        || name == "proxy-authorization"
        || name == "te"
        || name == "trailer"
}

pub(crate) fn json_body(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(path: &str) -> Target {
        resolve(path, None).expect("路径应可解析")
    }

    #[test]
    fn anthropic_paths_keep_shape() {
        let t = resolved("/v1/messages");
        assert_eq!(t.provider, Provider::Anthropic);
        assert_eq!(t.url, "https://api.anthropic.com/v1/messages");

        let t = resolved("/v1/messages/count_tokens");
        assert_eq!(t.url, "https://api.anthropic.com/v1/messages/count_tokens");
        assert_eq!(resolved("/v1/models").provider, Provider::Anthropic);
    }

    #[test]
    fn codex_paths_drop_v1_prefix() {
        let t = resolved("/v1/responses");
        assert_eq!(t.provider, Provider::Codex);
        assert_eq!(t.url, "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(t.upstream_path, "/responses");

        let t = resolved("/v1/responses/compact");
        assert_eq!(
            t.url,
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );

        let t = resolved("/backend-api/codex/responses");
        assert_eq!(t.url, "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn query_is_preserved_and_unknown_path_rejected() {
        let t = resolve("/v1/models", Some("limit=5")).unwrap();
        assert_eq!(t.url, "https://api.anthropic.com/v1/models?limit=5");
        assert!(resolve("/v1/chat/completions", None).is_none());
        assert!(resolve("/v1/messagesfoo", None).is_none());
    }

    #[test]
    fn claude_code_prefix_injected_once() {
        let t = resolved("/v1/messages");

        let (body, stream) = prepare_body(&t, Bytes::from(r#"{"model":"m","stream":true}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(stream);
        assert_eq!(
            v["system"][0]["text"].as_str().unwrap(),
            provider::CLAUDE_CODE_SYSTEM_PREFIX
        );

        // 已带前缀 -> 不重复注入
        let (again, _) = prepare_body(&t, body);
        let v: Value = serde_json::from_slice(&again).unwrap();
        assert_eq!(v["system"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn string_system_is_promoted_to_blocks() {
        let t = resolved("/v1/messages");
        let (body, _) = prepare_body(&t, Bytes::from(r#"{"system":"be brief"}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["system"][0]["text"].as_str().unwrap(),
            provider::CLAUDE_CODE_SYSTEM_PREFIX
        );
        assert_eq!(v["system"][1]["text"].as_str().unwrap(), "be brief");
    }

    #[test]
    fn codex_body_gets_protocol_defaults() {
        let t = resolved("/v1/responses");
        let (body, stream) = prepare_body(&t, Bytes::from(r#"{"model":"gpt-5.3-codex"}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(stream);
        assert_eq!(v["stream"], json!(true));
        assert_eq!(v["store"], json!(false));
        assert_eq!(v["instructions"], json!(""));

        // 显式意图不被覆盖
        let (body, _) = prepare_body(&t, Bytes::from(r#"{"stream":false,"store":true}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["stream"], json!(false));
        assert_eq!(v["store"], json!(true));
    }

    #[test]
    fn compact_subresource_is_untouched() {
        let t = resolved("/v1/responses/compact");
        let raw = r#"{"model":"m"}"#;
        let (body, _) = prepare_body(&t, Bytes::from(raw));
        assert_eq!(body, Bytes::from(raw));
    }

    #[test]
    fn oauth_beta_is_prepended_and_deduped() {
        let mut client = HeaderMap::new();
        client.insert(
            "anthropic-beta",
            HeaderValue::from_static("custom-beta,custom-beta"),
        );
        let h = anthropic_headers("tok", &client, true);
        assert_eq!(
            h.get("anthropic-beta").unwrap(),
            "oauth-2025-04-20,custom-beta"
        );
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer tok");
        assert_eq!(h.get(ACCEPT).unwrap(), "text/event-stream");
        assert_eq!(h.get("x-app").unwrap(), "cli");

        let h = anthropic_headers("tok", &HeaderMap::new(), false);
        assert_eq!(h.get("anthropic-beta").unwrap(), "oauth-2025-04-20");
        assert_eq!(h.get(ACCEPT).unwrap(), "application/json");
    }

    #[test]
    fn non_json_body_passes_through() {
        let t = resolved("/v1/messages");
        let raw = Bytes::from_static(b"not json");
        let (body, stream) = prepare_body(&t, raw.clone());
        assert_eq!(body, raw);
        assert!(!stream);
    }
}
