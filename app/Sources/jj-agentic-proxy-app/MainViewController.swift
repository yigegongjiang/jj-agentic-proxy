import AppKit

// 主界面 = 往返数据浏览器: 左侧一条往返一行 (新 -> 旧), 右侧上下绑定展示同一条的 Request / Response。
// 数据源只有 CLI 落下的 ~/.config/jj-agentic-proxy/log/<日期>.jsonl; app 自己不发起任何代理请求。
final class MainViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate,
                                NSSearchFieldDelegate, NSSplitViewDelegate {
    // MARK: 数据
    private var day = ""
    private var all: [TrafficRecord] = [] // 文件顺序: 旧 -> 新
    private var rows: [TrafficRecord] = [] // 展示顺序: 新 -> 旧
    private var consumed: UInt64 = 0
    private var nextSeq = 0
    private var selectedSeq: Int?
    private var detailToken = 0
    private var scanning = false
    private var followTimer: Timer?
    private var cliOutput = ""
    private var dividersPlaced = false
    private var detail: TrafficReader.Detail?

    // MARK: 视图
    private let dot = NSTextField(labelWithString: "●")
    private let statusLabel = NSTextField(labelWithString: "读取状态…")
    private let credLabel = NSTextField(labelWithString: "")
    private let startButton = NSButton()
    private let stopButton = NSButton()
    private let consoleButton = NSButton()
    private let searchField = NSSearchField()
    private let dayPopup = NSPopUpButton()
    private let followCheck = NSButton(checkboxWithTitle: "Follow", target: nil, action: nil)
    private let countLabel = NSTextField(labelWithString: "")
    private let tableView = NSTableView()
    private let metaLabel = NSTextField(labelWithString: "")
    private let legPicker = NSSegmentedControl(labels: ["Client ↔ Proxy", "Proxy ↔ Upstream"],
                                               trackingMode: .selectOne, target: nil, action: nil)
    private let reqPane = BodyPane(title: "Request")
    private let resPane = BodyPane(title: "Response")
    private let mainSplit = NSSplitView()
    private let detailSplit = NSSplitView()

    private let runner = CommandRunner()
    private var console: ConsoleWindowController?

    // method 并进 endpoint, req / res 字节数合成一列 -> 列数压到能全部塞进左半窗。
    // minW = 该列内容的实际下限: sizeToFit 按比例伸缩时不会把它压到截断。
    private static let columns: [(id: String, title: String, width: CGFloat, minW: CGFloat,
                                  mono: Bool, right: Bool)] = [
        ("time", "Time", 92, 92, true, false),
        ("surface", "Surface", 92, 92, false, false),
        ("path", "Endpoint", 156, 110, false, false), // 唯一可被压缩的列
        ("model", "Model", 104, 104, false, false),
        ("status", "Status", 48, 48, true, true),
        ("took", "Took", 52, 52, true, true),
        ("bytes", "Req / Res", 104, 104, true, true),
    ]

    // MARK: - 装配

    override func loadView() {
        let root = NSView()
        let header = buildHeader()
        let filter = buildFilterRow()
        let split = buildSplit()

        for v in [header, filter, split] {
            v.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(v)
        }
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: root.topAnchor, constant: 10),
            header.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: 24),

            filter.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 10),
            filter.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            filter.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            filter.heightAnchor.constraint(equalToConstant: 24),

            split.topAnchor.constraint(equalTo: filter.bottomAnchor, constant: 10),
            split.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            split.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            split.bottomAnchor.constraint(equalTo: root.bottomAnchor),
        ])
        view = root
    }

    private func buildHeader() -> NSView {
        let box = NSView()
        dot.font = .systemFont(ofSize: 13)
        dot.textColor = .tertiaryLabelColor
        statusLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        credLabel.font = .systemFont(ofSize: 11)
        credLabel.textColor = .secondaryLabelColor
        credLabel.lineBreakMode = .byTruncatingTail
        style(startButton, "Start", #selector(startProxy))
        style(stopButton, "Stop", #selector(stopProxy))
        style(consoleButton, "Console…", #selector(openConsole))

        let items = [dot, statusLabel, credLabel, startButton, stopButton, consoleButton]
        for v in items {
            v.translatesAutoresizingMaskIntoConstraints = false
            box.addSubview(v)
            v.centerYAnchor.constraint(equalTo: box.centerYAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            dot.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 12),
            statusLabel.leadingAnchor.constraint(equalTo: dot.trailingAnchor, constant: 6),
            credLabel.leadingAnchor.constraint(equalTo: statusLabel.trailingAnchor, constant: 14),
            credLabel.trailingAnchor.constraint(lessThanOrEqualTo: startButton.leadingAnchor, constant: -12),
            startButton.trailingAnchor.constraint(equalTo: stopButton.leadingAnchor, constant: -8),
            stopButton.trailingAnchor.constraint(equalTo: consoleButton.leadingAnchor, constant: -8),
            consoleButton.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -12),
            startButton.widthAnchor.constraint(greaterThanOrEqualTo: stopButton.widthAnchor),
        ])
        return box
    }

    private func buildFilterRow() -> NSView {
        let box = NSView()
        searchField.placeholderString = "Filter: path / model / status / surface"
        searchField.delegate = self
        searchField.font = .systemFont(ofSize: 12)

        dayPopup.target = self
        dayPopup.action = #selector(dayChanged)
        dayPopup.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)

        followCheck.target = self
        followCheck.action = #selector(followChanged)
        followCheck.state = .on
        followCheck.font = .systemFont(ofSize: 11)
        followCheck.toolTip = "自动读入新写入的往返记录"

        countLabel.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        countLabel.textColor = .secondaryLabelColor
        countLabel.alignment = .right

        let items: [NSView] = [searchField, dayPopup, followCheck, countLabel]
        for v in items {
            v.translatesAutoresizingMaskIntoConstraints = false
            box.addSubview(v)
            v.centerYAnchor.constraint(equalTo: box.centerYAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            searchField.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 12),
            searchField.trailingAnchor.constraint(equalTo: dayPopup.leadingAnchor, constant: -10),
            dayPopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 116),
            dayPopup.trailingAnchor.constraint(equalTo: followCheck.leadingAnchor, constant: -12),
            followCheck.trailingAnchor.constraint(equalTo: countLabel.leadingAnchor, constant: -12),
            countLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 84),
            countLabel.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -12),
        ])
        return box
    }

    private func buildSplit() -> NSView {
        buildTable()
        let tableScroll = NSScrollView()
        tableScroll.documentView = tableView
        tableScroll.hasVerticalScroller = true
        // 不留横向滚动: 表宽被 clip view 约束住, 富余 / 不足都由末列吸收 -> 没有看不见的列
        tableScroll.hasHorizontalScroller = false
        tableScroll.autohidesScrollers = true
        tableScroll.borderType = .noBorder

        metaLabel.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        metaLabel.textColor = .secondaryLabelColor
        metaLabel.lineBreakMode = .byTruncatingTail
        metaLabel.translatesAutoresizingMaskIntoConstraints = false

        // 同一条往返的两条腿: 客户端看到的 vs 代理真正发给上游的 (header 已注入 / body 可能被改写)
        legPicker.selectedSegment = 0
        legPicker.segmentStyle = .rounded
        legPicker.font = .systemFont(ofSize: 11)
        legPicker.target = self
        legPicker.action = #selector(legChanged)
        legPicker.translatesAutoresizingMaskIntoConstraints = false

        detailSplit.isVertical = false // 上下: Request / Response
        detailSplit.dividerStyle = .thin
        detailSplit.delegate = self
        detailSplit.addArrangedSubview(reqPane)
        detailSplit.addArrangedSubview(resPane)
        detailSplit.translatesAutoresizingMaskIntoConstraints = false

        let detail = NSView()
        detail.addSubview(metaLabel)
        detail.addSubview(legPicker)
        detail.addSubview(detailSplit)
        NSLayoutConstraint.activate([
            metaLabel.topAnchor.constraint(equalTo: detail.topAnchor, constant: 4),
            metaLabel.leadingAnchor.constraint(equalTo: detail.leadingAnchor, constant: 10),
            metaLabel.trailingAnchor.constraint(equalTo: detail.trailingAnchor, constant: -10),
            legPicker.topAnchor.constraint(equalTo: metaLabel.bottomAnchor, constant: 6),
            legPicker.leadingAnchor.constraint(equalTo: detail.leadingAnchor, constant: 10),
            detailSplit.topAnchor.constraint(equalTo: legPicker.bottomAnchor, constant: 6),
            detailSplit.leadingAnchor.constraint(equalTo: detail.leadingAnchor),
            detailSplit.trailingAnchor.constraint(equalTo: detail.trailingAnchor),
            detailSplit.bottomAnchor.constraint(equalTo: detail.bottomAnchor),
        ])

        mainSplit.isVertical = true // 左右: 列表 / 详情
        mainSplit.dividerStyle = .thin
        mainSplit.delegate = self
        mainSplit.addArrangedSubview(tableScroll)
        mainSplit.addArrangedSubview(detail)
        // split view 自身无 intrinsic size -> 给下限, 否则窗口按 fitting size 会塌成两行工具条
        NSLayoutConstraint.activate([
            mainSplit.widthAnchor.constraint(greaterThanOrEqualToConstant: 860),
            mainSplit.heightAnchor.constraint(greaterThanOrEqualToConstant: 420),
        ])
        return mainSplit
    }

    private func style(_ b: NSButton, _ title: String, _ action: Selector) {
        b.title = title
        b.bezelStyle = .rounded
        b.font = .systemFont(ofSize: 12)
        b.target = self
        b.action = action
    }

    private func buildTable() {
        tableView.dataSource = self
        tableView.delegate = self
        tableView.style = .fullWidth
        tableView.rowHeight = 22
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.allowsMultipleSelection = false
        tableView.allowsColumnReordering = false
        tableView.gridStyleMask = []
        // 表宽跟随可视宽度 + 所有列等比缩放 -> 任何窗口尺寸下都不会有列被裁在视野外
        // (代码创建的 table 默认不跟随 clip view, 列宽会按自然合计外溢)
        tableView.autoresizingMask = [.width]
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        for c in Self.columns {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(c.id))
            col.title = c.title
            col.width = c.width
            col.minWidth = c.minW
            col.resizingMask = [.autoresizingMask, .userResizingMask]
            tableView.addTableColumn(col)
        }
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        guard day.isEmpty else { return }
        reloadDays(selecting: nil)
        refreshStatus()
        startFollowTimer()
    }

    /// 分隔条初始位置只能在真实尺寸已知后放 (viewDidAppear 时 split 还没拿到最终 bounds)。
    override func viewDidLayout() {
        super.viewDidLayout()
        guard !dividersPlaced, mainSplit.bounds.width > 600, detailSplit.bounds.height > 200 else { return }
        dividersPlaced = true
        mainSplit.setPosition(mainSplit.bounds.width * 0.62, ofDividerAt: 0)
        detailSplit.setPosition(detailSplit.bounds.height * 0.5, ofDividerAt: 0)
        tableView.sizeToFit() // 按最终列表宽度重排列宽, 不留裁在视野外的列
    }

    // MARK: - 日期 / 扫描

    /// 列出可用日期 (默认最新一天), 并重建该日索引。
    private func reloadDays(selecting wanted: String?) {
        let days = TrafficReader.days()
        let target = wanted ?? (days.contains(day) ? day : days.first)
        dayPopup.removeAllItems()
        dayPopup.addItems(withTitles: days.isEmpty ? ["无记录"] : days)
        dayPopup.isEnabled = !days.isEmpty
        guard let target, days.contains(target) else {
            day = ""
            all = []
            applyFilter()
            return
        }
        dayPopup.selectItem(withTitle: target)
        if target != day {
            day = target
            all = []
            consumed = 0
            nextSeq = 0
        }
        scan(reset: true)
    }

    /// `reset` = 从头重建索引; 否则只读文件新追加的那段。
    private func scan(reset: Bool) {
        guard !scanning, !day.isEmpty else { return }
        scanning = true
        let target = day
        let from = reset ? 0 : consumed
        let seq = reset ? 0 : nextSeq
        Task.detached(priority: .userInitiated) {
            let batch = TrafficReader.scan(day: target, from: from, startSeq: seq)
            await MainActor.run { self.apply(batch, day: target, reset: reset || batch.reset) }
        }
    }

    private func apply(_ batch: TrafficReader.Batch, day target: String, reset: Bool) {
        scanning = false
        guard target == day else { return } // 期间用户切了日期
        if reset {
            all = batch.records
        } else if batch.records.isEmpty {
            consumed = batch.consumed
            return // 没有新记录: 不动表格, 不打断选中与滚动
        } else {
            all.append(contentsOf: batch.records)
        }
        consumed = batch.consumed
        nextSeq = batch.nextSeq
        applyFilter()
    }

    // MARK: - 过滤 / 表格

    private func applyFilter() {
        let terms = searchField.stringValue.lowercased()
            .split(whereSeparator: { $0 == " " || $0 == "\t" })
            .map(String.init)
        let matched = terms.isEmpty
            ? all
            : all.filter { rec in terms.allSatisfy { rec.haystack.contains($0) } }
        rows = matched.reversed()

        tableView.reloadData()
        countLabel.stringValue = terms.isEmpty ? "\(all.count) 条" : "\(rows.count) / \(all.count) 条"

        // 选中态按 seq 恢复: follow 追加新记录时不会跳走
        if let seq = selectedSeq, let idx = rows.firstIndex(where: { $0.seq == seq }) {
            if tableView.selectedRow != idx {
                tableView.selectRowIndexes([idx], byExtendingSelection: false)
            }
        } else if selectedSeq == nil, !rows.isEmpty {
            tableView.selectRowIndexes([0], byExtendingSelection: false)
        } else if rows.isEmpty {
            showDetail(nil)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let column = tableColumn, row < rows.count else { return nil }
        let rec = rows[row]
        let spec = Self.columns.first { $0.id == column.identifier.rawValue }
        let cell = tableView.makeView(withIdentifier: column.identifier, owner: self) as? NSTableCellView
            ?? makeCell(identifier: column.identifier, mono: spec?.mono ?? false, right: spec?.right ?? false)

        let field = cell.textField
        field?.textColor = .labelColor
        switch column.identifier.rawValue {
        case "time": field?.stringValue = rec.clock
        case "surface":
            field?.stringValue = rec.surface
            field?.textColor = .secondaryLabelColor
        case "path": field?.stringValue = "\(rec.method) \(rec.path)"
        case "model": field?.stringValue = rec.model ?? "—"
        case "status":
            field?.stringValue = rec.statusText
            field?.textColor = statusColor(rec)
        case "took": field?.stringValue = rec.elapsedText
        case "bytes":
            field?.stringValue = "\(TrafficRecord.size(rec.reqBytes)) / \(TrafficRecord.size(rec.resBytes))"
        default: field?.stringValue = ""
        }
        return cell
    }

    private func statusColor(_ rec: TrafficRecord) -> NSColor {
        switch rec.status {
        case 0: return .tertiaryLabelColor // 没等到响应
        case 200..<300: return .systemGreen
        case 300..<400: return .systemTeal
        case 400..<500: return .systemOrange
        default: return .systemRed
        }
    }

    private func makeCell(identifier: NSUserInterfaceItemIdentifier, mono: Bool, right: Bool) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = identifier
        let field = NSTextField(labelWithString: "")
        field.font = mono
            ? .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
            : .systemFont(ofSize: 11.5)
        field.lineBreakMode = .byTruncatingMiddle
        field.alignment = right ? .right : .left
        field.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(field)
        cell.textField = field
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            field.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            field.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = tableView.selectedRow
        guard row >= 0, row < rows.count else { return }
        selectedSeq = rows[row].seq
        showDetail(rows[row])
    }

    // MARK: - 详情: 同一条的 req / res 绑定展示

    private func showDetail(_ rec: TrafficRecord?) {
        detailToken += 1
        detail = nil
        guard let rec else {
            metaLabel.stringValue = ""
            reqPane.text = ""
            resPane.text = ""
            reqPane.sizeText = ""
            resPane.sizeText = ""
            return
        }
        metaLabel.stringValue = rec.summary
        reqPane.sizeText = TrafficRecord.size(rec.reqBytes)
        resPane.sizeText = TrafficRecord.size(rec.resBytes)
        reqPane.text = "读取中…"
        resPane.text = ""

        let token = detailToken
        let target = day
        Task.detached(priority: .userInitiated) {
            let loaded = TrafficReader.detail(day: target, offset: rec.offset, length: rec.length)
            await MainActor.run {
                guard token == self.detailToken else { return } // 期间又换了选中行
                self.detail = loaded
                self.renderDetail()
            }
        }
    }

    /// 两条腿共用同一对面板: 只换文本, 不重读文件。
    private func renderDetail() {
        guard let detail else { return }
        legPicker.setEnabled(detail.hasUpstream, forSegment: 1)
        if !detail.hasUpstream, legPicker.selectedSegment == 1 {
            legPicker.selectedSegment = 0
        }
        let upstream = legPicker.selectedSegment == 1
        reqPane.title = upstream ? "Request → 上游" : "Request ← 客户端"
        resPane.title = upstream ? "Response ← 上游" : "Response → 客户端"
        reqPane.text = upstream ? detail.upstreamRequest : detail.clientRequest
        resPane.text = upstream ? detail.upstreamResponse : detail.clientResponse
    }

    @objc private func legChanged() {
        renderDetail()
    }

    // MARK: - follow

    private func startFollowTimer() {
        guard followTimer == nil else { return }
        let t = Timer(timeInterval: 1.5, target: self, selector: #selector(tick), userInfo: nil, repeats: true)
        RunLoop.main.add(t, forMode: .common)
        followTimer = t
    }

    @objc private func tick() {
        guard followCheck.state == .on else { return }
        // 跨零点: 新的一天是新文件, 自动跟到最新
        if let latest = TrafficReader.days().first, latest != day {
            selectedSeq = nil
            reloadDays(selecting: latest)
            return
        }
        scan(reset: false)
    }

    @objc private func followChanged() {
        if followCheck.state == .on { tick() }
    }

    @objc private func dayChanged() {
        guard let picked = dayPopup.titleOfSelectedItem, picked != "无记录" else { return }
        selectedSeq = nil
        reloadDays(selecting: picked)
    }

    // MARK: - 菜单 / 按钮动作

    @objc func refresh(_ sender: Any?) {
        reloadDays(selecting: day.isEmpty ? nil : day)
        refreshStatus()
    }

    @objc func focusSearch(_ sender: Any?) {
        view.window?.makeFirstResponder(searchField)
    }

    @objc func toggleFollow(_ sender: Any?) {
        followCheck.state = followCheck.state == .on ? .off : .on
        followChanged()
    }

    @objc func revealLogs(_ sender: Any?) {
        NSWorkspace.shared.open(ProxyPaths.logDir)
    }

    @objc func openConsole(_ sender: Any?) {
        if console == nil {
            console = ConsoleWindowController(onFinish: { [weak self] in self?.refreshStatus() })
        }
        console?.showWindow(nil)
    }

    func controlTextDidChange(_ obj: Notification) {
        guard obj.object as? NSSearchField === searchField else { return }
        applyFilter()
    }

    // MARK: - 服务状态: 一律经 CLI, app 不复刻任何判断

    @objc private func startProxy() { runControl(["start"]) }
    @objc private func stopProxy() { runControl(["stop"]) }

    private func runControl(_ args: [String]) {
        guard !runner.isRunning else { return }
        startButton.isEnabled = false
        stopButton.isEnabled = false
        statusLabel.stringValue = "\(args[0])…"
        runner.run(
            args,
            onText: { [weak self] text in self?.cliOutput = text },
            onExit: { [weak self] code in
                guard let self else { return }
                self.startButton.isEnabled = true
                if code != 0 {
                    self.alert("\(args.joined(separator: " ")) 失败 (exit \(code))", self.cliOutput)
                }
                self.refreshStatus()
            }
        )
    }

    private func refreshStatus() {
        guard !runner.isRunning else { return }
        runner.run(
            ["status"],
            onText: { [weak self] text in self?.cliOutput = text },
            onExit: { [weak self] _ in
                guard let self else { return }
                self.applyStatus(self.cliOutput)
            }
        )
    }

    private func applyStatus(_ text: String) {
        let lines = text.split(separator: "\n").map(String.init)
        let running = lines.first { $0.contains("运行中") || $0.contains("未运行") } ?? "状态未知"
        let up = running.contains("运行中")
        dot.textColor = up ? .systemGreen : .tertiaryLabelColor
        statusLabel.stringValue = running
        credLabel.stringValue = credSummary(lines)
        startButton.title = up ? "Restart" : "Start"
        startButton.isEnabled = true
        stopButton.isEnabled = up
    }

    /// `- codex: acc | plan=team | ...` -> `codex ✓ team`
    private func credSummary(_ lines: [String]) -> String {
        var out: [String] = []
        for line in lines where line.hasPrefix("- ") {
            let body = line.dropFirst(2)
            guard let colon = body.firstIndex(of: ":") else { continue }
            let name = String(body[body.startIndex..<colon])
            guard name == "codex" || name == "anthropic" else { continue }
            let rest = body[body.index(after: colon)...]
            if rest.contains("未登录") {
                out.append("\(name) ✗")
                continue
            }
            let plan = rest.split(separator: "|")
                .first { $0.contains("plan=") }?
                .replacingOccurrences(of: "plan=", with: "")
                .trimmingCharacters(in: .whitespaces) ?? ""
            out.append(plan.isEmpty || plan == "-" ? "\(name) ✓" : "\(name) ✓ \(plan)")
        }
        return out.joined(separator: "    ")
    }

    private func alert(_ title: String, _ detail: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = detail.isEmpty ? "无输出" : detail
        a.alertStyle = .warning
        a.runModal()
    }

    // MARK: - NSSplitViewDelegate

    func splitView(_ splitView: NSSplitView, constrainMinCoordinate proposed: CGFloat,
                   ofSubviewAt index: Int) -> CGFloat {
        splitView === mainSplit ? 420 : 80
    }

    func splitView(_ splitView: NSSplitView, constrainMaxCoordinate proposed: CGFloat,
                   ofSubviewAt index: Int) -> CGFloat {
        splitView === mainSplit ? splitView.bounds.width - 300 : splitView.bounds.height - 80
    }
}
