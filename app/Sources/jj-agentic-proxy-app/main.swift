import AppKit

// 手动装配 NSApplication, 无 Storyboard / @NSApplicationMain
let args = CommandLine.arguments
if let flag = args.firstIndex(of: "--snapshot"), flag + 1 < args.count {
    Snapshot.run(path: args[flag + 1]) // 界面自检: 渲染 PNG 后退出
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular) // 进 Dock、有主菜单、可聚焦
app.run()
