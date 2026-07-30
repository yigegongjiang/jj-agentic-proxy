//! 透传层: provider 由端口固定 -> 按 path 定上游 -> 注入官方 CLI 凭证与 header -> 流式回传。
//!
//! 只做官方协议要求的最小改写: 客户端看到的行为必须与官方 api key 服务一致。

use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING, UPGRADE,
    USER_AGENT,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

use crate::auth::AuthManager;
use crate::convert;
use crate::provider::{self, Provider};

pub struct App {
    pub auth: Arc<AuthManager>,
    pub http: reqwest::Client,
    /// 单进程一个 session id, 形似一次 CLI 会话。
    pub session_id: String,
}

/// 一个端口 = 一个 provider: 请求打到哪个端口就决定走哪家上游。
#[derive(Clone)]
pub struct Port {
    pub app: Arc<App>,
    pub provider: Provider,
}

/// 该端口对外可用路径 (404 提示与 /health 共用一份)。
pub(crate) fn endpoints(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Anthropic => &[
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/v1/models",
            "/v1/chat/completions",
            "/health",
        ],
        Provider::Codex => &[
            "/v1/responses",
            "/v1/responses/compact",
            "/v1/models",
            "/v1/chat/completions",
            "/backend-api/codex/*",
            "/health",
        ],
    }
}

/// 浏览器里的本地页面也要能直连 -> 全放开; 预检由该层直接应答, 不打到上游。
pub(crate) fn cors() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any)
        .max_age(std::time::Duration::from_secs(86400))
}

pub(crate) async fn health(State(port): State<Port>) -> Response {
    let p = port.provider;
    let login = match port.app.auth.snapshot(p).await {
        Some(c) => json!({
            "logged_in": true,
            "account": c.account,
            "plan": c.plan,
            "expires_at": c.expires_at,
        }),
        None => json!({ "logged_in": false }),
    };
    json_body(
        StatusCode::OK,
        json!({
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "provider": p.key(),
            "port": p.port(),
            "auth": login,
            "endpoints": endpoints(p),
        }),
    )
}

// ---------- 客户端方言 ----------

/// 客户端以为自己在用哪家的 api key -> 决定错误信封与 `/models` 的形状。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    Anthropic,
    OpenAI,
}

impl Dialect {
    /// Anthropic 官方 SDK 一定带 `x-api-key` 或 `anthropic-version`; OpenAI 侧只有 Bearer。
    pub(crate) fn of(h: &HeaderMap) -> Self {
        let anthropic = ["x-api-key", "anthropic-version", "anthropic-beta"]
            .iter()
            .any(|k| h.contains_key(*k));
        if anthropic {
            Dialect::Anthropic
        } else {
            Dialect::OpenAI
        }
    }

    /// `kind`: not_found | authentication | invalid_request | upstream
    pub(crate) fn error(self, status: StatusCode, kind: &str, message: &str) -> Response {
        json_body(status, self.error_body(kind, message))
    }

    fn error_body(self, kind: &str, message: &str) -> Value {
        match self {
            Dialect::Anthropic => {
                let t = match kind {
                    "not_found" => "not_found_error",
                    "authentication" => "authentication_error",
                    "invalid_request" => "invalid_request_error",
                    _ => "api_error",
                };
                json!({ "type": "error", "error": { "type": t, "message": message } })
            }
            Dialect::OpenAI => {
                let (t, code) = match kind {
                    "authentication" => ("invalid_request_error", json!("invalid_api_key")),
                    "not_found" => ("invalid_request_error", json!("not_found")),
                    "invalid_request" => ("invalid_request_error", Value::Null),
                    _ => ("api_error", Value::Null),
                };
                json!({ "error": { "message": message, "type": t, "param": null, "code": code } })
            }
        }
    }
}

// ---------- 请求处理 ----------

