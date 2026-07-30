//! Chat Completions <-> Anthropic Messages (Claude Code CLI 渠道)。

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::{max_tokens, messages, text_of, tool_def, Delta, Translate};
use crate::sse;

/// Anthropic 的 max_tokens 必填; 客户端没给时的兜底。
const DEFAULT_MAX_TOKENS: u64 = 8192;
/// 思考 token 也计入输出上限 -> 开思考时抬到这个下限, 免得答案被思考挤空。
const THINKING_MIN_OUTPUT: u64 = 4096;

// ---------- 请求: chat -> messages ----------

pub fn request(req: &Value, model: &str) -> Value {
    let mut system: Vec<String> = Vec::new();
    let mut msgs: Vec<Value> = Vec::new();

    for m in messages(req) {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = m.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                let t = text_of(content);
                if !t.is_empty() {
                    system.push(t);
                }
            }
            "tool" | "function" => push(
                &mut msgs,
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "content": text_of(content),
                })],
            ),
            "assistant" => {
                let mut blocks = Vec::new();
                let t = text_of(content);
                if !t.is_empty() {
                    blocks.push(json!({ "type": "text", "text": t }));
                }
                for call in m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                {
                    let f = call.get("function").unwrap_or(call);
                    let args = f.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": f.get("name").cloned().unwrap_or(Value::Null),
                        "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
                    }));
                }
                push(&mut msgs, "assistant", blocks);
            }
            _ => push(&mut msgs, "user", user_blocks(content)),
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(msgs));
    // 上游只需一条解析路径: 统一 SSE, 非流式由本层聚合。
    out.insert("stream".into(), json!(true));
    if !system.is_empty() {
        out.insert("system".into(), json!(system.join("\n\n")));
    }

    let mut limit = max_tokens(req).unwrap_or(DEFAULT_MAX_TOKENS);
    let effort = reasoning_effort(req);
    if effort.is_some() {
        limit = limit.max(THINKING_MIN_OUTPUT);
        out.insert("thinking".into(), json!({ "type": "adaptive" }));
    }
    out.insert("max_tokens".into(), json!(limit));

    // 采样参数 (temperature / top_p / top_k) 一律不转发: 上游自 Opus 4.7 起在所有新模型上
    // 硬拒 ("`temperature` is deprecated for this model", 400), 且名单随新模型持续变化 ->
    // 不做模型名判断, 统一丢弃 (仅老模型损失采样控制, 换取任何客户端都不会整条请求失败)。
    let mut cfg = Map::new();
    if let Some(e) = effort {
        cfg.insert("effort".into(), json!(e));
    }
    if let Some(f) = output_format(req) {
        cfg.insert("format".into(), f);
    }
    if !cfg.is_empty() {
        out.insert("output_config".into(), Value::Object(cfg));
    }
    if let Some(stop) = stop_sequences(req) {
        out.insert("stop_sequences".into(), stop);
    }
    if let Some(tools) = tools(req) {
        out.insert("tools".into(), tools);
    }
    if let Some(tc) = tool_choice(req) {
        out.insert("tool_choice".into(), tc);
    }
    // OAuth 凭证要求 system 首块带 Claude Code 前缀。
    crate::proxy::inject_claude_code_prefix(&mut out);
    Value::Object(out)
}

/// 同角色相邻消息合并 -> Anthropic 要求 user/assistant 交替。
fn push(msgs: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = msgs.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
                arr.extend(blocks);
                return;
            }
        }
    }
    msgs.push(json!({ "role": role, "content": blocks }));
}

fn user_blocks(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => nonempty_text(&text_of(p)),
                Some("image_url") => image_block(p),
                Some("image") => Some(p.clone()),
                _ => None,
            })
            .collect(),
        other => nonempty_text(&text_of(other)).into_iter().collect(),
    }
}

fn nonempty_text(t: &str) -> Option<Value> {
    (!t.is_empty()).then(|| json!({ "type": "text", "text": t }))
}

/// data URI 走 base64 source, 其余走 url source。
fn image_block(part: &Value) -> Option<Value> {
    let iv = part.get("image_url")?;
    let url = iv
        .get("url")
        .and_then(Value::as_str)
        .or_else(|| iv.as_str())?;
    let Some(rest) = url.strip_prefix("data:") else {
        return Some(json!({ "type": "image", "source": { "type": "url", "url": url } }));
    };
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.split(';').next().unwrap_or("image/png");
    Some(json!({
        "type": "image",
        "source": { "type": "base64", "media_type": media_type, "data": data },
    }))
}

/// `reasoning_effort` -> `output_config.effort` (档位同名直通)。
///
/// 上游已移除按 token 数给思考预算的 `thinking.enabled`, 新模型只认 adaptive + effort。
fn reasoning_effort(req: &Value) -> Option<&'static str> {
    match req.get("reasoning_effort")?.as_str()? {
        "none" | "minimal" => None,
        "low" => Some("low"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => Some("medium"), // medium 及未知档位
    }
}

