import AppKit

// 一个 body 面板: 标题 + 尺寸 + Copy 按钮 + 等宽只读文本 (自动换行, 长 system 提示词也能读)。
final class BodyPane: NSView {
    private let titleLabel = NSTextField(labelWithString: "")
    private let sizeLabel = NSTextField(labelWithString: "")
    private let copyButton = NSButton()
    private let textView = NSTextView(frame: NSRect(x: 0, y: 0, width: 480, height: 320))
    private let scroll = NSScrollView()

    var text: String = "" {
        didSet {
            textView.string = text
            if !keepingScroll { textView.scrollRangeToVisible(NSRange(location: 0, length: 0)) }
            copyButton.isEnabled = !text.isEmpty
        }
    }

    /// 同一条的刷新 (进行中逐拍更新 / 落盘后换成最终记录): 保住阅读位置; 原本停在底部的继续贴底 (看流式输出)。
    func update(_ next: String) {
        guard next != text else { return }
        let clip = scroll.contentView
        let origin = clip.bounds.origin
        let atBottom = origin.y > 0 && origin.y + clip.bounds.height >= textView.frame.height - 4
        keepingScroll = true
        text = next
        keepingScroll = false
        if let layout = textView.layoutManager, let container = textView.textContainer {
            layout.ensureLayout(for: container)
        }
        let y = atBottom ? max(0, textView.frame.height - clip.bounds.height) : origin.y
        clip.scroll(to: NSPoint(x: origin.x, y: y))
        scroll.reflectScrolledClipView(clip)
    }

    private var keepingScroll = false

    var sizeText: String {
        get { sizeLabel.stringValue }
        set { sizeLabel.stringValue = newValue }
    }

    var title: String {
        get { titleLabel.stringValue }
        set { titleLabel.stringValue = newValue }
    }

    init(title: String) {
        super.init(frame: .zero)

        titleLabel.stringValue = title
        titleLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        sizeLabel.font = .monospacedDigitSystemFont(ofSize: 10.5, weight: .regular)
        sizeLabel.textColor = .secondaryLabelColor

        copyButton.title = "Copy"
        copyButton.bezelStyle = .rounded
        copyButton.controlSize = .small
        copyButton.font = .systemFont(ofSize: 11)
        copyButton.target = self
        copyButton.action = #selector(copyAll)

        let header = NSStackView(views: [titleLabel, sizeLabel, NSView(), copyButton])
        header.orientation = .horizontal
        header.spacing = 8
        header.edgeInsets = NSEdgeInsets(top: 4, left: 10, bottom: 4, right: 8)
        header.translatesAutoresizingMaskIntoConstraints = false

        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.drawsBackground = false
        textView.font = .monospacedSystemFont(ofSize: 11.5, weight: .regular)
        textView.textContainerInset = NSSize(width: 10, height: 8)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.minSize = .zero
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
        textView.isAutomaticQuoteSubstitutionEnabled = false

        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = true
        scroll.backgroundColor = .textBackgroundColor
        scroll.translatesAutoresizingMaskIntoConstraints = false

        addSubview(header)
        addSubview(scroll)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: topAnchor),
            header.leadingAnchor.constraint(equalTo: leadingAnchor),
            header.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("不走 xib") }

    @objc private func copyAll() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}
