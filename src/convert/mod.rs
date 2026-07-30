//! OpenAI Chat Completions <-> 两家原生协议的双向转换。
//!
//! 上游一律走 SSE; 客户端要非流式时在本层聚合 -> 只维护一条解析路径。

pub mod anthropic;
pub mod codex;

use std::io;

use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::StreamExt as _;
use rand::RngCore as _;
use serde_json::{json, Map, Value};

use crate::provider::Provider;
use crate::proxy::json_body;
use crate::sse;
use crate::store::now;

/// 与协议无关的增量语义。两家 translator 只负责产出它, chunk 组装只有一份。
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolArgs {
        index: usize,
        args: String,
    },
    Finish(&'static str),
    Usage(Value),
    Error(Value),
}

pub trait Translate: Send {
    fn on_event(&mut self, ev: &sse::Event) -> Vec<Delta>;
}

fn translator(p: Provider) -> Box<dyn Translate> {
    match p {
        Provider::Anthropic => Box::new(anthropic::Translator::default()),
        Provider::Codex => Box::new(codex::Translator::default()),
    }
}

/// model -> provider。允许 `anthropic/xxx` 这类前缀 (部分客户端会带)。
pub fn route(model: &str) -> Option<(Provider, &str)> {
    let name = model.rsplit('/').next().unwrap_or(model).trim();
    if name.is_empty() {
        return None;
    }
    let p = if name.to_ascii_lowercase().starts_with("claude") {
        Provider::Anthropic
    } else {
        Provider::Codex
    };
    Some((p, name))
}

/// 组装上游请求体 (已含 stream=true 等硬要求)。
pub fn request(p: Provider, req: &Value, model: &str) -> Value {
    match p {
        Provider::Anthropic => anthropic::request(req, model),
        Provider::Codex => codex::request(req, model),
    }
}

// ---------- 流式 ----------

pub fn stream_response(
    p: Provider,
    model: String,
    upstream: reqwest::Response,
    include_usage: bool,
) -> Response {
    let id = new_id();
    let created = now();
    let body = Body::from_stream(stream! {
        let mut tr = translator(p);
        let mut dec = sse::Decoder::default();
        let mut bytes = Box::pin(upstream.bytes_stream());
        let mut usage: Option<Value> = None;
        let mut finished = false;

        yield sse_chunk(&id, &model, created, json!({ "role": "assistant", "content": "" }), None);

        while let Some(item) = bytes.next().await {
            let raw = match item {
                Ok(b) => b,
                Err(e) => {
                    yield sse_data(&json!({ "error": { "message": e.to_string(), "type": "upstream_error" } }));
                    break;
                }
            };
            dec.push(&raw);
            while let Some(ev) = dec.next_event() {
                for d in tr.on_event(&ev) {
                    match d {
                        Delta::Usage(u) => usage = Some(u),
                        Delta::Finish(reason) if !finished => {
                            finished = true;
                            yield sse_chunk(&id, &model, created, json!({}), Some(reason));
                        }
                        Delta::Finish(_) => {}
                        Delta::Error(e) => yield sse_data(&json!({ "error": e })),
                        other => {
                            if let Some(v) = delta_value(&other) {
                                yield sse_chunk(&id, &model, created, v, None);
                            }
                        }
                    }
                }
            }
        }

        if !finished {
            yield sse_chunk(&id, &model, created, json!({}), Some("stop"));
        }
        if include_usage {
            if let Some(u) = usage {
                yield sse_data(&json!({
                    "id": id, "object": "chat.completion.chunk", "created": created,
                    "model": model, "choices": [], "usage": u,
                }));
            }
        }
        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap_or_else(|e| {
            json_body(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": { "message": e.to_string(), "type": "proxy_error" } }),
            )
        })
}

// ---------- 非流式: 聚合上游 SSE ----------

pub async fn aggregate_response(
    p: Provider,
    model: String,
    upstream: reqwest::Response,
) -> Response {
    let mut tr = translator(p);
    let mut dec = sse::Decoder::default();
    let mut bytes = Box::pin(upstream.bytes_stream());

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools: Vec<(String, String, String)> = Vec::new(); // (id, name, arguments)
    let mut finish = "stop";
    let mut usage: Option<Value> = None;

    while let Some(item) = bytes.next().await {
        let raw = match item {
            Ok(b) => b,
            Err(e) => {
                return json_body(
                    StatusCode::BAD_GATEWAY,
                    json!({ "error": { "message": e.to_string(), "type": "upstream_error" } }),
                )
            }
        };
        dec.push(&raw);
        while let Some(ev) = dec.next_event() {
            for d in tr.on_event(&ev) {
                match d {
                    Delta::Text(t) => text.push_str(&t),
                    Delta::Reasoning(t) => reasoning.push_str(&t),
                    Delta::ToolStart { index, id, name } => {
                        while tools.len() <= index {
                            tools.push(Default::default());
                        }
                        tools[index].0 = id;
                        tools[index].1 = name;
                    }
                    Delta::ToolArgs { index, args } => {
                        while tools.len() <= index {
                            tools.push(Default::default());
                        }
                        tools[index].2.push_str(&args);
                    }
                    Delta::Finish(r) => finish = r,
                    Delta::Usage(u) => usage = Some(u),
                    Delta::Error(e) => {
                        return json_body(StatusCode::BAD_GATEWAY, json!({ "error": e }))
                    }
                }
            }
        }
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() && !tools.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tools.is_empty() {
        message.insert(
            "tool_calls".into(),
            Value::Array(
                tools
                    .into_iter()
                    .map(|(id, name, args)| {
                        json!({
                            "id": id, "type": "function",
                            "function": { "name": name, "arguments": args },
                        })
                    })
                    .collect(),
            ),
        );
    }

    json_body(
        StatusCode::OK,
        json!({
            "id": new_id(),
            "object": "chat.completion",
            "created": now(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": finish,
                "logprobs": null,
            }],
            "usage": usage.unwrap_or_else(|| json!({
                "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
            })),
        }),
    )
}

// ---------- 组装 ----------

fn delta_value(d: &Delta) -> Option<Value> {
    match d {
        Delta::Text(t) => Some(json!({ "content": t })),
        Delta::Reasoning(t) => Some(json!({ "reasoning_content": t })),
        Delta::ToolStart { index, id, name } => Some(json!({
            "tool_calls": [{
                "index": index, "id": id, "type": "function",
                "function": { "name": name, "arguments": "" },
            }]
        })),
        Delta::ToolArgs { index, args } => Some(json!({
            "tool_calls": [{ "index": index, "function": { "arguments": args } }]
        })),
        _ => None,
    }
}

fn sse_chunk(
    id: &str,
    model: &str,
    created: u64,
    delta: Value,
    finish: Option<&str>,
) -> Result<Bytes, io::Error> {
    sse_data(&json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish, "logprobs": null }],
    }))
}