/// `response_format` -> `output_config.format` (结构化输出)。
/// 上游只认 json_schema; `json_object` 无对应形状 -> 不映射, 避免造出假约束。
fn output_format(req: &Value) -> Option<Value> {
    let rf = req.get("response_format")?;
    if rf.get("type").and_then(Value::as_str)? != "json_schema" {
        return None;
    }
    let s = rf.get("json_schema")?;
    Some(json!({ "type": "json_schema", "schema": s.get("schema")?.clone() }))
}

fn stop_sequences(req: &Value) -> Option<Value> {
    match req.get("stop")? {
        Value::String(s) => Some(json!([s])),
        Value::Array(a) if !a.is_empty() => Some(Value::Array(a.clone())),
        _ => None,
    }
}

fn tools(req: &Value) -> Option<Value> {
    let arr = req.get("tools")?.as_array()?;
    let mapped: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = tool_def(t);
            let mut o = Map::new();
            o.insert("name".into(), json!(f.get("name")?.as_str()?));
            if let Some(d) = f.get("description").filter(|v| v.is_string()) {
                o.insert("description".into(), d.clone());
            }
            o.insert(
                "input_schema".into(),
                f.get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            );
            Some(Value::Object(o))
        })
        .collect();
    (!mapped.is_empty()).then_some(Value::Array(mapped))
}

fn tool_choice(req: &Value) -> Option<Value> {
    match req.get("tool_choice")? {
        Value::String(s) => match s.as_str() {
            "auto" => Some(json!({ "type": "auto" })),
            "required" => Some(json!({ "type": "any" })),
            "none" => Some(json!({ "type": "none" })),
            _ => None,
        },
        Value::Object(o) => {
            let name = o
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| o.get("name"))?;
            Some(json!({ "type": "tool", "name": name }))
        }
        _ => None,
    }
}

// ---------- 响应: messages SSE -> chat 增量 ----------

#[derive(Default)]
pub struct Translator {
    /// content block index -> tool_calls 下标
    tools: HashMap<u64, usize>,
    next_index: usize,
    prompt_tokens: u64,
    cached_tokens: u64,
}

impl Translate for Translator {
    fn on_event(&mut self, ev: &sse::Event) -> Vec<Delta> {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return vec![];
        };
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(ev.name.as_str());
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);

        match kind {
            "message_start" => {
                let u = &v["message"]["usage"];
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                self.cached_tokens = n("cache_read_input_tokens");
                // Anthropic 的 input_tokens 不含缓存部分, OpenAI 的 prompt_tokens 含。
                self.prompt_tokens =
                    n("input_tokens") + self.cached_tokens + n("cache_creation_input_tokens");
                vec![]
            }
            "content_block_start" => {
                let block = &v["content_block"];
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return vec![];
                }
                let slot = self.next_index;
                self.next_index += 1;
                self.tools.insert(index, slot);
                vec![Delta::ToolStart {
                    index: slot,
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }]
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let text = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => vec![Delta::Text(text("text"))],
                    Some("thinking_delta") => vec![Delta::Reasoning(text("thinking"))],
                    Some("input_json_delta") => match self.tools.get(&index) {
                        Some(slot) => vec![Delta::ToolArgs {
                            index: *slot,
                            args: text("partial_json"),
                        }],
                        None => vec![],
                    },
                    _ => vec![],
                }
            }
            "message_delta" => {
                let completion = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                let mut out = vec![Delta::Usage(json!({
                    "prompt_tokens": self.prompt_tokens,
                    "completion_tokens": completion,
                    "total_tokens": self.prompt_tokens + completion,
                    "prompt_tokens_details": { "cached_tokens": self.cached_tokens },
                }))];
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    out.push(Delta::Finish(finish_reason(r)));
                }
                out
            }
            "error" => vec![Delta::Error(json!({
                "message": v["error"]["message"].as_str().unwrap_or("上游流式错误"),
                "type": v["error"]["type"].as_str().unwrap_or("upstream_error"),
            }))],
            _ => vec![],
        }
    }
}

