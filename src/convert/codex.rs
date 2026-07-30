//! Chat Completions <-> OpenAI Responses (Codex CLI 渠道)。
//!
//! 事件形状取自真实上游抓包 (response.output_item.added / function_call_arguments.delta / ...)。

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::{max_tokens, messages, text_of, tool_def, Delta, Translate};
use crate::sse;

// ---------- 请求: chat -> responses ----------

pub fn request(req: &Value, model: &str) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for m in messages(req) {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = m.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                let t = text_of(content);
                if !t.is_empty() {
                    instructions.push(t);
                }
            }
            "tool" | "function" => input.push(json!({
                "type": "function_call_output",
                "call_id": m.get("tool_call_id").cloned().unwrap_or(Value::Null),
                "output": text_of(content),
            })),
            "assistant" => {
                let t = text_of(content);
                if !t.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": t }],
                    }));
                }
                for call in m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                {
                    let f = call.get("function").unwrap_or(call);
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": f.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": f.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                    }));
                }
            }
            _ => input.push(json!({
                "type": "message", "role": "user", "content": user_content(content),
            })),
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("instructions".into(), json!(instructions.join("\n\n")));
    out.insert("input".into(), Value::Array(input));
    // 上游硬要求: 只走 SSE + 不落库。
    out.insert("stream".into(), json!(true));
    out.insert("store".into(), json!(false));

    if let Some(tools) = tools(req) {
        out.insert("tools".into(), tools);
    }
    if let Some(tc) = tool_choice(req) {
        out.insert("tool_choice".into(), tc);
    }
    if let Some(v) = req.get("parallel_tool_calls").filter(|v| v.is_boolean()) {
        out.insert("parallel_tool_calls".into(), v.clone());
    }
    if let Some(n) = max_tokens(req) {
        out.insert("max_output_tokens".into(), json!(n));
    }
    if let Some(r) = reasoning(req) {
        out.insert("reasoning".into(), r);
    }
    if let Some(f) = text_format(req) {
        out.insert("text".into(), json!({ "format": f }));
    }
    Value::Object(out)
}

fn user_content(v: &Value) -> Value {
    match v {
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                    Some("text") => Some(json!({ "type": "input_text", "text": p["text"] })),
                    Some("input_text" | "input_image") => Some(p.clone()),
                    Some("image_url") => {
                        image_url(p).map(|u| json!({ "type": "input_image", "image_url": u }))
                    }
                    _ => None,
                })
                .collect(),
        ),
        other => json!([{ "type": "input_text", "text": text_of(other) }]),
    }
}

fn image_url(part: &Value) -> Option<String> {
    let v = part.get("image_url")?;
    v.get("url")
        .and_then(Value::as_str)
        .or_else(|| v.as_str())
        .map(str::to_string)
}

fn tools(req: &Value) -> Option<Value> {
    let arr = req.get("tools")?.as_array()?;
    let mapped: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = tool_def(t);
            let name = f.get("name")?.as_str()?;
            let mut o = Map::new();
            o.insert("type".into(), json!("function"));
            o.insert("name".into(), json!(name));
            if let Some(d) = f.get("description").filter(|v| v.is_string()) {
                o.insert("description".into(), d.clone());
            }
            o.insert(
                "parameters".into(),
                f.get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            );
            o.insert(
                "strict".into(),
                f.get("strict").cloned().unwrap_or(json!(false)),
            );
            Some(Value::Object(o))
        })
        .collect();
    (!mapped.is_empty()).then_some(Value::Array(mapped))
}

fn tool_choice(req: &Value) -> Option<Value> {
    match req.get("tool_choice")? {
        Value::String(s) => Some(json!(s)),
        Value::Object(o) => {
            let name = o
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| o.get("name"))?;
            Some(json!({ "type": "function", "name": name }))
        }
        _ => None,
    }
}

fn reasoning(req: &Value) -> Option<Value> {
    if let Some(o) = req.get("reasoning").filter(|v| v.is_object()) {
        return Some(o.clone());
    }
    let effort = req.get("reasoning_effort")?.as_str()?;
    Some(json!({ "effort": effort, "summary": "auto" }))
}

/// response_format -> Responses 的 text.format。
fn text_format(req: &Value) -> Option<Value> {
    let rf = req.get("response_format")?;
    match rf.get("type").and_then(Value::as_str)? {
        "json_object" => Some(json!({ "type": "json_object" })),
        "json_schema" => {
            let s = rf.get("json_schema")?;
            let mut o = Map::new();
            o.insert("type".into(), json!("json_schema"));
            o.insert(
                "name".into(),
                s.get("name").cloned().unwrap_or(json!("response")),
            );
            o.insert("schema".into(), s.get("schema")?.clone());
            if let Some(strict) = s.get("strict") {
                o.insert("strict".into(), strict.clone());
            }
            Some(Value::Object(o))
        }
        _ => None,
    }
}

// ---------- 响应: responses SSE -> chat 增量 ----------

#[derive(Default)]
pub struct Translator {
    /// item_id -> tool_calls 下标
    calls: HashMap<String, usize>,
    next_index: usize,
    saw_tool: bool,
}

impl Translator {
    fn tool_index(&mut self, item_id: &str) -> usize {
        if let Some(i) = self.calls.get(item_id) {
            return *i;
        }
        let i = self.next_index;
        self.next_index += 1;
        self.calls.insert(item_id.to_string(), i);
        self.saw_tool = true;
        i
    }
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
        let text = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();

