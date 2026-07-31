import AppKit

// 程序化主菜单 (无 xib): App / Edit / View / Window。动作走响应链, 落到 MainViewController。
nonisolated enum MainMenu {
    @MainActor
    static func build() -> NSMenu {
        let main = NSMenu()
        let app = "jj-agentic-proxy"

        // App
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        appItem.submenu = appMenu
        appMenu.addItem(withTitle: "About \(app)",
                        action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        let hide = appMenu.addItem(withTitle: "Hide \(app)",
                                   action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        hide.keyEquivalentModifierMask = [.command]
        let hideOthers = appMenu.addItem(withTitle: "Hide Others",
                                        action: #selector(NSApplication.hideOtherApplications(_:)),
                                        keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(withTitle: "Show All",
                        action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        let quit = appMenu.addItem(withTitle: "Quit \(app)",
                                   action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        quit.keyEquivalentModifierMask = [.command]

        // Edit (过滤框与文本面板需要标准剪贴板动作)
        let editItem = NSMenuItem()
        main.addItem(editItem)
        let editMenu = NSMenu(title: "Edit")
        editItem.submenu = editMenu
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")

        // View
        let viewItem = NSMenuItem()
        main.addItem(viewItem)
        let viewMenu = NSMenu(title: "View")
        viewItem.submenu = viewMenu
        viewMenu.addItem(withTitle: "Refresh",
                         action: #selector(MainViewController.refresh(_:)), keyEquivalent: "r")
        viewMenu.addItem(withTitle: "Filter",
                         action: #selector(MainViewController.focusSearch(_:)), keyEquivalent: "f")
        viewMenu.addItem(withTitle: "Toggle Follow",
                         action: #selector(MainViewController.toggleFollow(_:)), keyEquivalent: "t")
        viewMenu.addItem(.separator())
        viewMenu.addItem(withTitle: "CLI Console",
                         action: #selector(MainViewController.openConsole(_:)), keyEquivalent: "l")
        let reveal = viewMenu.addItem(withTitle: "Open Log Folder",
                                      action: #selector(MainViewController.revealLogs(_:)), keyEquivalent: "l")
        reveal.keyEquivalentModifierMask = [.command, .shift]

        // Window
        let winItem = NSMenuItem()
        main.addItem(winItem)
        let winMenu = NSMenu(title: "Window")
        winItem.submenu = winMenu
        winMenu.addItem(withTitle: "Minimize",
                        action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        winMenu.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        NSApp.windowsMenu = winMenu

        return main
    }
}
