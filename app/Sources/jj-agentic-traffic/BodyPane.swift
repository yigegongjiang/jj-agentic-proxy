import AppKit

// 一个 body 面板: 标题 + 尺寸 + Copy 按钮 + 等宽只读文本 (自动换行, 长 system 提示词也能读)。
final class BodyPane: NSView {
    private let titleLabel = NSTextField(labelWithString: "")
    private let sizeLabel = NSTextField(labelWithString: "")
    private let copyButton = NSButton()
    // 系统工厂给的 scroll + text view 已配好随宽换行; 手搭时 text view 的初始宽度会叠加到
    // clip view 的宽度上 (autoresizing 按差值伸缩) -> 长行超出面板右缘被裁掉
    private let scroll = NSTextView.scrollableTextView()
    private var textView: NSTextView { scroll.documentView as! NSTextView }

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

    /// 把最后一处 `marker` 顶到面板首行: 长对话的请求体, 要看的是最新那轮, 不是开头的 system。
    /// 全文放得下时不动。
    func scrollToLast(_ marker: String) {
        let range = (text as NSString).range(of: marker, options: .backwards)
        guard range.location != NSNotFound,
              let layout = textView.layoutManager, let container = textView.textContainer
        else { return }
        layout.ensureLayout(for: container)
        let glyphs = layout.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        let y = layout.boundingRect(forGlyphRange: glyphs, in: container).minY
            + textView.textContainerOrigin.y - 4
        let clip = scroll.contentView
        let maxY = max(0, textView.frame.height - clip.bounds.height)
        clip.scroll(to: NSPoint(x: 0, y: min(max(0, y), maxY)))
        scroll.reflectScrolledClipView(clip)
    }

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

        let textView = self.textView
        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.drawsBackground = false
        textView.font = .monospacedSystemFont(ofSize: 11.5, weight: .regular)
        textView.textContainerInset = NSSize(width: 10, height: 8)
        textView.isAutomaticQuoteSubstitutionEnabled = false

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
