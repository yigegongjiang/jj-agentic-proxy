//! api key 兼容端口: 业务按官方 api key 方式调用 -> proxy 转成 CLI (OAuth) 渠道。
//!
//! 与原生端口的区别只有两点: 认可 api key 风格鉴权 + 提供 Chat Completions / 合并模型列表。
//! 其余路径原样落回原生透传, 所以业务把 base url 全指到本端口即可。

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use serde_json::{json, Value};

use crate::convert;
use crate::provider::{self, Provider};
use crate::proxy::{self, json_body, App};
use crate::store::now;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/health", any(proxy::health))
        .fallback(any(passthrough))
        .layer(DefaultBodyLimit::disable())
        .with_state(app)
}

/// 非 Chat Completions 的路径 = 官方原生协议, 直接复用透传层。
async fn passthrough(
    state: State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = denied(&headers) {
        return r;
    }
    proxy::handle(state, method, uri, headers, body).await
}

// ---------- Chat Completions ----------

async fn chat_completions(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = denied(&headers) {
        return r;
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "请求体不是合法 JSON",
        );
    };
    let Some((p, model)) = req
        .get("model")
        .and_then(Value::as_str)
        .and_then(convert::route)
    else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model 必填; 可用取值见 GET /v1/models",
        );
    };
    let model = model.to_string();
    let want_stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let include_usage = req
        .pointer("/stream_options/include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let url = match p {
        Provider::Anthropic => format!("{}/v1/messages", provider::ANTHROPIC_UPSTREAM),
        Provider::Codex => format!("{}/responses", provider::CODEX_UPSTREAM),
    };
    let upstream_body = match serde_json::to_vec(&convert::request(p, &req, &model)) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "proxy_error",
                &e.to_string(),
            )
        }
    };

    let started = Instant::now();
    // 上游一律 SSE: 客户端要非流式时由转换层聚合。
    let resp = match proxy::upstream(
        &app,
        p,
        Method::POST,
        &url,
        &HeaderMap::new(),
        upstream_body,
        true,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e,
    };

    let status = resp.status();
    tracing::info!(
        provider = %p,
        model = %model,
        stream = want_stream,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "chat.completions"
    );
    if !status.is_success() {
        return upstream_error(resp).await;
    }
    if want_stream {
        convert::stream_response(p, model, resp, include_usage)
    } else {
        convert::aggregate_response(p, model, resp).await
    }
}

/// 上游错误统一裹成 OpenAI 错误信封, 保留原始文案。
async fn upstream_error(resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let raw = resp.bytes().await.unwrap_or_default();
    let msg = serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|v| {
            ["/error/message", "/detail", "/message", "/error"]
                .iter()
                .find_map(|p| v.pointer(p).and_then(Value::as_str).map(str::to_string))
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&raw).into_owned());
    json_body(
        status,
        json!({ "error": { "message": msg, "type": "upstream_error", "code": status.as_u16() } }),
    )
}

// ---------- 模型列表 ----------

async fn models(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(r) = denied(&headers) {
        return r;
    }
    let (anthropic, codex) = tokio::join!(anthropic_models(&app), codex_models(&app));
    let data: Vec<Value> = anthropic.into_iter().chain(codex).collect();
    json_body(StatusCode::OK, json!({ "object": "list", "data": data }))
}

async fn anthropic_models(app: &App) -> Vec<Value> {
    let url = format!("{}/v1/models?limit=1000", provider::ANTHROPIC_UPSTREAM);
    let Some(v) = fetch_json(app, Provider::Anthropic, &url).await else {
        return vec![];
    };
    entries(&v, "data", "id", "anthropic")
}

async fn codex_models(app: &App) -> Vec<Value> {
    let url = format!(
        "{}/models?client_version={}",
        provider::CODEX_UPSTREAM,
        provider::codex_cli_version()
    );
    let Some(v) = fetch_json(app, Provider::Codex, &url).await else {
        return vec![];
    };
    entries(&v, "models", "slug", "openai")
}

fn entries(v: &Value, list_key: &str, id_key: &str, owner: &str) -> Vec<Value> {
    v.get(list_key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get(id_key).and_then(Value::as_str))
                .map(|id| {
                    json!({
                        "id": id, "object": "model", "created": now(), "owned_by": owner,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 单家取不到不影响另一家 -> 列表尽力而为。
async fn fetch_json(app: &App, p: Provider, url: &str) -> Option<Value> {
    match proxy::upstream(
        app,
        p,
        Method::GET,
        url,
        &HeaderMap::new(),
        Bytes::new(),
        false,
    )
    .await
    {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
        Ok(r) => {
            tracing::warn!(provider = %p, status = r.status().as_u16(), "模型列表获取失败");
            None
        }
        Err(_) => {
            tracing::warn!(provider = %p, "模型列表跳过: 未登录或上游异常");
            None
        }
    }
}

// ---------- api key ----------

/// 未设 `JJ_PROXY_API_KEY` 时接受任意 key (本机 loopback 已是边界)。
fn expected_key() -> Option<&'static str> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| {
        std::env::var("JJ_PROXY_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
    })
    .as_deref()
}

fn denied(h: &HeaderMap) -> Option<Response> {
    let expect = expected_key()?;
    let got = h
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            h.get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).trim())
        });
    (got != Some(expect)).then(|| {
        error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "api key 无效",
        )
    })
}

fn error(status: StatusCode, kind: &str, message: &str) -> Response {
    json_body(
        status,
        json!({ "error": { "message": message, "type": kind, "code": status.as_u16() } }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_entries_are_openai_shaped() {
        let v = json!({"models":[{"slug":"gpt-5.6-sol"},{"noslug":1}]});
        let out = entries(&v, "models", "slug", "openai");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], json!("gpt-5.6-sol"));
        assert_eq!(out[0]["object"], json!("model"));
        assert_eq!(out[0]["owned_by"], json!("openai"));
        assert!(entries(&json!({}), "data", "id", "anthropic").is_empty());
    }

    #[test]
    fn any_key_accepted_without_env() {
        // 测试进程未设 JJ_PROXY_API_KEY -> 放行
        assert!(denied(&HeaderMap::new()).is_none());
    }
}
