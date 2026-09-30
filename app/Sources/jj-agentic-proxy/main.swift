import AppKit

// 手动装配 NSApplication, 无 Storyboard / @NSApplicationMain
let args = CommandLine.arguments
if let flag = args.firstIndex(of: "--snapshot"), flag + 1 < args.count {
    // 界面自检: 渲染 PNG 后退出; `--filter <词>` 预填过滤框 -> 首行 = 想看的那条
    let filter = args.firstIndex(of: "--filter").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
    Snapshot.run(path: args[flag + 1], filter: filter)
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular) // 进 Dock、有主菜单、可聚焦
app.run()
