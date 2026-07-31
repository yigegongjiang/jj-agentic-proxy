import Foundation

// 跑 CLI 子命令并实时回吐输出。app 侧不实现任何代理能力, 只是按钮 -> 命令。
// 输出每次回吐「全量文本」(累积 Data 重解码): 中文被块边界切断时下一块自动补齐, 无乱码残留。
final class CommandRunner {
    private var process: Process?
    private var output = Data()

    var isRunning: Bool { process != nil }

    /// `onText` 全量文本; `onExit` 退出码 (-1 = 没找到 CLI / 启动失败)。
    func run(
        _ args: [String],
        onText: @escaping @MainActor @Sendable (String) -> Void,
        onExit: @escaping @MainActor @Sendable (Int32) -> Void
    ) {
        guard process == nil else { return }
        output.removeAll(keepingCapacity: false)
        guard let cli = ProxyPaths.cli() else {
            onText(ProxyPaths.cliHint)
            onExit(-1)
            return
        }

        let task = Process()
        task.executableURL = cli
        task.arguments = args
        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = pipe
        task.standardInput = FileHandle.nullDevice

        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.output.append(data)
                onText(String(decoding: self.output, as: UTF8.self))
            }
        }
        task.terminationHandler = { [weak self] proc in
            let code = proc.terminationStatus
            Task { @MainActor [weak self] in
                pipe.fileHandleForReading.readabilityHandler = nil
                // 结束后把管道里的残余读干净, 否则最后几行可能丢
                if let rest = try? pipe.fileHandleForReading.readToEnd(), !rest.isEmpty {
                    self?.output.append(rest)
                    if let text = self?.output { onText(String(decoding: text, as: UTF8.self)) }
                }
                self?.process = nil
                onExit(code)
            }
        }

        do {
            try task.run()
            process = task
        } catch {
            onText("启动失败: \(error.localizedDescription)")
            onExit(-1)
        }
    }

    func cancel() {
        process?.terminate()
    }
}
