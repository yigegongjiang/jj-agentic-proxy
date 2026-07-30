//! 端口层: 一个端口一个 provider, 业务按官方 api key 方式调用 -> proxy 换成 CLI (OAuth) 渠道。
//!
//! 客户端全程以为自己在直连官方付费 api, 因此:
//! - 两家 base url 约定 (域名根 / `.../v1`) 都要通 -> 路径先归一
//! - `/models` 按客户端方言给官方形状
//! - 自产错误也按方言裹官方信封
//!
//! 本层只接 Chat Completions 与模型列表, 其余路径落回原生透传。

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use serde_json::{json, Value};

use crate::convert;
use crate::provider::{self, Provider};
use crate::proxy::{self, json_body, App, Dialect, Port};
use crate::store::now;

pub fn router(port: Port) -> Router {
    Router::new()
        .fallback(any(dispatch))
        .layer(DefaultBodyLimit::disable())
        .layer(proxy::cors())
        .with_state(port)
}

async fn dispatch(
    state: State<Port>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let dialect = Dialect::of(&headers);
    let path = proxy::strip_v1(uri.path());
    // Anthropic 方言在 claude 端口落回透传 -> 拿到官方原样形状;
    // codex 上游 `/models` 不是官方 OpenAI 形状 -> 该端口一律由本层出列表。
    let openai_list =
        method == Method::GET && (dialect == Dialect::OpenAI || state.provider == Provider::Codex);

    match path {
        "/health" => proxy::health(state).await,
        "/chat/completions" if method == Method::POST => chat_completions(state, body).await,
        "/models" if openai_list => openai_models(state).await,
        p if openai_list && p.starts_with("/models/") => {
            openai_model(state, &p["/models/".len()..]).await
        }
        _ => proxy::handle(state, method, uri, headers, body).await,
    }
}

// ---------- Chat Completions ----------

/// provider 由端口定, model 只取名字 (允许 `openai/`、`anthropic/` 前缀)。
async fn chat_completions(State(port): State<Port>, body: Bytes) -> Response {
    let p = port.provider;
    let bad = |msg: &str| Dialect::OpenAI.error(StatusCode::BAD_REQUEST, "invalid_request", msg);
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return bad("请求体不是合法 JSON");
    };
    let Some(model) = req
        .get("model")
        .and_then(Value::as_str)
        .and_then(convert::model_name)
    else {
        return bad("model 必填; 可用取值见 GET /v1/models");
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
            return Dialect::OpenAI.error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "upstream",
                &e.to_string(),
            )
        }
    };

    let started = Instant::now();
    // 上游一律 SSE: 客户端要非流式时由转换层聚合。
    let resp = match proxy::upstream(
        &port.app,
        p,
        Method::POST,
        &url,
        &HeaderMap::new(),
        upstream_body,
        true,
        Dialect::OpenAI,
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
        // chat/completions 永远是 OpenAI 协议 -> 错误信封也固定 OpenAI 形状。
        return proxy::normalize_error(resp, Dialect::OpenAI).await;
    }
    if want_stream {
        convert::stream_response(p, model, resp, include_usage)
    } else {
        convert::aggregate_response(p, model, resp).await
    }
}

// ---------- 模型列表 (OpenAI 方言) ----------

async fn openai_models(State(port): State<Port>) -> Response {
    let data = list(&port.app, port.provider).await;
    json_body(StatusCode::OK, json!({ "object": "list", "data": data }))
}

async fn openai_model(State(port): State<Port>, id: &str) -> Response {
    let Some(name) = convert::model_name(id) else {
        return Dialect::OpenAI.error(StatusCode::NOT_FOUND, "not_found", "model 不存在");
    };
    match list(&port.app, port.provider)
        .await
        .into_iter()
        .find(|m| m["id"] == name)
    {
        Some(m) => json_body(StatusCode::OK, m),
        None => Dialect::OpenAI.error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("model `{id}` 不在 {} 端口的可用列表中", port.provider),
        ),
    }
}

/// `owned_by` 取官方厂商名的规范大小写。
///
/// Anthropic 没有 OpenAI 形状的 models 端点 -> 无既定小写约定, 而客户端
/// (ChatWise 等) 按 `owned_by == "Anthropic"` 过滤, 小写会被筛成空列表。
/// Codex 保持 `openai`: OpenAI 官方 `/v1/models` 就是小写。
fn owner_of(p: Provider) -> &'static str {
    match p {
        Provider::Anthropic => "Anthropic",
        Provider::Codex => "openai",
    }
}

/// 取不到不报错 -> 列表尽力而为。
async fn list(app: &Arc<App>, p: Provider) -> Vec<Value> {
    let owner = owner_of(p);
    let (url, list_key, id_key) = match p {
        Provider::Anthropic => (
            format!("{}/v1/models?limit=1000", provider::ANTHROPIC_UPSTREAM),
            "data",
            "id",
        ),
        Provider::Codex => (
            format!(
                "{}/models?client_version={}",
                provider::CODEX_UPSTREAM,
                provider::codex_cli_version()
            ),
            "models",
            "slug",
        ),
    };
    let sent = proxy::upstream(
        app,
        p,
        Method::GET,
        &url,
        &HeaderMap::new(),
        Bytes::new(),
        false,
        Dialect::OpenAI,
    )
    .await;
    let body = match sent {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
        Ok(r) => {
            tracing::warn!(provider = %p, status = r.status().as_u16(), "模型列表获取失败");
            None
        }
        Err(_) => {
            tracing::warn!(provider = %p, "模型列表跳过: 未登录或上游异常");
            None
        }
    };
    body.map(|v| entries(&v, list_key, id_key, owner))
        .unwrap_or_default()
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
        assert!(entries(&json!({}), "data", "id", "Anthropic").is_empty());
    }

    #[test]
    fn owner_uses_vendor_casing() {
        // 客户端按 `owned_by == "Anthropic"` 过滤模型列表, 小写会被筛空。
        assert_eq!(owner_of(Provider::Anthropic), "Anthropic");
        assert_eq!(owner_of(Provider::Codex), "openai");
    }
}
