import Foundation

// 路径与 CLI 侧写死的一致: 配置目录 ~/.config/jj-agentic-proxy (XDG_CONFIG_HOME 优先)。
// app 不发明任何路径, 也不写这些文件 -> 唯一数据源是 CLI 落下的往返记录。
nonisolated enum ProxyPaths {
    static var configDir: URL {
        let env = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"] ?? ""
        let base = env.isEmpty
            ? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config", isDirectory: true)
            : URL(fileURLWithPath: env, isDirectory: true)
        return base.appendingPathComponent("jj-agentic-proxy", isDirectory: true)
    }

    static var logDir: URL {
        configDir.appendingPathComponent("log", isDirectory: true)
    }

    /// GUI 启动的 app 没有 shell 的 PATH -> 按固定候选逐个探。
    static func cli() -> URL? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let candidates = [
            "\(home)/.local/bin/jj-agentic-proxy",
            "/opt/homebrew/bin/jj-agentic-proxy",
            "/usr/local/bin/jj-agentic-proxy",
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0) }
            .map { URL(fileURLWithPath: $0) }
    }

    static let cliHint = "找不到 jj-agentic-proxy: 先构建安装到 ~/.local/bin (见工程 workflow.md)"
}
