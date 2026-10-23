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
    /// 失败时错误信封里的那句话 (含 200 流中途的 error 事件); 旧版本写的行没有
    let error: String?
    /// 归一后的 token 用量 (input 含缓存命中); 响应不带 usage / 旧版本写的行没有
    let tokens: Tokens?
    /// 与进行中那条同 id -> 落盘后选中态不丢; 旧版本写的行没有
    let id: String?
    /// 进行中 (代理内存里, 还没落盘): 取自查看器端口, 不在日志文件里
    let live: Bool
    /// 过滤用: 摘要字段小写拼接
    let haystack: String

    struct Tokens: Sendable {
        let input: Int
        let output: Int
        let cached: Int
        let cacheWrite: Int
    }

    /// 没成功的往返: 没等到响应 / 4xx 5xx / 中途断开 / 流里报错 (200 开头也算)。进行中的不算。
    var anomaly: Bool {
        !live && (status == 0 || status >= 400 || incomplete != nil || error != nil)
    }

    /// 异常原因, 一句话: 列表 tooltip 与详情告警行共用
    var problem: String? {
        let parts = [incomplete, error].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// 选中态的键: 有 id 用 id (进行中 -> 落盘后键不变), 旧行退回文件内序号
    var key: String { id.map { "i:\($0)" } ?? "s:\(seq)" }

    /// HH:MM:SS.mmm
    var clock: String {
        let parts = ts.split(separator: "T", maxSplits: 1)
        guard parts.count == 2 else { return ts }
        return String(parts[1].prefix(12))
    }

    /// 列表用 HH:MM:SS: 毫秒留给详情
    var shortClock: String { String(clock.prefix(8)) }

    /// 列表用: 去掉 query 与 `/v1` / `/backend-api` 前缀, POST 不写 (绝大多数都是) ->
    /// `chat/completions` 不再被截成 `/v1/...mpletions`。完整路径在详情与过滤里。
    var endpoint: String {
        var p = Substring(path.split(separator: "?", maxSplits: 1).first ?? "")
        for prefix in ["/v1/", "/backend-api/"] where p.hasPrefix(prefix) {
            p = p.dropFirst(prefix.count)
            break
        }
        if p.hasPrefix("/") { p = p.dropFirst() }
        return method == "POST" ? String(p) : "\(method) \(p)"
    }

    /// 进行中: 还没响应头 = …, 有了 = 状态码 + …; 带状态码但没成功 (断开 / 流里报错) 加 ⚠︎
    var statusText: String {
        if live { return status == 0 ? "…" : "\(status)…" }
        if status == 0 { return "—" }
        return problem != nil && status < 400 ? "\(status) ⚠︎" : String(status)
    }

    /// 列表用: `12.3K → 456`
    var tokensText: String {
        guard let t = tokens else { return "—" }
        return "\(Self.count(t.input)) → \(Self.count(t.output))"
    }

    /// 详情用: 带缓存命中率
    var tokensDetail: String? {
        guard let t = tokens else { return nil }
        var out = "输入 \(Self.count(t.input))"
        if t.cached > 0, t.input > 0 { out += " (缓存 \(t.cached * 100 / t.input)%)" }
        out += " → 输出 \(Self.count(t.output))"
        if t.cacheWrite > 0 { out += " · 写缓存 \(Self.count(t.cacheWrite))" }
        return out
    }

    static func count(_ n: Int) -> String {
        switch n {
        case ..<1000: return String(n)
        case ..<1_000_000: return String(format: "%.1fK", Double(n) / 1000)
        default: return String(format: "%.2fM", Double(n) / 1_000_000)
        }
    }

    var elapsedText: String {
        elapsedMs < 1000 ? "\(elapsedMs)ms" : String(format: "%.1fs", Double(elapsedMs) / 1000)
    }

    /// 详情页首行: 结果 + 模型 + 耗时 + 用量 (最常要看的几项, 不被长路径挤出视野)
    var headline: String {
        var out = [live ? (status == 0 ? "进行中" : "\(status) · 进行中") : (status == 0 ? "没等到响应" : String(status))]
        if let model { out.append(model) }
        out.append(elapsedText)
        if let t = tokensDetail { out.append(t) }
        return out.joined(separator: " · ")
    }

    /// 详情页次行: 定位信息。不重复 req / res 字节数 (两个面板标题里已有)
    var summary: String {
        var out = "\(clock) · \(method) \(path) · \(surface)"
        if stream { out += " · stream" }
        if let id { out += " · \(id)" }
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
        let error = dict["error"] as? String
        let status = int("status")
        var tokens: TrafficRecord.Tokens?
        if let t = dict["tokens"] as? [String: Any] {
            let n = { (key: String) -> Int in (t[key] as? NSNumber)?.intValue ?? 0 }
            tokens = .init(input: n("input"), output: n("output"), cached: n("cached"),
                           cacheWrite: n("cache_write"))
        }
        let fields = [ts, str("surface"), str("method"), str("path"),
                      status == 0 ? "" : String(status), model ?? "", incomplete ?? "", error ?? "",
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
            error: error,
            tokens: tokens,
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