        match kind {
            "response.output_text.delta" => vec![Delta::Text(text("delta"))],
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                vec![Delta::Reasoning(text("delta"))]
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = &v["item"];
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return vec![];
                }
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                if kind == "response.output_item.done" && self.calls.contains_key(item_id) {
                    return vec![]; // 已由 added + arguments.delta 覆盖
                }
                let index = self.tool_index(item_id);
                let start = Delta::ToolStart {
                    index,
                    id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(item_id)
                        .to_string(),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                };
                // done 直达 (中途没收到 added): 参数一次性补齐
                match item.get("arguments").and_then(Value::as_str) {
                    Some(args) if kind == "response.output_item.done" && !args.is_empty() => {
                        vec![
                            start,
                            Delta::ToolArgs {
                                index,
                                args: args.to_string(),
                            },
                        ]
                    }
                    _ => vec![start],
                }
            }
            "response.function_call_arguments.delta" => {
                let index = self.tool_index(&text("item_id"));
                vec![Delta::ToolArgs {
                    index,
                    args: text("delta"),
                }]
            }
            "response.completed" | "response.incomplete" => {
                let mut out = Vec::new();
                if let Some(u) = usage(&v["response"]["usage"]) {
                    out.push(Delta::Usage(u));
                }
                let reason = if kind == "response.incomplete" {
                    "length"
                } else if self.saw_tool {
                    "tool_calls"
                } else {
                    "stop"
                };
                out.push(Delta::Finish(reason));
                out
            }
            "response.failed" => vec![Delta::Error(error_of(&v["response"]["error"]))],
            "error" => vec![Delta::Error(error_of(&v))],
            _ => vec![],
        }
    }
}

fn usage(u: &Value) -> Option<Value> {
    let input = u.get("input_tokens")?.as_u64().unwrap_or(0);
    let output = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    Some(json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": u.get("total_tokens").and_then(Value::as_u64).unwrap_or(input + output),
        "prompt_tokens_details": {
            "cached_tokens": u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        },
        "completion_tokens_details": {
            "reasoning_tokens": u["output_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
        },
    }))
}

fn error_of(e: &Value) -> Value {
    let msg = e
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("上游流式错误");
    json!({ "message": msg, "type": e.get("type").cloned().unwrap_or(json!("upstream_error")) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Translator, data: &str) -> Vec<Delta> {
        t.on_event(&sse::Event {
            name: String::new(),
            data: data.to_string(),
        })
    }

    #[test]
    fn system_goes_to_instructions_and_user_to_input() {
        let req = json!({
            "model": "x",
            "messages": [
                {"role":"system","content":"be brief"},
                {"role":"user","content":"hi"},
            ],
        });
        let out = request(&req, "gpt-5.6-sol");
        assert_eq!(out["instructions"], json!("be brief"));
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["store"], json!(false));
        assert_eq!(out["input"][0]["role"], json!("user"));
        assert_eq!(out["input"][0]["content"][0]["type"], json!("input_text"));
    }

    #[test]
    fn tool_roundtrip_shapes() {
        let req = json!({
            "messages": [
                {"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_1","type":"function","function":{"name":"f","arguments":"{\"a\":1}"}}
                ]},
                {"role":"tool","tool_call_id":"call_1","content":"42"},
            ],
            "tools": [{"type":"function","function":{"name":"f","description":"d","parameters":{"type":"object"}}}],
            "tool_choice": {"type":"function","function":{"name":"f"}},
            "max_tokens": 128,
        });
        let out = request(&req, "m");
        assert_eq!(out["input"][0]["type"], json!("function_call"));
        assert_eq!(out["input"][0]["call_id"], json!("call_1"));
        assert_eq!(out["input"][1]["type"], json!("function_call_output"));
        assert_eq!(out["input"][1]["output"], json!("42"));
        assert_eq!(out["tools"][0]["name"], json!("f"));
        assert_eq!(out["tool_choice"], json!({"type":"function","name":"f"}));
        assert_eq!(out["max_output_tokens"], json!(128));
    }

    #[test]
    fn image_part_becomes_input_image() {
        let req = json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"https://x/y.png"}}
        ]}]});
        let out = request(&req, "m");
        assert_eq!(out["input"][0]["content"][1]["type"], json!("input_image"));
        assert_eq!(
            out["input"][0]["content"][1]["image_url"],
            json!("https://x/y.png")
        );
    }

    #[test]
    fn stream_text_then_finish() {
        let mut t = Translator::default();
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"response.output_text.delta","delta":"PRO"}"#
            ),
            vec![Delta::Text("PRO".into())]
        );
        let out = feed(
            &mut t,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":68,"output_tokens":18,"total_tokens":86}}}"#,
        );
        assert_eq!(out[1], Delta::Finish("stop"));
        assert!(matches!(&out[0], Delta::Usage(u) if u["prompt_tokens"] == json!(68)));
    }

    #[test]
    fn stream_tool_call_indexes_by_item_id() {
        let mut t = Translator::default();
        let added = feed(
            &mut t,
            r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"get_weather","arguments":""}}"#,
        );
        assert_eq!(
            added,
            vec![Delta::ToolStart {
                index: 0,
                id: "call_1".into(),
                name: "get_weather".into()
            }]
        );
        assert_eq!(
            feed(
                &mut t,
                r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"c"}"#
            ),
            vec![Delta::ToolArgs {
                index: 0,
                args: "{\"c".into()
            }]
        );
        // done 不重复产出
        assert!(feed(
            &mut t,
            r#"{"type":"response.output_item.done","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{}"}}"#
        )
        .is_empty());
        let end = feed(&mut t, r#"{"type":"response.completed","response":{}}"#);
        assert_eq!(end, vec![Delta::Finish("tool_calls")]);
    }
}
