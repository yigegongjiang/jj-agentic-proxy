import AppKit
import Foundation

// 终端命令入口 ~/.local/bin/jj-agentic-proxy -> 本 bundle 内的 CLI (symlink, 不拷贝)。
// app 是唯一副本: 换 app 即换 CLI, 入口自动跟随, 不存在两份二进制版本错位。
// 打开 app 时检查一次, 没装 / 指错就弹窗装 -> 用户全程只点一下, 不需要跑任何安装脚本。
nonisolated enum CLIInstall {
    static let linkName = "jj-agentic-proxy"
    static let cliName = "jj-agentic-proxy-cli"

    static var linkURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/bin/\(linkName)")
    }

    /// bundle 内那份 CLI; `swift build` 的 debug 布局下不存在 -> nil = 整套安装流程跳过 (开发时不打扰)
    static var bundledCLI: URL? {
        guard
            let url = Bundle.main.executableURL?
                .deletingLastPathComponent()
                .appendingPathComponent(cliName),
            FileManager.default.isExecutableFile(atPath: url.path)
        else { return nil }
        return url
    }

    /// app 待在临时位置: 从 dmg 里直接双击 (/Volumes) 或被 Gatekeeper 搬去随机只读路径 (AppTranslocation)。
    /// 此时建的 symlink 卷一卸载就是死链 -> 先让用户把 app 拖进「应用程序」。
    static var isEphemeral: Bool {
        let path = Bundle.main.bundleURL.resolvingSymlinksInPath().path
        return path.hasPrefix("/Volumes/") || path.contains("/AppTranslocation/")
    }

    static var isLinked: Bool {
        guard let cli = bundledCLI else { return false }
        return (try? FileManager.default.destinationOfSymbolicLink(atPath: linkURL.path)) == cli.path
    }

    /// 建目录 + 建链接 + 摘 quarantine, 再实跑一次 `--version` 自检; 返回版本串。
    static func install() throws -> String {
        guard let cli = bundledCLI else {
            throw NSError(domain: "CLIInstall", code: 1, userInfo: [NSLocalizedDescriptionKey: "包体内没有 \(cliName)"])
        }
        let fm = FileManager.default
        try fm.createDirectory(at: linkURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        // createSymbolicLink 不覆盖: 旧链接 / 旧普通文件都先删掉 (fileExists 对死链返回 false, 故两个条件都要看)
        if fm.fileExists(atPath: linkURL.path) || (try? fm.destinationOfSymbolicLink(atPath: linkURL.path)) != nil {
            try fm.removeItem(at: linkURL)
        }
        try fm.createSymbolicLink(at: linkURL, withDestinationURL: cli)
        stripQuarantine()
        return try probeVersion()
    }

    /// 从 dmg 拖进来的 app 带 com.apple.quarantine。viewer 自己已经过了 Gatekeeper, 但终端里跑包体内的 CLI
    /// 会被单独拦一次 -> 就地摘掉。失败 (只读卷 / 不属当前用户) 不阻断安装, 顶多终端第一次多弹一个框。
    private static func stripQuarantine() {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/xattr")
        task.arguments = ["-dr", "com.apple.quarantine", Bundle.main.bundleURL.path]
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice
        try? task.run()
        task.waitUntilExit()
    }

    /// 走 symlink 本身跑 (不是直接跑 bundle 内的路径): 链接指错 / 死链在这里就暴露
    private static func probeVersion() throws -> String {
        let task = Process()
        task.executableURL = linkURL
        task.arguments = ["--version"]
        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe
        task.standardInput = FileHandle.nullDevice
        try task.run()
        let data = (try? pipe.fileHandleForReading.readToEnd()) ?? Data()
        task.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard task.terminationStatus == 0 else {
            throw NSError(
                domain: "CLIInstall", code: Int(task.terminationStatus),
                userInfo: [NSLocalizedDescriptionKey: text.isEmpty ? "退出码 \(task.terminationStatus)" : text]
            )
        }
        return text
    }
}

// 弹窗侧 (MainActor): 窗口起来之后再挂 sheet, 否则可能落到别的 app 后面。
enum CLIInstallPrompt {
    static func runIfNeeded(in window: NSWindow) {
        guard CLIInstall.bundledCLI != nil else { return }
        if CLIInstall.isEphemeral {
            show(
                in: window, style: .warning, title: "先把 app 拖进「应用程序」",
                text: "现在是在临时位置运行的。拖进「应用程序」再打开, 才能装上终端命令。"
            )
            return
        }
        guard !CLIInstall.isLinked else { return }

        show(
            in: window, style: .informational, title: "安装终端命令",
            text: "把 jj-agentic-proxy 装到 ~/.local/bin, 之后终端里可以直接敲。\napp 上的按钮就是同名子命令, 两边是同一套。"
        ) {
            do {
                let version = try CLIInstall.install()
                show(
                    in: window, style: .informational, title: "装好了 (\(version))",
                    text: "终端里敲 jj-agentic-proxy 就能启动, --help 有全部子命令。\n提示找不到命令, 把 ~/.local/bin 加进 PATH。"
                )
            } catch {
                show(
                    in: window, style: .critical, title: "装不上",
                    text: "\(error.localizedDescription)\n\n可以在终端里手动建链接:\nln -sfn \(CLIInstall.bundledCLI?.path ?? "") \(CLIInstall.linkURL.path)"
                )
            }
        }
    }

    private static func show(
        in window: NSWindow, style: NSAlert.Style, title: String, text: String,
        onOK: (@MainActor () -> Void)? = nil
    ) {
        let alert = NSAlert()
        alert.alertStyle = style
        alert.messageText = title
        alert.informativeText = text
        alert.addButton(withTitle: "好")
        // 下一轮 runloop 再跑回调: 结果框也是 sheet, 贴着上一张的关闭动画挂会挂不上去
        alert.beginSheetModal(for: window) { _ in
            guard let onOK else { return }
            DispatchQueue.main.async { MainActor.assumeIsolated { onOK() } }
        }
    }
}
