import Foundation

// 路径与 CLI 侧写死的一致: 配置目录 ~/.config/jj-agentic-traffic (XDG_CONFIG_HOME 优先)。
// app 不发明任何路径, 也不写这些文件 -> 唯一数据源是 CLI 落下的往返记录。
nonisolated enum AppPaths {
    static var configDir: URL {
        let env = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"] ?? ""
        let base = env.isEmpty
            ? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".config", isDirectory: true)
            : URL(fileURLWithPath: env, isDirectory: true)
        return base.appendingPathComponent("jj-agentic-traffic", isDirectory: true)
    }

    /// 与 CLI 的 `store::migrate_legacy_dir` 同一规则: 新目录不在而旧名目录在 -> 整目录 rename, 只搬不合并。
    /// app 先于 CLI 打开时也能直接看到旧往返记录; 两边并发时 rename 原子, 输的一方失败即忽略。
    static func migrateLegacy() {
        let legacy = configDir.deletingLastPathComponent().appendingPathComponent("jj-agentic-proxy", isDirectory: true)
        let fm = FileManager.default
        guard !fm.fileExists(atPath: configDir.path), fm.fileExists(atPath: legacy.path) else { return }
        try? fm.moveItem(at: legacy, to: configDir)
    }

    static var logDir: URL {
        configDir.appendingPathComponent("log", isDirectory: true)
    }

    /// GUI 启动的 app 没有 shell 的 PATH -> 按固定候选逐个探。
    /// 首选与自身同目录那份 (bundle 内 CLI = 权威副本, 不依赖 ~/.local/bin 的 symlink 建没建);
    /// 同一表达式在 `swift build` 的 .build/debug 布局下自然落空 -> 退到已装的入口。
    static func cli() -> URL? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let sibling = Bundle.main.executableURL?
            .deletingLastPathComponent()
            .appendingPathComponent("jj-agentic-traffic-cli").path
        let candidates = [
            sibling,
            "\(home)/.local/bin/jj-agentic-traffic",
            "/opt/homebrew/bin/jj-agentic-traffic",
            "/usr/local/bin/jj-agentic-traffic",
        ].compactMap { $0 }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0) }
            .map { URL(fileURLWithPath: $0) }
    }

    static let cliHint = "找不到 jj-agentic-traffic: 先构建安装 app (CLI 在 app 包体内, 见工程 workflow.md)"
}