fn finish_reason(stop: &str) -> &'static str {
    match stop {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CLAUDE_CODE_SYSTEM_PREFIX;

    fn feed(t: &mut Translator, data: &str) -> Vec<Delta> {
        t.on_event(&sse::Event {
            name: String::new(),
            data: data.to_string(),
        })
    }

    #[test]
    fn system_merged_and_cli_prefix_injected() {
        let req = json!({
            "messages": [
                {"role":"system","content":"a"},
                {"role":"user","content":"hi"},
                {"role":"system","content":"b"},
            ],
        });
        let out = request(&req, "claude-opus-5");
        assert_eq!(out["system"][0]["text"], json!(CLAUDE_CODE_SYSTEM_PREFIX));
        assert_eq!(out["system"][1]["text"], json!("a\n\nb"));
        assert_eq!(out["max_tokens"], json!(DEFAULT_MAX_TOKENS));
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["messages"][0]["content"][0]["text"], json!("hi"));
    }

    #[test]
    fn tool_calls_and_results_map_to_blocks() {
        let req = json!({
            "messages": [
                {"role":"user","content":"go"},
                {"role":"assistant","tool_calls":[
                    {"id":"toolu_1","function":{"name":"f","arguments":"{\"a\":1}"}}
                ]},
                {"role":"tool","tool_call_id":"toolu_1","content":"42"},
                {"role":"tool","tool_call_id":"toolu_2","content":"7"},
            ],
            "tools": [{"type":"function","function":{"name":"f","parameters":{"type":"object"}}}],
            "tool_choice": "required",
        });
        let out = request(&req, "m");
        assert_eq!(out["messages"][1]["content"][0]["type"], json!("tool_use"));
        assert_eq!(out["messages"][1]["content"][0]["input"], json!({"a":1}));
        // 相邻 tool 结果合并进同一条 user 消息
        assert_eq!(out["messages"][2]["role"], json!("user"));
        assert_eq!(out["messages"][2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(out["tools"][0]["input_schema"], json!({"type":"object"}));
        assert_eq!(out["tool_choice"], json!({"type":"any"}));
    }

    /// 两个端口的 chat/completions 行为必须一致: codex 侧支持的这两项, anthropic 侧也要有。
    #[test]
    fn reasoning_effort_and_response_format_map_to_anthropic() {
        let req = json!({
            "messages": [{"role":"user","content":"hi"}],
            "reasoning_effort": "low",
            "max_tokens": 100,
            "temperature": 0.5,
            "response_format": {"type":"json_schema","json_schema":{"name":"r","schema":{"type":"object"}}},
        });
        let out = request(&req, "m");
        // 新模型只认 adaptive + effort 档位, 不认按 token 给的思考预算
        assert_eq!(out["thinking"], json!({"type":"adaptive"}));
        assert_eq!(out["output_config"]["effort"], json!("low"));
        // 上限不足以容纳思考 -> 抬到下限
        assert_eq!(out["max_tokens"], json!(THINKING_MIN_OUTPUT));
        assert_eq!(
            out["output_config"]["format"],
            json!({"type":"json_schema","schema":{"type":"object"}})
        );

        // 不要思考 -> 上限不动, 也不带 output_config
        let plain = request(
            &json!({"messages":[],"max_tokens":100,"temperature":0.5,"reasoning_effort":"none"}),
            "m",
        );
        assert!(plain.get("thinking").is_none());
        assert!(plain.get("output_config").is_none());
        assert_eq!(plain["max_tokens"], json!(100));
    }

    /// 采样参数上游硬拒 (400 deprecated) -> 任何档位下都不转发。
    #[test]
    fn sampling_params_are_never_forwarded() {
        for effort in ["none", "high"] {
            let out = request(
                &json!({
                    "messages": [{"role":"user","content":"hi"}],
                    "reasoning_effort": effort,
                    "temperature": 0.5, "top_p": 0.9, "top_k": 40,
                }),
                "claude-opus-5",
            );
            for key in ["temperature", "top_p", "top_k"] {
                assert!(out.get(key).is_none(), "{effort}: {key} 应被丢弃");
            }
        }
    }

    #[test]
    fn data_uri_image_becomes_base64_source() {
        let req = json!({"messages":[{"role":"user","content":[
            {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,QUJD"}}
        ]}]});
        let out = request(&req, "m");
        let src = &out["messages"][0]["content"][0]["source"];
        assert_eq!(src["type"], json!("base64"));
        assert_eq!(src["media_type"], json!("image/jpeg"));
        assert_eq!(src["data"], json!("QUJD"));
    }

    #[test]
    fn stream_tool_use_and_usage() {
        let mut t = Translator::default();
        feed(
            &mut t,
            r#"{"type":"message_start","message":{"usage":{"input_tokens":580,"cache_read_input_tokens":20}}}"#,
        );
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather"}}"#
            ),
            vec![Delta::ToolStart {
                index: 0,
                id: "toolu_1".into(),
                name: "get_weather".into()
            }]
        );
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"c"}}"#
            ),
            vec![Delta::ToolArgs {
                index: 0,
                args: "{\"c".into()
            }]
        );
        let end = feed(
            &mut t,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":53}}"#,
        );
        assert!(matches!(&end[0], Delta::Usage(u) if u["prompt_tokens"] == json!(600)));
        assert_eq!(end[1], Delta::Finish("tool_calls"));
    }

    #[test]
    fn text_and_thinking_deltas() {
        let mut t = Translator::default();
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#
            ),
            vec![Delta::Text("hi".into())]
        );
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#
            ),
            vec![Delta::Reasoning("hmm".into())]
        );
    }
}
