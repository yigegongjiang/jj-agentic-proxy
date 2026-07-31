import Foundation

// 「核心内容」视图: 把一次往返里真正有语义的东西 (对话轮次 / 模型产出 / 工具调用 / usage)
// 抽成纯文本。SSE 尤其需要 —— 上百帧 `event:` / `data:` 里, 真正的回复只是被切碎的几行字。
//
// 覆盖三种方言 (与代理三个协议面一致):
//   Anthropic Messages      /v1/messages         event: message_start / content_block_delta / ...
//   OpenAI Chat Completions /v1/chat/completions data: {"choices":[{"delta":{...}}]}
//   OpenAI Responses        /v1/responses        event: response.output_text.delta / ...
// 认不出方言就回退成紧凑事件清单或原 JSON -> 永远给得出东西, 不空屏。
nonisolated enum CoreContent {
    // MARK: - 入口

    /// 请求体 -> 对话视图 (system / 逐轮消息 / 工具调用与结果)。
    static func request(path: String, body: Any?) -> String {
        guard let dict = object(body) else { return fallback(body) }
        var doc = Doc()
        let kind = dialect(path: path, body: dict)
        doc.head.append(kind.name)
        doc.head.append(contentsOf: paramLines(dict))

        switch kind {
        case .responses:
            appendBlock(&doc, "instructions", text: contentText(dict["instructions"]))
            appendTurns(&doc, items(dict["input"]))
        default:
            appendBlock(&doc, "system", text: contentText(dict["system"]))
            appendTurns(&doc, items(dict["messages"]))
        }
        // 一条消息都没有 (count_tokens / models / 非对话请求) -> 原 JSON 比空视图有用
        if doc.blocks.isEmpty { return fallback(body) }
        return doc.render()
    }

    /// 响应体 -> 模型产出视图。SSE 先重建成完整消息再排版。
    static func response(path: String, body: Any?) -> String {
        if let text = body as? String {
            return isSSE(text) ? sse(path: path, text: text) : cap(text)
        }
        guard let dict = object(body) else { return fallback(body) }
        var b = Builder()
        switch dialect(path: path, body: dict) {
        case .anthropic:
            b.head = "Anthropic Messages"
            absorbAnthropicMessage(dict, into: &b)
        case .chat:
            b.head = "OpenAI Chat Completions"
            absorbChatMessage(dict, into: &b)
        case .responses:
            b.head = "OpenAI Responses"
            absorbResponsesObject(dict, into: &b)
        case .unknown:
            if errorBlock(dict, into: &b) { b.head = "错误响应" } else { return fallback(body) }
        }
        if b.blocks.isEmpty, !errorBlock(dict, into: &b) { return fallback(body) }
        return b.doc().render()
    }

    // MARK: - SSE

    /// 一帧 = `event:` + 若干 `data:`, 空行分隔。
    private struct Frame {
        var name = ""
        var data = ""
    }

    private static func isSSE(_ s: String) -> Bool {
        let head = s.prefix(4096)
        return head.hasPrefix("data:") || head.hasPrefix("event:")
            || head.contains("\ndata:") || head.contains("\nevent:")
    }

    private static func frames(_ text: String) -> [Frame] {
        var out: [Frame] = []
        var cur = Frame()
        var open = false
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = raw.hasSuffix("\r") ? String(raw.dropLast()) : String(raw)
            if line.isEmpty {
                if open { out.append(cur) }
                cur = Frame()
                open = false
                continue
            }
            if line.hasPrefix(":") { continue } // 注释 / keep-alive
            var field = line
            var value = ""
            if let colon = line.firstIndex(of: ":") {
                field = String(line[line.startIndex..<colon])
                var v = line[line.index(after: colon)...]
                if v.hasPrefix(" ") { v = v.dropFirst() }
                value = String(v)
            }
            switch field {
            case "event":
                cur.name = value
                open = true
            case "data":
                if !cur.data.isEmpty { cur.data.append("\n") }
                cur.data.append(value)
                open = true
            default:
                break
            }
        }
        if open { out.append(cur) }
        return out
    }

    private static func sse(path: String, text: String) -> String {
        let list = frames(text)
        guard !list.isEmpty else { return cap(text) }

        var b = Builder()
        b.frameCount = list.count
        var counts: [String: Int] = [:]
        var kind = Dialect.unknown

        for f in list {
            let tag = f.name.isEmpty ? "chunk" : f.name
            if f.data == "[DONE]" {
                b.done = true
                counts["[DONE]", default: 0] += 1
                continue
            }
            guard let payload = object(json(f.data)) else {
                counts[tag, default: 0] += 1
                if !f.data.isEmpty, f.name.isEmpty == false || f.data.hasPrefix("{") == false {
                    b.notes.append("\(tag): \(f.data.prefix(200))")
                }
                continue
            }
            let type = payload["type"] as? String ?? ""
            counts[type.isEmpty ? tag : type, default: 0] += 1
            if kind == .unknown { kind = sseDialect(event: f.name, type: type, payload: payload) }

            switch kind {
            case .anthropic: absorbAnthropicEvent(type: type, payload, into: &b)
            case .chat: absorbChatChunk(payload, into: &b)
            case .responses: absorbResponsesEvent(type: type, payload, into: &b)
            case .unknown: _ = errorBlock(payload, into: &b)
            }
        }

        b.head = kind == .unknown ? "SSE (未知方言)" : kind.name
        b.frameBreakdown = counts
            .sorted { $0.value == $1.value ? $0.key < $1.key : $0.value > $1.value }
            .prefix(6)
            .map { "\($0.key) \($0.value)" }
            .joined(separator: " · ")
        if kind == .unknown {
            // 方言认不出: 至少把每帧压成一行, 比几百行原文好读
            b.notes = list.prefix(200).map { f in
                let name = f.name.isEmpty ? "-" : f.name
                return "\(name)  \(f.data.prefix(160))"
            }
            if list.count > 200 { b.notes.append("… 其余 \(list.count - 200) 帧略") }
        }
        return b.doc().render()
    }

    // MARK: - Anthropic Messages

    private static func absorbAnthropicEvent(type: String, _ v: [String: Any], into b: inout Builder) {
        switch type {
        case "message_start":
            guard let msg = v["message"] as? [String: Any] else { return }
            b.model = msg["model"] as? String ?? b.model
            b.id = msg["id"] as? String ?? b.id
            b.usage = usageLine(msg["usage"])
        case "content_block_start":
            let idx = int(v["index"])
            guard let block = v["content_block"] as? [String: Any] else { return }
            b.append(key: "b\(idx)", label: anthropicLabel(block), blockLabelWins: true)
            // start 里可能已带首段内容 (text / thinking 非空)
            if let t = block["text"] as? String { b.append(key: "b\(idx)", label: anthropicLabel(block), t) }
            if let t = block["thinking"] as? String { b.append(key: "b\(idx)", label: anthropicLabel(block), t) }
            if let input = block["input"], !isEmptyObject(input) {
                b.append(key: "b\(idx)", label: anthropicLabel(block), jsonText(input))
            }
        case "content_block_delta":
            let idx = int(v["index"])
            guard let d = v["delta"] as? [String: Any] else { return }
            switch d["type"] as? String ?? "" {
            case "text_delta": b.append(key: "b\(idx)", label: "text", d["text"] as? String ?? "")
            case "thinking_delta": b.append(key: "b\(idx)", label: "thinking", d["thinking"] as? String ?? "")
            case "input_json_delta":
                b.append(key: "b\(idx)", label: "tool_use", d["partial_json"] as? String ?? "")
                b.markJSON("b\(idx)")
            case "citations_delta": break // 引文元数据, 不进正文
            // 签名本身无阅读价值; 但只回签名不回正文 = 上游加密了思考, 得说清楚, 否则像丢了内容
            case "signature_delta": b.note("b\(idx)", encryptedThinking)
            default: b.append(key: "b\(idx)", label: d["type"] as? String ?? "delta", jsonText(d))
            }
        case "message_delta":
            if let d = v["delta"] as? [String: Any] {
                b.stop = d["stop_reason"] as? String ?? b.stop
                if let seq = d["stop_sequence"] as? String { b.stop += " (\(seq))" }
            }
            if let u = usageLine(v["usage"]), !u.isEmpty { b.usage = u }
        case "message_stop":
            b.done = true
        case "error":
            _ = errorBlock(v, into: &b)
        default:
            break // ping 等无语义事件
        }
    }

    /// 非流式 Anthropic 响应对象。
    private static func absorbAnthropicMessage(_ v: [String: Any], into b: inout Builder) {
        b.model = v["model"] as? String ?? ""
        b.id = v["id"] as? String ?? ""
        b.stop = v["stop_reason"] as? String ?? ""
        b.usage = usageLine(v["usage"])
        b.done = true
        for (i, raw) in items(v["content"]).enumerated() {
            guard let block = raw as? [String: Any] else { continue }
            let label = anthropicLabel(block)
            switch block["type"] as? String ?? "" {
            case "text": b.append(key: "b\(i)", label: label, block["text"] as? String ?? "")
            case "thinking":
                b.append(key: "b\(i)", label: label, block["thinking"] as? String ?? "")
                if block["signature"] != nil { b.note("b\(i)", encryptedThinking) }
            default: b.append(key: "b\(i)", label: label, jsonText(block["input"] ?? block))
            }
        }
        _ = errorBlock(v, into: &b)
    }

    private static func anthropicLabel(_ block: [String: Any]) -> String {
        let type = block["type"] as? String ?? "block"
        guard type == "tool_use" || type == "server_tool_use" else { return type }
        var out = type
        if let name = block["name"] as? String { out += " · \(name)" }
        if let id = block["id"] as? String { out += " #\(id)" }
        return out
    }

    // MARK: - OpenAI Chat Completions

    private static func absorbChatChunk(_ v: [String: Any], into b: inout Builder) {
        if errorBlock(v, into: &b) { return }
        b.model = v["model"] as? String ?? b.model
        b.id = v["id"] as? String ?? b.id
        if let u = usageLine(v["usage"]), !u.isEmpty { b.usage = u }
        guard let choice = items(v["choices"]).first as? [String: Any] else { return }
        if let reason = choice["finish_reason"] as? String {
            b.stop = reason
            b.done = true
        }
        guard let d = choice["delta"] as? [String: Any] ?? choice["message"] as? [String: Any] else { return }
        absorbChatDelta(d, into: &b)
    }

    /// delta 与非流式 message 形状一致 -> 同一处消化。
    private static func absorbChatDelta(_ d: [String: Any], into b: inout Builder) {
        if let r = d["reasoning_content"] as? String, !r.isEmpty {
            b.append(key: "reasoning", label: "reasoning", r)
        }
        if let c = d["content"] as? String, !c.isEmpty {
            b.append(key: "content", label: "text", c)
        } else if d["content"] is [Any] {
            b.append(key: "content", label: "text", contentText(d["content"]))
        }
        for raw in items(d["tool_calls"]) {
            guard let call = raw as? [String: Any] else { continue }
            let idx = int(call["index"])
            let key = "tool\(idx)"
            let fn = call["function"] as? [String: Any] ?? [:]
            var label = "tool_call"
            if let name = fn["name"] as? String, !name.isEmpty { label += " · \(name)" }
            if let id = call["id"] as? String, !id.isEmpty { label += " #\(id)" }
            if label != "tool_call" { b.append(key: key, label: label, blockLabelWins: true) }
            if let args = fn["arguments"] as? String, !args.isEmpty {
                b.append(key: key, label: label, args)
                b.markJSON(key)
            }
        }
    }

    /// 非流式 Chat Completions 响应对象。
    private static func absorbChatMessage(_ v: [String: Any], into b: inout Builder) {
        b.model = v["model"] as? String ?? ""
        b.id = v["id"] as? String ?? ""
        b.usage = usageLine(v["usage"])
        b.done = true
        for raw in items(v["choices"]) {
            guard let choice = raw as? [String: Any] else { continue }
            b.stop = choice["finish_reason"] as? String ?? b.stop
            if let msg = choice["message"] as? [String: Any] { absorbChatDelta(msg, into: &b) }
        }
        _ = errorBlock(v, into: &b)
    }

    // MARK: - OpenAI Responses

    private static func absorbResponsesEvent(type: String, _ v: [String: Any], into b: inout Builder) {
        switch type {
        case "response.created", "response.in_progress":
            if let r = v["response"] as? [String: Any] {
                b.model = r["model"] as? String ?? b.model
                b.id = r["id"] as? String ?? b.id
            }
        case "response.output_text.delta":
            b.append(key: "text", label: "text", v["delta"] as? String ?? "")
        case "response.reasoning_summary_text.delta", "response.reasoning_text.delta":
            b.append(key: "reasoning", label: "reasoning", v["delta"] as? String ?? "")
        case "response.output_item.added", "response.output_item.done":
            guard let item = v["item"] as? [String: Any],
                  item["type"] as? String == "function_call" else { return }
            let key = "tool:\(item["id"] as? String ?? String(int(v["output_index"])))"
            b.append(key: key, label: responsesToolLabel(item), blockLabelWins: true)
            // done 才带完整 arguments; 增量已拼过就不重复追加
            if type == "response.output_item.done", b.isEmptyBlock(key),
               let args = item["arguments"] as? String, !args.isEmpty {
                b.append(key: key, label: responsesToolLabel(item), args)
                b.markJSON(key)
            }
        case "response.function_call_arguments.delta":
            let key = "tool:\(v["item_id"] as? String ?? String(int(v["output_index"])))"
            b.append(key: key, label: "tool_call", v["delta"] as? String ?? "")
            b.markJSON(key)
        case "response.completed", "response.incomplete", "response.failed":
            b.done = true
            guard let r = v["response"] as? [String: Any] else { return }
            b.model = r["model"] as? String ?? b.model
            b.id = r["id"] as? String ?? b.id
            b.usage = usageLine(r["usage"])
            b.stop = r["status"] as? String ?? (type == "response.failed" ? "failed" : b.stop)
            if let d = r["incomplete_details"] as? [String: Any] {
                b.stop += " (\(d["reason"] as? String ?? jsonText(d, pretty: false)))"
            }
            _ = errorBlock(r, into: &b)
        default:
            break // .done / .delta 的收尾事件与增量重复, 忽略
        }
    }

    /// 非流式 Responses 对象。
    private static func absorbResponsesObject(_ v: [String: Any], into b: inout Builder) {
        b.model = v["model"] as? String ?? ""
        b.id = v["id"] as? String ?? ""
        b.stop = v["status"] as? String ?? ""
        b.usage = usageLine(v["usage"])
        b.done = true
        for (i, raw) in items(v["output"]).enumerated() {
            guard let item = raw as? [String: Any] else { continue }
            switch item["type"] as? String ?? "" {
            case "message":
                b.append(key: "o\(i)", label: "text", contentText(item["content"]))
            case "reasoning":
                let text = contentText(item["summary"]) + contentText(item["content"])
                if !text.isEmpty { b.append(key: "o\(i)", label: "reasoning", text) }
            case "function_call":
                b.append(key: "o\(i)", label: responsesToolLabel(item), item["arguments"] as? String ?? "")
                b.markJSON("o\(i)")
            default:
                b.append(key: "o\(i)", label: item["type"] as? String ?? "item", jsonText(item))
            }
        }
        _ = errorBlock(v, into: &b)
    }

    private static func responsesToolLabel(_ item: [String: Any]) -> String {
        var out = "tool_call"
        if let name = item["name"] as? String, !name.isEmpty { out += " · \(name)" }
        if let id = item["call_id"] as? String ?? item["id"] as? String { out += " #\(id)" }
        return out
    }

    // MARK: - 请求侧: 参数行 + 对话轮次

    private static func paramLines(_ d: [String: Any]) -> [String] {
        var out: [String] = []
        var head: [String] = []
        if let m = d["model"] as? String { head.append("model \(m)") }
        if (d["stream"] as? NSNumber)?.boolValue == true { head.append("stream") }
        for key in ["max_tokens", "max_output_tokens", "max_completion_tokens", "temperature",
                    "top_p", "top_k", "reasoning_effort", "service_tier", "previous_response_id"] {
            if let v = d[key], !(v is NSNull) { head.append("\(key) \(scalar(v))") }
        }
        if let r = d["reasoning"] as? [String: Any] { head.append("reasoning \(jsonText(r, pretty: false))") }
        if let f = d["response_format"] ?? d["text"] { head.append("format \(jsonText(f, pretty: false))") }
        if !head.isEmpty { out.append(head.joined(separator: " · ")) }

        let tools = items(d["tools"])
        if !tools.isEmpty {
            let names = tools.compactMap { raw -> String? in
                guard let t = raw as? [String: Any] else { return nil }
                return t["name"] as? String ?? (t["function"] as? [String: Any])?["name"] as? String
                    ?? t["type"] as? String
            }
            var line = "tools(\(tools.count)) \(names.joined(separator: " · "))"
            if let choice = d["tool_choice"] { line += "  ·  tool_choice \(jsonText(choice, pretty: false))" }
            out.append(line)
        } else if let choice = d["tool_choice"] {
            out.append("tool_choice \(jsonText(choice, pretty: false))")
        }
        return out
    }

    private static func appendTurns(_ doc: inout Doc, _ list: [Any]) {
        for (i, raw) in list.enumerated() {
            guard let msg = raw as? [String: Any] else {
                appendBlock(&doc, "[\(i + 1)]", text: jsonText(raw))
                continue
            }
            let (label, text) = turn(msg, index: i + 1)
            doc.blocks.append(Block(label: label, body: text))
        }
    }

    /// 一轮消息 -> (标题, 正文)。Anthropic / OpenAI / Responses 三种形状同处消化:
    /// 结构上互不冲突, 分方言反而多三份几乎一样的代码。
    private static func turn(_ msg: [String: Any], index: Int) -> (String, String) {
        let type = msg["type"] as? String ?? ""
        var role = msg["role"] as? String ?? type
        var parts: [String] = []

        switch type {
        case "function_call": // Responses: 工具调用作为独立 input 项 (名字进标题, 这里只补 id + 参数)
            role = "function_call"
            let id = msg["call_id"] as? String ?? msg["id"] as? String ?? "?"
            parts.append(mark("arguments #\(id)", prettyJSON(msg["arguments"] as? String ?? "")))
        case "function_call_output":
            role = "function_call_output"
            let id = msg["call_id"] as? String ?? "?"
            parts.append(mark("output #\(id)", contentText(msg["output"])))
        default:
            let text = contentText(msg["content"])
            if !text.isEmpty { parts.append(text) }
            if let r = msg["reasoning_content"] as? String, !r.isEmpty {
                parts.append(mark("reasoning", r))
            }
            // OpenAI: assistant 的工具调用另挂 tool_calls, 工具结果是 role=tool 的独立消息
            for raw in items(msg["tool_calls"]) {
                guard let call = raw as? [String: Any] else { continue }
                let fn = call["function"] as? [String: Any] ?? [:]
                var label = "tool_call"
                if let n = fn["name"] as? String { label += " · \(n)" }
                if let id = call["id"] as? String { label += " #\(id)" }
                parts.append(mark(label, prettyJSON(fn["arguments"] as? String ?? "")))
            }
            if let id = msg["tool_call_id"] as? String {
                role = "tool"
                parts.insert("↳ 对应 #\(id)", at: 0)
            }
        }

        var label = "[\(index)] \(role.isEmpty ? "message" : role)"
        if let name = msg["name"] as? String { label += " · \(name)" }
        return (label, parts.joined(separator: "\n"))
    }

    /// content 字段 -> 纯文本。String / 分块数组 / 任意 JSON 都吃得下。
    private static func contentText(_ any: Any?) -> String {
        guard let any, !(any is NSNull) else { return "" }
        if let s = any as? String { return s }
        if let list = any as? [Any] {
            return list.map { part($0) }.filter { !$0.isEmpty }.joined(separator: "\n")
        }
        if let d = any as? [String: Any] { return part(d) }
        return jsonText(any)
    }

    private static func part(_ any: Any) -> String {
        guard let p = any as? [String: Any] else { return String(describing: any) }
        switch p["type"] as? String ?? "" {
        case "text", "input_text", "output_text", "summary_text", "reasoning_text":
            return p["text"] as? String ?? ""
        case "thinking":
            return mark("thinking", p["thinking"] as? String ?? "")
        case "redacted_thinking":
            return mark("redacted_thinking", "（已加密, 不可读）")
        case "tool_use", "server_tool_use":
            return mark(anthropicLabel(p), jsonText(p["input"]))
        case "tool_result":
            let id = p["tool_use_id"] as? String ?? "?"
            let bad = (p["is_error"] as? NSNumber)?.boolValue == true ? " ⚠︎ error" : ""
            return mark("tool_result #\(id)\(bad)", contentText(p["content"]))
        case "image", "input_image":
            let src = p["source"] as? [String: Any] ?? [:]
            if let url = p["image_url"] as? String ?? src["url"] as? String {
                return mark("image", String(url.prefix(120)))
            }
            let media = src["media_type"] as? String ?? "?"
            let bytes = (src["data"] as? String)?.count ?? 0
            return mark("image", "\(media), base64 \(bytes) 字符")
        case "image_url":
            let url = (p["image_url"] as? [String: Any])?["url"] as? String ?? ""
            return mark("image", String(url.prefix(120)))
        case "document":
            return mark("document", jsonText(p["source"], pretty: false).prefix(200).description)
        default:
            if let t = p["text"] as? String { return t }
            return jsonText(p)
        }
    }

    // MARK: - 组装

    private static let encryptedThinking = "（思考内容被上游加密, 只回了签名）"

    private struct Block {
        var label: String
        var body: String
        var isJSON = false // 增量拼出来的字符串, 渲染前尝试格式化
        var note = "" // 正文为空时的说明 (「空」与「拿不到」不是一回事)
    }

    private struct Doc {
        var head: [String] = []
        var blocks: [Block] = []

        func render() -> String {
            var out = head
            if blocks.isEmpty { out.append("\n（本次没有内容产出）") }
            for b in blocks {
                out.append("")
                out.append(rule(b.label))
                let body = b.isJSON ? prettyJSON(b.body) : b.body
                if body.isEmpty {
                    out.append(b.note.isEmpty ? "（空）" : b.note)
                } else {
                    out.append(clampBlock(body))
                }
            }
            return cap(out.joined(separator: "\n"))
        }
    }

    /// 增量事件按 key 归并到同一段, 顺序 = 首次出现顺序。
    private struct Builder {
        var head = ""
        var model = ""
        var id = ""
        var stop = ""
        var usage: String?
        var done = false
        var frameCount = 0
        var frameBreakdown = ""
        var notes: [String] = []
        var blocks: [Block] = []
        private var index: [String: Int] = [:]

        /// `blockLabelWins` = 用这次的 label 覆盖既有的 (start 事件比 delta 知道得多)。
        mutating func append(key: String, label: String, _ chunk: String = "",
                             blockLabelWins: Bool = false) {
            if let i = index[key] {
                if blockLabelWins { blocks[i].label = label }
                blocks[i].body += chunk
                return
            }
            index[key] = blocks.count
            blocks.append(Block(label: label, body: chunk))
        }

        mutating func markJSON(_ key: String) {
            if let i = index[key] { blocks[i].isJSON = true }
        }

        mutating func note(_ key: String, _ text: String) {
            if let i = index[key] { blocks[i].note = text }
        }

        func isEmptyBlock(_ key: String) -> Bool {
            guard let i = index[key] else { return true }
            return blocks[i].body.isEmpty
        }

        func doc() -> Doc {
            var d = Doc()
            var line = head
            if frameCount > 0 {
                line += " · SSE \(frameCount) 帧"
                line += done ? " · 已完整收尾" : " · ⚠︎ 未见收尾事件"
            }
            d.head.append(line)

            var facts: [String] = []
            if !model.isEmpty { facts.append("model \(model)") }
            if !id.isEmpty { facts.append("id \(id)") }
            if !stop.isEmpty { facts.append("stop \(stop)") }
            if !facts.isEmpty { d.head.append(facts.joined(separator: " · ")) }
            if let usage, !usage.isEmpty { d.head.append("usage \(usage)") }
            if !frameBreakdown.isEmpty { d.head.append("帧 \(frameBreakdown)") }

            d.blocks = blocks
            if !notes.isEmpty {
                d.blocks.append(Block(label: "其余帧", body: notes.joined(separator: "\n")))
            }
            return d
        }
    }

    private static func appendBlock(_ doc: inout Doc, _ label: String, text: String) {
        guard !text.isEmpty else { return }
        doc.blocks.append(Block(label: label, body: text))
    }

    /// 错误信封 (两家方言 + 本代理自产) -> 一段; 返回是否命中。
    private static func errorBlock(_ v: [String: Any], into b: inout Builder) -> Bool {
        let err: Any? = v["error"] ?? (v["type"] as? String == "error" ? v : nil)
        guard let err, !(err is NSNull) else { return false }
        b.append(key: "error#\(b.blocks.count)", label: "⚠︎ error", jsonText(err))
        return true
    }

    // MARK: - 排版工具

    private static let ruleWidth = 56

    private static func rule(_ label: String) -> String {
        let used = 4 + width(label) // `── ` 占 3 列 + 尾随空格
        return "── \(label) " + String(repeating: "─", count: max(3, ruleWidth - used))
    }

    /// 等宽字体下的粗略显示宽度: 非 ASCII 一律按双宽算 (中日文标签才需要, 精度够用)。
    private static func width(_ s: String) -> Int {
        s.unicodeScalars.reduce(0) { $0 + ($1.isASCII ? 1 : 2) }
    }

    /// 嵌套内容: 一行标记 + 缩进正文。
    private static func mark(_ label: String, _ body: String) -> String {
        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return "▸ \(label)" }
        if !text.contains("\n"), text.count <= 100 { return "▸ \(label)  \(text)" }
        return "▸ \(label)\n" + indent(text, 4)
    }

    private static func indent(_ s: String, _ n: Int) -> String {
        let pad = String(repeating: " ", count: n)
        return s.split(separator: "\n", omittingEmptySubsequences: false)
            .map { pad + $0 }
            .joined(separator: "\n")
    }

    // MARK: - JSON 工具

    private static func object(_ any: Any?) -> [String: Any]? { any as? [String: Any] }

    private static func items(_ any: Any?) -> [Any] { any as? [Any] ?? [] }

    private static func int(_ any: Any?) -> Int { (any as? NSNumber)?.intValue ?? 0 }

    private static func json(_ text: String) -> Any? {
        guard let d = text.data(using: .utf8) else { return nil }
        return try? JSONSerialization.jsonObject(with: d, options: [.fragmentsAllowed])
    }

    private static func jsonText(_ value: Any?, pretty: Bool = true) -> String {
        guard let value, !(value is NSNull) else { return "" }
        if let s = value as? String { return s }
        var opts: JSONSerialization.WritingOptions = [.withoutEscapingSlashes, .fragmentsAllowed]
        if pretty { opts.formUnion([.prettyPrinted, .sortedKeys]) }
        guard let d = try? JSONSerialization.data(withJSONObject: value, options: opts),
              let s = String(data: d, encoding: .utf8)
        else { return String(describing: value) }
        return s
    }

    /// 工具参数是逐帧拼出来的 JSON 文本: 拼完整就缩进展示, 没拼完 (流被截断) 就原样。
    private static func prettyJSON(_ s: String) -> String {
        let text = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let v = json(text) else { return s }
        return jsonText(v)
    }

    private static func scalar(_ v: Any) -> String {
        if let s = v as? String { return s }
        if let n = v as? NSNumber { return n.stringValue }
        return jsonText(v, pretty: false)
    }

    private static func isEmptyObject(_ v: Any?) -> Bool {
        if let d = v as? [String: Any] { return d.isEmpty }
        if let s = v as? String { return s.isEmpty }
        return v == nil || v is NSNull
    }

    /// usage 摊平成一行: 只留非 0 计数 (上游把没用上的计数也一并回, 全留下就没法看),
    /// 字符串项 (service_tier / inference_geo) 是投放元信息, 不是用量, 一律不进这行。
    private static func usageLine(_ any: Any?) -> String? {
        guard let d = any as? [String: Any] else { return nil }
        var out: [String] = []
        flatten(d, prefix: "", into: &out)
        return out.joined(separator: " · ")
    }

    private static func flatten(_ d: [String: Any], prefix: String, into out: inout [String]) {
        for key in d.keys.sorted() {
            let name = prefix.isEmpty ? key : "\(prefix).\(key)"
            switch d[key] {
            case let n as NSNumber where n.intValue != 0:
                out.append("\(name.hasSuffix("_tokens") ? String(name.dropLast(7)) : name) \(n.intValue)")
            case let sub as [String: Any]:
                flatten(sub, prefix: name, into: &out)
            default:
                break
            }
        }
    }

    // MARK: - 回退与截断

    private static func fallback(_ body: Any?) -> String {
        guard let body, !(body is NSNull) else { return "（空）" }
        if let s = body as? String { return cap(s) }
        return cap("（无对话结构, 原样展示）\n" + jsonText(body))
    }

    /// 单段上限: 一个超大 tool_result 不能把后面的段全挤出视野。
    private static let blockCap = 128 * 1024
    private static let totalCap = 4 * 1024 * 1024

    private static func clampBlock(_ s: String) -> String { clamp(s, blockCap, "本段已截断") }

    private static func cap(_ s: String) -> String { clamp(s, totalCap, "已截断展示") }

    private static func clamp(_ s: String, _ limit: Int, _ note: String) -> String {
        guard s.utf8.count > limit else { return s }
        let head = String(decoding: Array(s.utf8.prefix(limit)), as: UTF8.self)
        return head + "\n\n… \(note) (完整内容见「原始报文」)"
    }

    // MARK: - 方言

    private enum Dialect {
        case anthropic, chat, responses, unknown

        var name: String {
            switch self {
            case .anthropic: return "Anthropic Messages"
            case .chat: return "OpenAI Chat Completions"
            case .responses: return "OpenAI Responses"
            case .unknown: return "未知方言"
            }
        }
    }

    /// path 先判 (最可靠), 再退回按 body 形状猜。
    private static func dialect(path: String, body: [String: Any]) -> Dialect {
        if path.contains("/messages") { return .anthropic }
        if path.contains("/chat/completions") { return .chat }
        if path.contains("/responses") { return .responses }
        if body["choices"] != nil || (body["object"] as? String)?.hasPrefix("chat.completion") == true {
            return .chat
        }
        if body["output"] is [Any] || body["input"] != nil { return .responses }
        if body["content"] is [Any] || body["system"] != nil { return .anthropic }
        if body["messages"] is [Any] { return .chat }
        return .unknown
    }

    private static let anthropicEvents: Set<String> = [
        "message_start", "message_delta", "message_stop",
        "content_block_start", "content_block_delta", "content_block_stop", "ping",
    ]

    private static func sseDialect(event: String, type: String, payload: [String: Any]) -> Dialect {
        if type.hasPrefix("response.") || event.hasPrefix("response.") { return .responses }
        if anthropicEvents.contains(type) || anthropicEvents.contains(event) { return .anthropic }
        if payload["choices"] != nil { return .chat }
        return .unknown
    }
}