pub(crate) async fn handle(
    State(port): State<Port>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let dialect = Dialect::of(&headers);
    let Some(target) = resolve(port.provider, &path, uri.query()) else {
        return dialect.error(
            StatusCode::NOT_FOUND,
            "not_found",
            &not_found_hint(port.provider, &path),
        );
    };

    let (body, plan) = prepare_body(&target, body);
    let started = Instant::now();

    let resp = match upstream(
        &port.app,
        target.provider,
        method.clone(),
        &target.url,
        &headers,
        body,
        plan.upstream_stream,
        dialect,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e,
    };
    let status = resp.status();
    tracing::info!(
        provider = %target.provider,
        %method,
        path = %path,
        status = status.as_u16(),
        aggregated = plan.aggregate_responses,
        elapsed_ms = started.elapsed().as_millis(),
        "proxied"
    );
    // 上游错误形状不一定是官方的 (Codex 后端给 `{"detail":...}`) -> 按方言归一。
    if !status.is_success() {
        return normalize_error(resp, dialect).await;
    }
    // 上游只接受 SSE, 客户端要的是 JSON 对象 -> 本层聚合成官方非流式形状。
    if plan.aggregate_responses {
        return convert::aggregate_responses(resp).await;
    }
    relay(resp)
}

/// 上游错误统一成客户端方言的官方信封。
///
/// 上游已给官方形状时原样回传 (保留 `request-id` / `retry-after` 等 SDK 依赖的头),
/// 否则按方言重裹 -> 客户端的官方 SDK 永远能解析出 message。
pub(crate) async fn normalize_error(resp: reqwest::Response, dialect: Dialect) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = resp.headers().clone();
    let raw = resp.bytes().await.unwrap_or_default();
    let parsed = serde_json::from_slice::<Value>(&raw).ok();

    if parsed
        .as_ref()
        .is_some_and(|v| v.get("error").is_some_and(Value::is_object))
    {
        let mut builder = Response::builder().status(status);
        for (name, value) in &headers {
            if !is_hop_by_hop(name) {
                builder = builder.header(name, value);
            }
        }
        if let Ok(r) = builder.body(Body::from(raw.clone())) {
            return r;
        }
    }

    let message = parsed
        .as_ref()
        .and_then(|v| {
            ["/error/message", "/detail", "/message", "/error"]
                .iter()
                .find_map(|p| v.pointer(p).and_then(Value::as_str).map(str::to_string))
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&raw).into_owned());
    dialect.error(status, error_kind(status), &message)
}

/// 4xx 是请求本身的问题, 5xx 才算上游故障。
pub(crate) fn error_kind(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 | 403 => "authentication",
        404 => "not_found",
        400..=499 => "invalid_request",
        _ => "upstream",
    }
}