fn sse_data(v: &Value) -> Result<Bytes, io::Error> {
    Ok(Bytes::from(format!("data: {v}\n\n")))
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("chatcmpl-{hex}")
}

// ---------- 两家共用的小工具 ----------

/// content 取纯文本: string 原样, 数组拼接其中的文本片段。
pub(crate) fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| p.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn messages(req: &Value) -> &[Value] {
    req.get("messages")
        .and_then(Value::as_array)
        .map_or(&[], |v| v)
}

/// chat 的 tool 定义可能是 `{type,function:{...}}` 或已扁平化的 `{name,...}`。
pub(crate) fn tool_def(t: &Value) -> &Value {
    t.get("function").filter(|f| f.is_object()).unwrap_or(t)
}

pub(crate) fn max_tokens(req: &Value) -> Option<u64> {
    ["max_completion_tokens", "max_tokens"]
        .iter()
        .find_map(|k| req.get(*k).and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_routes_by_family() {
        assert_eq!(route("claude-opus-5").unwrap().0, Provider::Anthropic);
        assert_eq!(
            route("anthropic/claude-sonnet-5").unwrap(),
            (Provider::Anthropic, "claude-sonnet-5")
        );
        assert_eq!(route("gpt-5.6-sol").unwrap().0, Provider::Codex);
        assert_eq!(
            route("openai/gpt-5.5").unwrap(),
            (Provider::Codex, "gpt-5.5")
        );
        assert!(route("  ").is_none());
    }

    #[test]
    fn text_of_flattens_parts() {
        assert_eq!(text_of(&json!("hi")), "hi");
        assert_eq!(
            text_of(
                &json!([{"type":"text","text":"a"},{"type":"image_url"},{"type":"text","text":"b"}])
            ),
            "ab"
        );
        assert_eq!(text_of(&Value::Null), "");
    }

    #[test]
    fn tool_def_accepts_both_shapes() {
        let wrapped = json!({"type":"function","function":{"name":"f"}});
        assert_eq!(tool_def(&wrapped)["name"], json!("f"));
        let flat = json!({"name":"g"});
        assert_eq!(tool_def(&flat)["name"], json!("g"));
    }
}
