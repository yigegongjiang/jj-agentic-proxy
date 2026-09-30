import Foundation

// 进行中的往返只在代理进程内存里 (落盘要等响应结束) -> 经查看器端口取, 不读文件。
// 代理没在跑 / 端口不通 = 没有进行中的记录, 不算错误。
nonisolated enum LiveFeed {
    /// 与 CLI 侧 `provider::UI_PORT` 一致; 只连 loopback (查看器端口恒绑 127.0.0.1)
    static let base = URL(string: "http://127.0.0.1:10020/api/inflight")!

    private static let session: URLSession = {
        let cfg = URLSessionConfiguration.ephemeral
        // 本机回环: 超过 1s 没回就当代理没在跑, 别让 follow 那拍卡住
        cfg.timeoutIntervalForRequest = 1
        cfg.connectionProxyDictionary = [:] // 不走系统代理
        return URLSession(configuration: cfg)
    }()

    /// 旧 -> 新 (请求进来的顺序)。
    static func list() async -> [TrafficRecord] {
        guard let (data, resp) = try? await session.data(from: base),
              (resp as? HTTPURLResponse)?.statusCode == 200,
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let items = obj["records"] as? [[String: Any]]
        else { return [] }
        return items.compactMap(TrafficParser.live(dict:))
    }

    /// 进行中那条的整行快照; 已落盘 (404) / 取不到 = nil。
    static func line(id: String) async -> Data? {
        let url = base.appendingPathComponent(id)
        guard let (data, resp) = try? await session.data(from: url),
              (resp as? HTTPURLResponse)?.statusCode == 200
        else { return nil }
        return data
    }
}
