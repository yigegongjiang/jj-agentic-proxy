import AppKit

// CLI 控制台: 按钮 -> 子命令, 输出实时回吐。app 侧不实现任何能力, 只是替人敲命令。
final class ConsoleWindowController: NSWindowController {
    private static let commands: [(title: String, args: [String], confirm: Bool)] = [
        ("Start", ["start"], false),
        ("Stop", ["stop"], false),
        ("Status", ["status"], false),
        ("Models", ["models"], false),
        ("Login Anthropic", ["login", "anthropic"], false),
        ("Login Codex", ["login", "codex"], false),
        ("Logout All", ["logout", "all"], true),
    ]

    private let runner = CommandRunner()
    private let textView = NSTextView(frame: NSRect(x: 0, y: 0, width: 620, height: 320))
    private let spinner = NSProgressIndicator()
    private let cancelButton = NSButton()
    private let clearButton = NSButton()
    private var commandButtons: [NSButton] = []
    private let onFinish: @MainActor @Sendable () -> Void
    private var header = ""
    private var body = ""

    init(onFinish: @escaping @MainActor @Sendable () -> Void) {
        self.onFinish = onFinish
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 680, height: 460),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "CLI Console"
        window.contentMinSize = NSSize(width: 560, height: 360)
        window.isRestorable = false
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
    }

    required init?(coder: NSCoder) { fatalError("不走 xib") }

    private func buildContent() -> NSView {
        let root = NSView()

        for (index, cmd) in Self.commands.enumerated() {
            let b = NSButton()
            b.title = cmd.title
            b.bezelStyle = .rounded
            b.font = .systemFont(ofSize: 12)
            b.tag = index
            b.target = self
            b.action = #selector(runTapped(_:))
            commandButtons.append(b)
        }
        cancelButton.title = "Cancel"
        cancelButton.bezelStyle = .rounded
        cancelButton.font = .systemFont(ofSize: 12)
        cancelButton.target = self
        cancelButton.action = #selector(cancelTapped)
        cancelButton.isEnabled = false

        clearButton.title = "Clear"
        clearButton.bezelStyle = .rounded
        clearButton.font = .systemFont(ofSize: 12)
        clearButton.target = self
        clearButton.action = #selector(clearTapped)

        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false

        let row1 = NSStackView(views: Array(commandButtons.prefix(4)))
        let row2 = NSStackView(views: Array(commandButtons.dropFirst(4)) + [spinner, cancelButton, clearButton])
        for row in [row1, row2] {
            row.orientation = .horizontal
            row.spacing = 8
            row.alignment = .centerY
            row.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(row)
        }

        textView.isEditable = false
        textView.isRichText = false
        textView.drawsBackground = false
        textView.font = .monospacedSystemFont(ofSize: 11.5, weight: .regular)
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.minSize = .zero
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
        textView.string = "点上面的按钮跑对应的 jj-agentic-proxy 子命令。"

        let scroll = NSScrollView()
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.backgroundColor = .textBackgroundColor
        scroll.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(scroll)

        NSLayoutConstraint.activate([
            row1.topAnchor.constraint(equalTo: root.topAnchor, constant: 12),
            row1.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 12),
            row1.trailingAnchor.constraint(lessThanOrEqualTo: root.trailingAnchor, constant: -12),
            row2.topAnchor.constraint(equalTo: row1.bottomAnchor, constant: 8),
            row2.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 12),
            row2.trailingAnchor.constraint(lessThanOrEqualTo: root.trailingAnchor, constant: -12),
            scroll.topAnchor.constraint(equalTo: row2.bottomAnchor, constant: 12),
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 12),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -12),
        ])
        return root
    }

    @objc private func runTapped(_ sender: NSButton) {
        let cmd = Self.commands[sender.tag]
        if cmd.confirm, !confirm(cmd.title) { return }
        run(cmd.args)
    }

    @objc private func cancelTapped() { runner.cancel() }

    @objc private func clearTapped() {
        header = ""
        body = ""
        textView.string = ""
    }

    private func confirm(_ title: String) -> Bool {
        let a = NSAlert()
        a.messageText = "确认执行 \(title)?"
        a.informativeText = "本地凭证会被删除, 需重新 login 才能继续使用。"
        a.addButton(withTitle: "执行")
        a.addButton(withTitle: "取消")
        a.alertStyle = .warning
        return a.runModal() == .alertFirstButtonReturn
    }

    private func run(_ args: [String]) {
        guard !runner.isRunning else { return }
        header = "$ jj-agentic-proxy \(args.joined(separator: " "))\n"
        body = ""
        render()
        setBusy(true)
        runner.run(
            args,
            onText: { [weak self] text in
                self?.body = text
                self?.render()
            },
            onExit: { [weak self] code in
                guard let self else { return }
                self.setBusy(false)
                self.body += code == 0 ? "\n(完成)\n" : "\n(exit \(code))\n"
                self.render()
                self.onFinish()
            }
        )
    }

    private func render() {
        textView.string = header + body
        textView.scrollToEndOfDocument(nil)
    }

    private func setBusy(_ busy: Bool) {
        commandButtons.forEach { $0.isEnabled = !busy }
        cancelButton.isEnabled = busy
        if busy { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
    }
}