/// 带凭证发起上游请求: 到期预判刷新 + 401 强制续期重试一次。
///
/// `Err` 已是可直接返回给客户端的错误响应。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upstream(
    app: &App,
    provider: Provider,
    method: Method,
    url: &str,
    client: &HeaderMap,
    body: Bytes,
    stream: bool,
    dialect: Dialect,
) -> Result<reqwest::Response, Response> {
    let mut rejected_token: Option<String> = None;
    for attempt in 0..2u8 {
        let cred = match app.auth.token(provider, rejected_token.as_deref()).await {
            Ok(c) => c,
            Err(e) => {
                return Err(dialect.error(
                    StatusCode::UNAUTHORIZED,
                    "authentication",
                    &e.to_string(),
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
                    rejected_token = Some(cred.access_token);
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                tracing::error!(provider = %provider, error = %e, "上游请求失败");
                return Err(dialect.error(StatusCode::BAD_GATEWAY, "upstream", &e.to_string()));
            }
        }
    }

    Err(dialect.error(
        StatusCode::UNAUTHORIZED,
        "authentication",
        "上游持续返回 401, 请重新执行 login",
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

/// 走错端口是端口拆分后最常见的误用 -> 直接给出正确端口, 不让客户端猜。
fn not_found_hint(provider: Provider, path: &str) -> String {
    let other = provider.other();
    if resolve(other, path, None).is_some() {
        format!(
            "路径 {path} 属于 {other}, 请改用 http://{}:{}",
            provider::HOST,
            other.port()
        )
    } else {
        format!(
            "未支持的路径 {path}; {provider} 端口 ({}) 可用: {}",
            provider.port(),
            endpoints(provider).join(", ")
        )
    }
}

/// 官方两家 base url 约定不同: Anthropic 到域名根 (SDK 自己拼 `/v1`), OpenAI 到 `/v1`。
/// 客户端两种写法都必须通 -> 去掉前导 `/v1` 后再路由。
pub(crate) fn strip_v1(path: &str) -> &str {
    let mut p = path;
    while let Some(rest) = p.strip_prefix("/v1") {
        if rest.is_empty() || rest.starts_with('/') {
            p = rest;
        } else {
            break;
        }
    }
    if p.is_empty() {
        "/"
    } else {
        p
    }
}

/// provider 由端口固定; path 只决定上游子路径, 不属于该 provider 的路径直接拒绝。
fn resolve(provider: Provider, path: &str, query: Option<&str>) -> Option<Target> {
    let p = strip_v1(path);
    let (base, upstream_path) = match provider {
        Provider::Anthropic => {
            let hit = ["/messages", "/models", "/complete"]
                .iter()
                .any(|pre| sub_path(p, pre).is_some());
            hit.then(|| (provider::ANTHROPIC_UPSTREAM, format!("/v1{p}")))?
        }
        Provider::Codex => {
            let rest = match sub_path(p, "/responses") {
                Some(_) => p.to_string(),
                None => sub_path(path, "/backend-api/codex")?.to_string(),
            };
            (provider::CODEX_UPSTREAM, rest)
        }
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

/// 只在 `prefix` 后是路径边界时命中, 避免 `/messagesfoo` 误匹配。
fn sub_path<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

// ---------- body: 上游协议硬要求 ----------

/// 上游与客户端期望的差异, 由本层补齐。
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    /// 发给上游时是否按 SSE 请求
    upstream_stream: bool,
    /// 上游 SSE 需聚合成官方非流式 response 对象
    aggregate_responses: bool,
}

/// 返回 (待发送 body, 差异补齐计划)。非 JSON body 原样透传。
fn prepare_body(target: &Target, body: Bytes) -> (Bytes, Plan) {
    let plain = Plan {
        upstream_stream: false,
        aggregate_responses: false,
    };
    if body.is_empty() {
        return (body, plain);
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return (body, plain);
    };
    let Some(obj) = value.as_object_mut() else {
        return (body, plain);
    };
    let client_stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let (changed, plan) = match target.provider {
        // OAuth 凭证只被授权用于 Claude Code, system 必须带 CLI 前缀。
        Provider::Anthropic if target.upstream_path.starts_with("/v1/messages") => (
            inject_claude_code_prefix(obj),
            Plan {
                upstream_stream: client_stream,
                aggregate_responses: false,
            },
        ),
        // /codex/responses 只接受 SSE + 不落库 + instructions 必填 + input 必须是数组。
        Provider::Codex if target.upstream_path == "/responses" => (
            normalize_codex(obj),
            Plan {
                upstream_stream: true,
                aggregate_responses: !client_stream,
            },
        ),
        _ => (
            false,
            Plan {
                upstream_stream: client_stream,
                aggregate_responses: false,
            },
        ),
    };

    if changed {
        match serde_json::to_vec(&value) {
            Ok(bytes) => (Bytes::from(bytes), plan),
            Err(_) => (body, plan),
        }
    } else {
        (body, plan)
    }
}

pub(crate) fn inject_claude_code_prefix(obj: &mut Map<String, Value>) -> bool {
    // 不带 cache_control: 客户端自己的 breakpoint 才是权威。
    // 注入一个 5m 块会插在客户端的 1h 块之前 -> 上游按 ttl 顺序硬拒 (400), 且白占一个 breakpoint。
    let prefix = json!({
        "type": "text",
        "text": provider::CLAUDE_CODE_SYSTEM_PREFIX,
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

/// 官方 Responses API 有、ChatGPT 订阅后端没有的纯标注参数: 丢掉不改变结果, 留着直接 400。
/// 有语义的 (temperature / previous_response_id / background / truncation / service_tier)
/// 不能静默丢 -> 交给上游报错, 由错误信封归一后按官方形状回给客户端。
const CODEX_DROP_KEYS: [&str; 5] = [
    "metadata",
    "user",
    "safety_identifier",
    "max_output_tokens",
    "max_tokens",
];

fn normalize_codex(obj: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    // 上游硬拒 stream:false ("Stream must be set to true")。
    if obj.get("stream") != Some(&Value::Bool(true)) {
        obj.insert("stream".into(), json!(true));
        changed = true;
    }
    // 官方默认 store:true, 上游只接受 false; 后端本就不落库 -> 强制改写无语义损失。
    if obj.get("store") != Some(&Value::Bool(false)) {
        obj.insert("store".into(), json!(false));
        changed = true;
    }
    if !obj.contains_key("instructions") {
        obj.insert("instructions".into(), json!(""));
        changed = true;
    }
    for key in CODEX_DROP_KEYS {
        if obj.remove(key).is_some() {
            changed = true;
        }
    }
    // 上游硬拒字符串 input ("Input must be a list")。
    if let Some(Value::String(text)) = obj.get("input") {
        let item = json!([{
            "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        }]);
        obj.insert("input".into(), item);
        changed = true;
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
pub(crate) fn relay(resp: reqwest::Response) -> Response {
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
                json!({ "error": { "type": "api_error", "message": e.to_string() } }),
            )
        })
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    // 上游的 CORS 头必须丢掉: 本机代理自己那份才是权威, 否则浏览器看到两份。
    name.as_str().starts_with("access-control-")
        || name == CONNECTION
        || name == TRANSFER_ENCODING
        || name == UPGRADE
        || name == CONTENT_LENGTH
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

    fn resolved(provider: Provider, path: &str) -> Target {
        resolve(provider, path, None).expect("路径应可解析")
    }

    #[test]
    fn both_base_url_conventions_route_the_same() {
        // Anthropic SDK: base = 域名根 -> /v1/messages
        assert_eq!(
            resolved(Provider::Anthropic, "/v1/messages").url,
            "https://api.anthropic.com/v1/messages"
        );
        // OpenAI SDK: base = .../v1 -> /chat 之外的路径不带 /v1
        assert_eq!(
            resolved(Provider::Codex, "/responses").url,
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            resolved(Provider::Anthropic, "/models").url,
            "https://api.anthropic.com/v1/models"
        );
        // base 误写成 .../v1 又被 SDK 再拼一次
        assert_eq!(
            resolved(Provider::Anthropic, "/v1/v1/messages").url,
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn anthropic_paths_keep_shape() {
        let t = resolved(Provider::Anthropic, "/v1/messages/count_tokens");
        assert_eq!(t.provider, Provider::Anthropic);
        assert_eq!(t.url, "https://api.anthropic.com/v1/messages/count_tokens");
        assert_eq!(
            resolved(Provider::Anthropic, "/v1/models/claude-opus-5").url,
            "https://api.anthropic.com/v1/models/claude-opus-5"
        );
    }

    #[test]
    fn codex_paths_drop_v1_prefix() {
        let t = resolved(Provider::Codex, "/v1/responses");
        assert_eq!(t.provider, Provider::Codex);
        assert_eq!(t.url, "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(t.upstream_path, "/responses");

        assert_eq!(
            resolved(Provider::Codex, "/v1/responses/compact").url,
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
        assert_eq!(
            resolved(Provider::Codex, "/backend-api/codex/usage").url,
            "https://chatgpt.com/backend-api/codex/usage"
        );
    }

    #[test]
    fn each_port_only_serves_its_own_provider() {
        // 一个端口 = 一个 provider: 另一家的路径在本端口不存在
        assert!(resolve(Provider::Anthropic, "/v1/responses", None).is_none());
        assert!(resolve(Provider::Codex, "/v1/messages", None).is_none());
        assert!(resolve(Provider::Anthropic, "/backend-api/codex/usage", None).is_none());
    }

    #[test]
    fn wrong_port_hint_points_at_the_right_one() {
        let hint = not_found_hint(Provider::Anthropic, "/v1/responses");
        assert!(hint.contains("codex"), "{hint}");
        assert!(hint.contains("127.0.0.1:10010"), "{hint}");

        let hint = not_found_hint(Provider::Codex, "/v1/messages");
        assert!(hint.contains("127.0.0.1:10011"), "{hint}");

        // 两家都不认的路径 -> 列本端口可用路径
        let hint = not_found_hint(Provider::Codex, "/embeddings");
        assert!(hint.contains("/v1/responses"), "{hint}");
    }

    #[test]
    fn query_is_preserved_and_unknown_path_rejected() {
        let t = resolve(Provider::Anthropic, "/v1/models", Some("limit=5")).unwrap();
        assert_eq!(t.url, "https://api.anthropic.com/v1/models?limit=5");
        // chat/completions 由 server 层接走, 不进透传
        assert!(resolve(Provider::Anthropic, "/v1/chat/completions", None).is_none());
        assert!(resolve(Provider::Codex, "/v1/chat/completions", None).is_none());
        assert!(resolve(Provider::Codex, "/embeddings", None).is_none());
        assert!(resolve(Provider::Anthropic, "/v1/messagesfoo", None).is_none());
    }

    #[test]
    fn dialect_follows_api_key_style() {
        let mut anthropic = HeaderMap::new();
        anthropic.insert("x-api-key", HeaderValue::from_static("sk-x"));
        assert_eq!(Dialect::of(&anthropic), Dialect::Anthropic);

        let mut openai = HeaderMap::new();
        openai.insert(AUTHORIZATION, HeaderValue::from_static("Bearer sk-x"));
        assert_eq!(Dialect::of(&openai), Dialect::OpenAI);
        assert_eq!(Dialect::of(&HeaderMap::new()), Dialect::OpenAI);
    }

    #[test]
    fn claude_code_prefix_injected_once() {
        let t = resolved(Provider::Anthropic, "/v1/messages");

        let (body, plan) = prepare_body(&t, Bytes::from(r#"{"model":"m","stream":true}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(plan.upstream_stream);
        assert!(!plan.aggregate_responses);
        assert_eq!(
            v["system"][0]["text"].as_str().unwrap(),
            provider::CLAUDE_CODE_SYSTEM_PREFIX
        );

        // 已带前缀 -> 不重复注入
        let (again, _) = prepare_body(&t, body);
        let v: Value = serde_json::from_slice(&again).unwrap();
        assert_eq!(v["system"].as_array().unwrap().len(), 1);
    }

    /// 客户端 (Claude Code CLI / Agent SDK) 用 1h 缓存时, 注入块若带默认 5m 会被上游按 ttl 顺序拒绝。
    #[test]
    fn injected_prefix_carries_no_cache_control() {
        let t = resolved(Provider::Anthropic, "/v1/messages");
        let client = r#"{"system":[
            {"type":"text","text":"billing"},
            {"type":"text","text":"big prompt","cache_control":{"type":"ephemeral","ttl":"1h"}}
        ]}"#;
        let (body, _) = prepare_body(&t, Bytes::from(client));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["system"][0]["text"].as_str().unwrap(),
            provider::CLAUDE_CODE_SYSTEM_PREFIX
        );
        assert!(v["system"][0].get("cache_control").is_none());
        // 客户端自己的 breakpoint 原样保留
        assert_eq!(v["system"][2]["cache_control"]["ttl"], json!("1h"));
    }

    #[test]
    fn string_system_is_promoted_to_blocks() {
        let t = resolved(Provider::Anthropic, "/v1/messages");
        let (body, _) = prepare_body(&t, Bytes::from(r#"{"system":"be brief"}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["system"][0]["text"].as_str().unwrap(),
            provider::CLAUDE_CODE_SYSTEM_PREFIX
        );
        assert_eq!(v["system"][1]["text"].as_str().unwrap(), "be brief");
    }

    #[test]
    fn codex_body_meets_upstream_hard_requirements() {
        let t = resolved(Provider::Codex, "/v1/responses");
        let (body, plan) = prepare_body(&t, Bytes::from(r#"{"model":"m","input":"hi"}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        // 上游只接受 SSE + 数组 input; 客户端没要流式 -> 由本层聚合
        assert!(plan.upstream_stream);
        assert!(plan.aggregate_responses);
        assert_eq!(v["stream"], json!(true));
        assert_eq!(v["store"], json!(false));
        assert_eq!(v["instructions"], json!(""));
        assert_eq!(v["input"][0]["content"][0]["text"], json!("hi"));

        // 客户端显式 stream:false 也要送 true 上游, 但下游给 JSON
        let (body, plan) = prepare_body(&t, Bytes::from(r#"{"stream":false}"#));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["stream"], json!(true));
        assert!(plan.aggregate_responses);

        // 客户端要流式 -> 原样 SSE 回传
        let (_, plan) = prepare_body(&t, Bytes::from(r#"{"stream":true}"#));
        assert!(!plan.aggregate_responses);
    }

    /// 官方 Responses API 的默认 store:true 与纯标注参数, 上游一律 400 -> 本层抹平。
    #[test]
    fn codex_drops_params_upstream_rejects() {
        let t = resolved(Provider::Codex, "/v1/responses");
        let raw = r#"{"model":"m","input":[],"store":true,"max_output_tokens":64,
            "metadata":{"a":"b"},"user":"u","safety_identifier":"s","temperature":1}"#;
        let (body, _) = prepare_body(&t, Bytes::from(raw));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["store"], json!(false));
        for k in CODEX_DROP_KEYS {
            assert!(v.get(k).is_none(), "{k} 应被丢弃");
        }
        // 有语义的参数不静默丢: 交给上游报错
        assert_eq!(v["temperature"], json!(1));
    }

    #[test]
    fn compact_subresource_is_untouched() {
        let t = resolved(Provider::Codex, "/v1/responses/compact");
        let raw = r#"{"model":"m"}"#;
        let (body, plan) = prepare_body(&t, Bytes::from(raw));
        assert_eq!(body, Bytes::from(raw));
        assert!(!plan.aggregate_responses);
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
    fn client_api_key_is_never_forwarded() {
        let mut client = HeaderMap::new();
        client.insert("x-api-key", HeaderValue::from_static("sk-client"));
        let h = anthropic_headers("tok", &client, false);
        assert!(h.get("x-api-key").is_none());
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer tok");
    }

    #[test]
    fn non_json_body_passes_through() {
        let t = resolved(Provider::Anthropic, "/v1/messages");
        let raw = Bytes::from_static(b"not json");
        let (body, plan) = prepare_body(&t, raw.clone());
        assert_eq!(body, raw);
        assert!(!plan.upstream_stream);
    }

    #[test]
    fn content_encoding_is_end_to_end() {
        assert!(!is_hop_by_hop(&HeaderName::from_static("content-encoding")));
        assert!(is_hop_by_hop(&CONNECTION));
        assert!(is_hop_by_hop(&CONTENT_LENGTH));
    }

    /// Codex 后端的 `{"detail":...}` 不是官方形状 -> 官方 SDK 解析不出 message。
    #[tokio::test]
    async fn non_official_upstream_error_is_rewrapped() {
        let resp = stub(400, r#"{"detail":"Store must be set to false"}"#).await;
        let out = normalize_error(resp, Dialect::OpenAI).await;
        assert_eq!(out.status(), StatusCode::BAD_REQUEST);
        let v = body_json(out).await;
        assert_eq!(v["error"]["message"], json!("Store must be set to false"));
        assert_eq!(v["error"]["type"], json!("invalid_request_error"));
    }

    /// 上游已是官方信封 -> 原样回传, 不丢 request_id 之类的字段。
    #[tokio::test]
    async fn official_upstream_error_is_passed_through() {
        let raw = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"},"request_id":"req_1"}"#;
        let resp = stub(429, raw).await;
        let out = normalize_error(resp, Dialect::Anthropic).await;
        assert_eq!(out.status(), StatusCode::TOO_MANY_REQUESTS);
        let v = body_json(out).await;
        assert_eq!(v["request_id"], json!("req_1"));
        assert_eq!(v["error"]["type"], json!("rate_limit_error"));
    }

    async fn stub(status: u16, body: &str) -> reqwest::Response {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0u8; 1024]);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        reqwest::Client::new()
            .get(format!("http://{addr}"))
            .send()
            .await
            .unwrap()
    }

    async fn body_json(r: Response) -> Value {
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    #[test]
    fn error_envelopes_match_each_official_shape() {
        assert_eq!(
            Dialect::Anthropic.error_body("not_found", "no"),
            json!({ "type": "error", "error": { "type": "not_found_error", "message": "no" } })
        );
        assert_eq!(
            Dialect::OpenAI.error_body("authentication", "no"),
            json!({ "error": {
                "message": "no", "type": "invalid_request_error",
                "param": null, "code": "invalid_api_key",
            }})
        );
    }
}
