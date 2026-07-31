// swift-tools-version:6.2
import PackageDescription

// SwiftPM executable, 无 xcodeproj / Storyboard / xib。产物由 package.sh 组装进 .app bundle。
// defaultIsolation(MainActor): AppKit 全程主线程; 日志扫描与子进程输出显式 hop 到后台队列。
// 零第三方依赖: 只用系统 AppKit / Foundation。
let package = Package(
    name: "jj-agentic-proxy",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "jj-agentic-proxy",
            path: "Sources/jj-agentic-proxy",
            swiftSettings: [
                .defaultIsolation(MainActor.self)
            ]
        )
    ]
)
