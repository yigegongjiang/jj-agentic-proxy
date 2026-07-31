import Foundation

// 读 CLI 落下的往返记录: 一个日期一个 .jsonl, 一行一次往返。
// 只读不写 (app 不是数据源), 扫描按块流式进行 -> 单日文件多大都不整读进内存。
nonisolated enum TrafficReader {
    static let ext = "jsonl"
    private static let chunkSize = 1 << 20

    /// 已有记录的日期, 新 -> 旧。
    static func days() -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: ProxyPaths.logDir.path)) ?? []
        return names
            .filter { $0.hasSuffix(".\(ext)") && $0.count == "0000-00-00.\(ext)".count }
            .map { String($0.dropLast(ext.count + 1)) }
            .sorted(by: >)
    }

    static func file(day: String) -> URL {
        ProxyPaths.logDir.appendingPathComponent("\(day).\(ext)", isDirectory: false)
    }

    struct Batch: Sendable {
        var records: [TrafficRecord] = []
        var consumed: UInt64 = 0
        var nextSeq: Int = 0
        /// 文件被换掉 (变短) -> 调用方需丢弃已有记录重建
        var reset = false
    }

    /// 从 `from` 字节处继续索引 -> follow 模式只读新追加的部分。
    static func scan(day: String, from: UInt64, startSeq: Int) -> Batch {
        let url = file(day: day)
        let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int ?? 0) ?? 0
        var batch = Batch(consumed: from, nextSeq: startSeq)
        var cursor = from
        if UInt64(size) < from { // 文件被替换 / 清理过
            batch.reset = true
            batch.nextSeq = 0
            cursor = 0
        }
        guard UInt64(size) > cursor, let fh = try? FileHandle(forReadingFrom: url) else {
            batch.consumed = cursor
            return batch
        }
        defer { try? fh.close() }
        try? fh.seek(toOffset: cursor)

        var seq = batch.nextSeq
        var pending = [UInt8]()
        var lineStart = cursor
        var base = cursor // 当前块首字节的绝对偏移

        while let chunk = try? fh.read(upToCount: chunkSize), !chunk.isEmpty {
            let bytes = [UInt8](chunk)
            var i = 0
            while i < bytes.count {
                guard let nl = bytes[i...].firstIndex(of: 0x0A) else {
                    pending.append(contentsOf: bytes[i...])
                    break
                }
                pending.append(contentsOf: bytes[i..<nl])
                if !pending.isEmpty,
                   let rec = TrafficParser.record(line: Data(pending), seq: seq, offset: lineStart) {
                    batch.records.append(rec)
                    seq += 1
                }
                pending.removeAll(keepingCapacity: true)
                lineStart = base + UInt64(nl) + 1
                i = nl + 1
            }
            base += UInt64(bytes.count)
        }
        // 尾部未换行的半行留给下一次 (写入方正在追加)
        batch.consumed = lineStart
        batch.nextSeq = seq
        return batch
    }

    /// 一条腿的两种读法: 原始报文 (起始行 + header + 空行 + body) 与核心内容 (语义摘出来的纯文本)。
    /// 两份都在后台一次算好 -> 界面切视图不重读文件、不重新解析。
    struct Leg: Sendable {
        var raw = ""
        var core = ""

        func text(core wantCore: Bool) -> String { wantCore ? core : raw }
    }

    /// 一条往返的两条腿。
    struct Detail: Sendable {
        var clientRequest = Leg()
        var clientResponse = Leg()
        var upstreamRequest = Leg()
        var upstreamResponse = Leg()
        var hasUpstream = false
    }

    /// 按 (offset, length) 现取整行 -> 只在选中某条时才付出解析大 body 的代价。
    static func detail(day: String, offset: UInt64, length: Int) -> Detail {
        guard let fh = try? FileHandle(forReadingFrom: file(day: day)) else {
            return Detail(clientRequest: both("读不到日志文件"))
        }
        defer { try? fh.close() }
        try? fh.seek(toOffset: offset)
        guard let data = try? fh.read(upToCount: length),
              let obj = try? JSONSerialization.jsonObject(with: data),
              let dict = obj as? [String: Any]
        else {
            return Detail(clientRequest: both("这一行解析失败 (可能正在写入)"))
        }

        let method = dict["method"] as? String ?? "?"
        let path = dict["path"] as? String ?? "?"
        var out = Detail(
            clientRequest: Leg(
                raw: http(start: "\(method) \(path)", headers: dict["req_headers"], body: dict["req"]),
                core: CoreContent.request(path: path, body: dict["req"])
            ),
            clientResponse: Leg(
                raw: http(start: status(dict["status"]), headers: dict["res_headers"], body: dict["res"]),
                core: CoreContent.response(path: path, body: dict["res"])
            )
        )

        guard let up = dict["upstream"] as? [String: Any] else { return out }
        out.hasUpstream = true
        let upMethod = up["method"] as? String ?? "?"
        let upURL = up["url"] as? String ?? "?"
        // req_body 只在本层改写过时才落盘 -> 缺席即等同客户端请求体
        let upBody: Any? = up["req_body"] ?? dict["req"]
        let note = up["req_body"] == nil ? "（请求体未被本层改写, 与客户端一致）\n" : ""
        out.upstreamRequest = Leg(
            raw: note + http(start: "\(upMethod) \(upURL)", headers: up["req_headers"], body: upBody),
            core: note + CoreContent.request(path: upURL, body: upBody)
        )
        out.upstreamResponse = both(http(
            start: status(up["status"]),
            headers: up["res_headers"],
            body: nil,
            bodyNote: "（上游响应体不单独记录: 透传面与客户端一致, 转换面见 Client 的 Response）"
        ))
        return out
    }

    /// 没有第二种读法的腿 (纯提示 / 只有 header): 两个视图给同一份。
    private static func both(_ text: String) -> Leg { Leg(raw: text, core: text) }

    private static func status(_ code: Any?) -> String {
        let n = (code as? NSNumber)?.intValue ?? 0
        return n == 0 ? "HTTP —  (没等到响应)" : "HTTP \(n)"
    }

    /// 起始行 + header (按名字排序) + 空行 + body -> 就是一段可读的 HTTP 报文。
    private static func http(start: String, headers: Any?, body: Any?, bodyNote: String = "") -> String {
        var lines = [start]
        if let map = headers as? [String: Any] {
            for key in map.keys.sorted() {
                lines.append("\(key): \(map[key] ?? "")")
            }
        }
        lines.append("")
        lines.append(bodyNote.isEmpty ? text(body) : bodyNote)
        return lines.joined(separator: "\n")
    }

    /// JSON body -> 缩进展示 (对象键按字典序, 数组保持原序 = 线上顺序);
    /// 字符串 body (SSE / 纯文本) -> 原样, 不重排不美化。
    private static func text(_ value: Any?) -> String {
        guard let value, !(value is NSNull) else { return "（空）" }
        if let s = value as? String { return cap(s) }
        guard let data = try? JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes, .fragmentsAllowed]
        ), let s = String(data: data, encoding: .utf8) else {
            return cap(String(describing: value))
        }
        return cap(s)
    }

    /// 文本视图排版上限: 超大 body 截断展示, 完整内容仍在文件里。
    private static let displayCap = 4 * 1024 * 1024
    private static func cap(_ s: String) -> String {
        guard s.utf8.count > displayCap else { return s }
        let head = String(decoding: Array(s.utf8.prefix(displayCap)), as: UTF8.self)
        return head + "\n\n… 已截断展示 (完整内容见日志文件)"
    }
}
