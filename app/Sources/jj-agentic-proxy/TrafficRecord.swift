import Foundation

// 一条往返记录 = 日志文件里的一行 JSON。
// 列表只需要摘要字段, 完整 req / res 留在文件里按 (offset, length) 现取 ->
// 单日文件可以很大 (完整 body), 内存只放摘要。
nonisolated struct TrafficRecord: Sendable {
    /// 文件内序号: 选中态跨刷新保持稳定
    let seq: Int
    let offset: UInt64
    let length: Int

    let ts: String // 2026-07-31T10:49:01.086+09:00
    let surface: String
    let method: String
    let path: String
    /// 0 = 没等到响应 (客户端提前断开)
    let status: Int
    let stream: Bool
    let elapsedMs: Int
    let reqBytes: Int
    let resBytes: Int
    let model: String?
    let incomplete: String?
    /// 与进行中那条同 id -> 落盘后选中态不丢; 旧版本写的行没有
    let id: String?
    /// 进行中 (代理内存里, 还没落盘): 取自查看器端口, 不在日志文件里
    let live: Bool
    /// 过滤用: 摘要字段小写拼接
    let haystack: String

    /// 选中态的键: 有 id 用 id (进行中 -> 落盘后键不变), 旧行退回文件内序号
    var key: String { id.map { "i:\($0)" } ?? "s:\(seq)" }

    /// HH:MM:SS.mmm
    var clock: String {
        let parts = ts.split(separator: "T", maxSplits: 1)
        guard parts.count == 2 else { return ts }
        return String(parts[1].prefix(12))
    }

    /// 进行中: 还没响应头 = …, 有了 = 状态码 + …
    var statusText: String {
        if live { return status == 0 ? "…" : "\(status)…" }
        return status == 0 ? "—" : String(status)
    }

    var elapsedText: String {
        elapsedMs < 1000 ? "\(elapsedMs)ms" : String(format: "%.1fs", Double(elapsedMs) / 1000)
    }

    /// 详情页顶栏: 不重复 req / res 字节数 (两个面板标题里已有)
    var summary: String {
        var out = "\(clock) · \(method) \(path) · \(surface) · \(statusText) · \(elapsedText)"
        if let model { out += " · \(model)" }
        if stream { out += " · stream" }
        if live { out += " · 进行中" }
        if let incomplete { out += " · ⚠︎ \(incomplete)" }
        return out
    }

    static func size(_ bytes: Int) -> String {
        switch bytes {
        case 0: return "—"
        case ..<1024: return "\(bytes)B"
        case ..<(1024 * 1024): return String(format: "%.1fKB", Double(bytes) / 1024)
        default: return String(format: "%.1fMB", Double(bytes) / (1024 * 1024))
        }
    }
}

nonisolated enum TrafficParser {
    /// CLI 侧刻意把摘要字段排在 `req` 之前 -> 只解析行首那截, 不碰大 body。
    /// 切不出摘要段 (格式变了) 时回退整行解析, 宁慢不丢记录。
    static func record(line: Data, seq: Int, offset: UInt64) -> TrafficRecord? {
        let head = summaryHead(of: line) ?? line
        guard let obj = try? JSONSerialization.jsonObject(with: head),
              let dict = obj as? [String: Any]
        else { return nil }
        return record(dict: dict, seq: seq, offset: offset, length: line.count, live: false)
    }

    /// `/api/inflight` 的一条: 字段与日志行摘要段同名同义, 只是不在文件里 (offset / length 无意义)。
    static func live(dict: [String: Any]) -> TrafficRecord? {
        guard dict["id"] is String else { return nil }
        return record(dict: dict, seq: -1, offset: 0, length: 0, live: true)
    }

    private static func record(dict: [String: Any], seq: Int, offset: UInt64, length: Int,
                               live: Bool) -> TrafficRecord? {
        guard let ts = dict["ts"] as? String else { return nil }

        let int = { (key: String) -> Int in (dict[key] as? NSNumber)?.intValue ?? 0 }
        let str = { (key: String) -> String in dict[key] as? String ?? "" }
        let model = dict["model"] as? String
        let incomplete = dict["incomplete"] as? String
        let status = int("status")
        let fields = [ts, str("surface"), str("method"), str("path"),
                      status == 0 ? "" : String(status), model ?? "", incomplete ?? "",
                      live ? "进行中 live" : ""]

        return TrafficRecord(
            seq: seq,
            offset: offset,
            length: length,
            ts: ts,
            surface: str("surface"),
            method: str("method"),
            path: str("path"),
            status: status,
            stream: (dict["stream"] as? NSNumber)?.boolValue ?? false,
            elapsedMs: int("elapsed_ms"),
            reqBytes: int("req_bytes"),
            resBytes: int("res_bytes"),
            model: model,
            incomplete: incomplete,
            id: dict["id"] as? String,
            live: live,
            haystack: fields.joined(separator: " ").lowercased()
        )
    }

    /// 截到 `,"req_headers":` (第一个非摘要字段) 之前并补上 `}` -> 一个只含标量字段的小对象。
    private static func summaryHead(of line: Data) -> Data? {
        let marker = Data(#","req_headers":"#.utf8)
        // 摘要段固定在行首几百字节内; 限定搜索窗口, 避免在 MB 级 body 里扫。
        let window = line.prefix(4096)
        guard let range = window.range(of: marker) else { return nil }
        var head = Data(line[line.startIndex..<range.lowerBound])
        head.append(0x7D) // }
        return head
    }
}
