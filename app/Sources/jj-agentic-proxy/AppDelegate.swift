import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var window: NSWindow!

    // 手动持久化 content 尺寸 (绕开状态恢复冲突); 位置居中于鼠标所在屏 (多显示器友好)
    private static let widthKey = "JJProxyApp.windowW"
    private static let heightKey = "JJProxyApp.windowH"
    private static let defaultSize = NSSize(width: 1180, height: 740)
    private static let minSize = NSSize(width: 900, height: 520)

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build()
        setupWindow()
        NSApp.activate(ignoringOtherApps: true)
        CLIInstallPrompt.runIfNeeded(in: window) // 终端命令没装就当场弹窗装 (装过则静默)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    private func setupWindow() {
        let contentSize = Self.launchContentSize()
        window = NSWindow(
            contentRect: NSRect(origin: .zero, size: contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "jj-agentic-proxy"
        window.contentMinSize = Self.minSize
        window.isRestorable = false
        window.delegate = self
        window.contentViewController = MainViewController()

        window.setContentSize(contentSize)
        let vf = Self.mouseScreen().visibleFrame
        let fs = window.frame.size
        window.setFrameOrigin(NSPoint(x: vf.midX - fs.width / 2, y: vf.midY - fs.height / 2))
        window.makeKeyAndOrderFront(nil)
    }

    private static func launchContentSize() -> NSSize {
        let w = UserDefaults.standard.double(forKey: widthKey)
        let h = UserDefaults.standard.double(forKey: heightKey)
        let size = (w >= minSize.width && h >= minSize.height) ? NSSize(width: w, height: h) : defaultSize
        let vf = mouseScreen().visibleFrame
        return NSSize(width: min(size.width, vf.width), height: min(size.height, vf.height))
    }

    private static func mouseScreen() -> NSScreen {
        let p = NSEvent.mouseLocation
        return NSScreen.screens.first { $0.frame.contains(p) }
            ?? NSScreen.main ?? NSScreen.screens.first ?? NSScreen()
    }

    private func saveWindowSize() {
        guard let window else { return }
        let cs = window.contentRect(forFrameRect: window.frame).size
        UserDefaults.standard.set(cs.width, forKey: Self.widthKey)
        UserDefaults.standard.set(cs.height, forKey: Self.heightKey)
    }

    func windowDidResize(_ notification: Notification) { saveWindowSize() }
    func windowWillClose(_ notification: Notification) { saveWindowSize() }
}
