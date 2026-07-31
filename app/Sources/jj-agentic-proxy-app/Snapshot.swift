import AppKit

// `--snapshot <path.png>`: 离屏渲染主窗口后退出。
// AI Only 工程需要能自检界面, 而截屏 API 要录屏授权 -> 用视图自身的离屏绘制, 无需任何权限。
nonisolated enum Snapshot {
    @MainActor
    static func run(path: String) -> Never {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited) // 不进 Dock, 不抢焦点
        let vc = MainViewController()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1180, height: 740),
            styleMask: [.titled, .resizable],
            backing: .buffered,
            defer: false
        )
        window.contentViewController = vc
        // 设 contentViewController 会把窗口缩到 fitting size -> 显式定回目标尺寸
        window.setContentSize(NSSize(width: 1180, height: 740))
        // 离屏 bitmap 没有窗口背板, 补一层背景色, 否则浅色文字落在透明底上看不见
        window.contentView?.wantsLayer = true
        window.contentView?.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        window.layoutIfNeeded()
        vc.viewDidAppear() // 触发首次加载 (无窗口显示流程时不会自动调用)
        // 等后台扫描 + 详情读取 + status 子进程回来
        RunLoop.current.run(until: Date().addingTimeInterval(2.5))
        window.layoutIfNeeded()

        guard let view = window.contentView,
              let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)
        else { exit(1) }
        view.cacheDisplay(in: view.bounds, to: rep)
        guard let data = rep.representation(using: .png, properties: [:]),
              (try? data.write(to: URL(fileURLWithPath: path))) != nil
        else { exit(1) }
        exit(0)
    }
}
