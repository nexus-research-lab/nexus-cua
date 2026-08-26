import AppKit

private let applicationName = "Nexus CUA Native Fixture"
private let secureSentinel = "fixture-secret"

@MainActor
final class FixtureController: NSObject, NSApplicationDelegate, NSTextFieldDelegate,
    NSTableViewDataSource, NSTableViewDelegate
{
    private var generation = 1
    private var counter = 0
    private var checked = false
    private var selection: String?
    private var expanded = false
    private var dropped = false
    private var geometryChanged = false
    private let captureProfile = ProcessInfo.processInfo.environment["NEXUS_CUA_FIXTURE_PROFILE"]
    private let faultHangDuration: TimeInterval = {
        let configured = ProcessInfo.processInfo.environment["NEXUS_CUA_FIXTURE_FAULT_HANG_MS"]
            .flatMap(Double.init) ?? 5_000
        return min(max(configured, 500), 30_000) / 1_000
    }()
    private let faultArmDelay: TimeInterval = {
        let configured = ProcessInfo.processInfo.environment["NEXUS_CUA_FIXTURE_FAULT_ARM_MS"]
            .flatMap(Double.init) ?? 3_000
        return min(max(configured, 500), 10_000) / 1_000
    }()

    private let choices = ["Alpha", "Beta", "Gamma"]
    private var window: NSWindow?
    private var occluderWindow: NSWindow?
    private var textField: NSTextField!
    private var stateLabel: NSTextField!
    private var advancedLabel: NSTextField!
    private var tableView: NSTableView!

    func applicationDidFinishLaunching(_ notification: Notification) {
        installMainMenu()
        createWindow()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        choices.count
    }

    func tableView(
        _ tableView: NSTableView,
        viewFor tableColumn: NSTableColumn?,
        row: Int
    ) -> NSView? {
        let value = choices[row]
        let identifier = NSUserInterfaceItemIdentifier("fixture.choice.\(value.lowercased())")
        let cell = (tableView.makeView(withIdentifier: identifier, owner: self) as? NSTextField)
            ?? label(value)
        cell.identifier = identifier
        cell.stringValue = value
        cell.setAccessibilityLabel(value)
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        let value = choices[row]
        let rowView = NSTableRowView()
        rowView.identifier = NSUserInterfaceItemIdentifier("fixture.choice.row.\(value.lowercased())")
        rowView.setAccessibilityLabel(value)
        return rowView
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = tableView.selectedRow
        selection = row >= 0 ? choices[row] : nil
        updateState()
    }

    func controlTextDidChange(_ notification: Notification) {
        updateState()
    }

    private func installMainMenu() {
        let mainMenu = NSMenu()
        let applicationMenuItem = NSMenuItem()
        let applicationMenu = NSMenu()
        applicationMenu.addItem(
            withTitle: "Quit \(applicationName)",
            action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q"
        )
        applicationMenuItem.submenu = applicationMenu
        mainMenu.addItem(applicationMenuItem)

        let editMenuItem = NSMenuItem()
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(
            withTitle: "Select All",
            action: #selector(NSText.selectAll(_:)),
            keyEquivalent: "a"
        )
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)
        NSApp.mainMenu = mainMenu
    }

    private func createWindow() {
        let frame = initialFrame()
        let window = NSWindow(
            contentRect: frame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "\(applicationName) · Generation \(generation)"
        window.minSize = NSSize(width: 760, height: 620)
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces]
        window.contentView = buildContent()
        if captureProfile != nil {
            window.setFrame(frame, display: false, animate: false)
        }
        window.makeKeyAndOrderFront(nil)
        window.orderFrontRegardless()
        self.window = window
        updateState()
    }

    private func initialFrame() -> NSRect {
        if geometryChanged {
            return NSRect(x: 280, y: 220, width: 900, height: 680)
        }
        switch captureProfile {
        case "1080p":
            return NSRect(x: 48, y: 48, width: 960, height: 540)
        case "4k":
            if let screen = NSScreen.screens.first(where: { screen in
                screen.backingScaleFactor >= 2
                    && screen.visibleFrame.width >= 1920
                    && screen.visibleFrame.height >= 1080
            }) {
                return NSRect(
                    x: screen.visibleFrame.minX,
                    y: screen.visibleFrame.minY,
                    width: 1920,
                    height: 1080
                )
            }
            return NSRect(x: 0, y: 0, width: 1920, height: 1080)
        default:
            return NSRect(x: 180, y: 160, width: 960, height: 720)
        }
    }

    private func buildContent() -> NSView {
        let root = NSView()
        root.setAccessibilityLabel(applicationName)

        let title = label(applicationName)
        title.font = .systemFont(ofSize: 24, weight: .semibold)

        let increment = button("Increment Counter", action: #selector(incrementCounter))
        let replace = button("Replace Fixture Window", action: #selector(replaceWindow))
        let geometry = button("Change Fixture Geometry", action: #selector(changeGeometry))
        let actionRow = row([increment, replace, geometry])
        let minimize = button("Minimize Fixture Window", action: #selector(minimizeWindow))
        let occluder = button("Toggle Fixture Occluder", action: #selector(toggleOccluder))
        let nextDisplay = button("Move Fixture To Next Display", action: #selector(moveToNextDisplay))
        let hangBeforeMutation = button(
            "Hang Before Mutation",
            action: #selector(hangBeforeMutation)
        )
        let hangDuringInvoke = button(
            "Hang During Invoke",
            action: #selector(hangDuringInvoke)
        )
        let desktopRow = row([minimize, occluder])
        let displayRow = row([nextDisplay])
        let faultRow = row([hangBeforeMutation, hangDuringInvoke])

        let textCaption = label("Fixture Text")
        textCaption.setContentHuggingPriority(.required, for: .horizontal)
        textField = NSTextField(string: "ready")
        textField.delegate = self
        textField.identifier = NSUserInterfaceItemIdentifier("fixture.text")
        textField.setAccessibilityLabel("Fixture Text")
        let textRow = row([textCaption, textField])

        let secureCaption = label("Fixture Secure Text")
        secureCaption.setContentHuggingPriority(.required, for: .horizontal)
        let secureField = NSSecureTextField(string: secureSentinel)
        secureField.identifier = NSUserInterfaceItemIdentifier("fixture.secure_text")
        secureField.setAccessibilityLabel("Fixture Secure Text")
        let secureRow = row([secureCaption, secureField])

        let toggle = NSButton(
            checkboxWithTitle: "Enable Feature",
            target: self,
            action: #selector(toggleFeature(_:))
        )
        toggle.identifier = NSUserInterfaceItemIdentifier("fixture.toggle")
        toggle.setAccessibilityLabel("Enable Feature")

        let listCaption = label("Fixture Choices")
        listCaption.setAccessibilityLabel("Fixture Choices")
        tableView = NSTableView()
        tableView.headerView = nil
        tableView.delegate = self
        tableView.dataSource = self
        tableView.allowsMultipleSelection = false
        tableView.allowsEmptySelection = true
        tableView.identifier = NSUserInterfaceItemIdentifier("fixture.choices")
        tableView.setAccessibilityLabel("Fixture Choices")
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("choice"))
        column.width = 180
        tableView.addTableColumn(column)
        let listScroll = NSScrollView()
        listScroll.documentView = tableView
        listScroll.hasVerticalScroller = true
        listScroll.borderType = .bezelBorder
        listScroll.heightAnchor.constraint(equalToConstant: 90).isActive = true

        let disclosure = DisclosureButton(
            title: "Advanced Options",
            target: self,
            action: #selector(toggleExpanded(_:))
        )
        disclosure.onAccessibilityChange = { [weak self] expanded in
            self?.setExpanded(expanded)
        }
        disclosure.bezelStyle = .disclosure
        disclosure.setButtonType(.pushOnPushOff)
        disclosure.identifier = NSUserInterfaceItemIdentifier("fixture.expandable")
        disclosure.setAccessibilityLabel("Advanced Options")
        advancedLabel = label("Advanced option is visible")
        advancedLabel.isHidden = true

        let markerStack = NSStackView()
        markerStack.orientation = .vertical
        markerStack.alignment = .leading
        markerStack.spacing = 8
        for index in 1...20 {
            let marker = label("Scroll Marker \(index)")
            marker.setAccessibilityLabel("Scroll Marker \(index)")
            markerStack.addArrangedSubview(marker)
        }
        let scroll = NSScrollView()
        scroll.documentView = markerStack
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.identifier = NSUserInterfaceItemIdentifier("fixture.scroll")
        scroll.setAccessibilityLabel("Fixture Scroll Region")
        scroll.heightAnchor.constraint(equalToConstant: 112).isActive = true

        let dropZone = DropZoneView()
        dropZone.translatesAutoresizingMaskIntoConstraints = false
        dropZone.setAccessibilityLabel("Drop Zone")
        let dragToken = DragTokenView { [weak self, weak dropZone] point in
            guard let self, let dropZone,
                  let superview = dropZone.superview
            else { return }
            let zoneFrame = superview.convert(dropZone.bounds, from: dropZone)
            if zoneFrame.contains(point) {
                self.dropped = true
                dropZone.accepted = true
                self.updateState()
            }
        }
        dragToken.translatesAutoresizingMaskIntoConstraints = false
        dragToken.setAccessibilityLabel("Drag Token")
        let dragRow = NSView()
        dragRow.translatesAutoresizingMaskIntoConstraints = false
        dragRow.addSubview(dragToken)
        dragRow.addSubview(dropZone)
        NSLayoutConstraint.activate([
            dragRow.heightAnchor.constraint(equalToConstant: 56),
            dragToken.leadingAnchor.constraint(equalTo: dragRow.leadingAnchor),
            dragToken.centerYAnchor.constraint(equalTo: dragRow.centerYAnchor),
            dragToken.widthAnchor.constraint(equalToConstant: 120),
            dragToken.heightAnchor.constraint(equalToConstant: 40),
            dropZone.trailingAnchor.constraint(equalTo: dragRow.trailingAnchor),
            dropZone.centerYAnchor.constraint(equalTo: dragRow.centerYAnchor),
            dropZone.widthAnchor.constraint(equalToConstant: 180),
            dropZone.heightAnchor.constraint(equalToConstant: 48),
        ])

        stateLabel = label("")
        stateLabel.identifier = NSUserInterfaceItemIdentifier("fixture.state")
        stateLabel.maximumNumberOfLines = 2
        stateLabel.lineBreakMode = .byWordWrapping

        let left = columnView([
            title,
            actionRow,
            desktopRow,
            displayRow,
            faultRow,
            textRow,
            secureRow,
            toggle,
            listCaption,
            listScroll,
            disclosure,
            advancedLabel,
        ])
        let right = columnView([scroll, dragRow, stateLabel])
        left.translatesAutoresizingMaskIntoConstraints = false
        right.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(left)
        root.addSubview(right)
        NSLayoutConstraint.activate([
            left.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 24),
            left.topAnchor.constraint(equalTo: root.topAnchor, constant: 24),
            left.bottomAnchor.constraint(lessThanOrEqualTo: root.bottomAnchor, constant: -24),
            left.widthAnchor.constraint(equalToConstant: 500),
            right.leadingAnchor.constraint(equalTo: left.trailingAnchor, constant: 24),
            right.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -24),
            right.topAnchor.constraint(equalTo: root.topAnchor, constant: 88),
            right.bottomAnchor.constraint(lessThanOrEqualTo: root.bottomAnchor, constant: -24),
            right.widthAnchor.constraint(equalToConstant: captureProfile == "4k" ? 1348 : 388),
        ])
        return root
    }

    private func label(_ value: String) -> NSTextField {
        let field = NSTextField(labelWithString: value)
        field.isSelectable = false
        return field
    }

    private func button(_ title: String, action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.bezelStyle = .rounded
        button.identifier = NSUserInterfaceItemIdentifier(
            "fixture.\(title.lowercased().replacingOccurrences(of: " ", with: "_"))"
        )
        button.setAccessibilityLabel(title)
        return button
    }

    private func row(_ views: [NSView]) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = 12
        stack.distribution = .fill
        return stack
    }

    private func columnView(_ views: [NSView]) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.distribution = .fill
        for view in views {
            view.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func updateState() {
        guard stateLabel != nil else { return }
        let selected = selection ?? "none"
        let state = "Fixture State: generation=\(generation); counter=\(counter); "
            + "text=\(textField.stringValue); checked=\(checked); selection=\(selected); "
            + "expanded=\(expanded); dropped=\(dropped)"
        stateLabel.stringValue = state
        stateLabel.setAccessibilityLabel(state)
    }

    @objc private func incrementCounter() {
        counter += 1
        updateState()
    }

    @objc private func toggleFeature(_ sender: NSButton) {
        checked = sender.state == .on
        updateState()
    }

    @objc private func toggleExpanded(_ sender: DisclosureButton) {
        let value = sender.state == .on
        sender.expanded = value
        setExpanded(value)
    }

    private func setExpanded(_ value: Bool) {
        expanded = value
        advancedLabel.isHidden = !expanded
        updateState()
    }

    @objc private func replaceWindow() {
        generation += 1
        geometryChanged = false
        let oldWindow = window
        window = nil
        oldWindow?.orderOut(nil)
        oldWindow?.close()
        DispatchQueue.main.async { [weak self] in
            self?.createWindow()
        }
    }

    @objc private func changeGeometry() {
        guard let window else { return }
        geometryChanged.toggle()
        let frame = geometryChanged
            ? NSRect(x: 280, y: 220, width: 900, height: 680)
            : NSRect(x: 180, y: 160, width: 960, height: 720)
        window.setFrame(frame, display: true, animate: false)
    }

    @objc private func minimizeWindow() {
        window?.miniaturize(nil)
    }

    @objc private func toggleOccluder() {
        if let occluderWindow {
            occluderWindow.close()
            self.occluderWindow = nil
            return
        }
        guard let window else { return }
        let occluder = NSWindow(
            contentRect: window.frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        occluder.title = "Nexus CUA Fixture Occluder"
        occluder.backgroundColor = NSColor(
            calibratedRed: 1,
            green: 0,
            blue: 1,
            alpha: 1
        )
        occluder.level = .floating
        occluder.isReleasedWhenClosed = false
        occluder.orderFrontRegardless()
        occluderWindow = occluder
    }

    @objc private func moveToNextDisplay() {
        guard let window, NSScreen.screens.count > 1 else { return }
        let screens = NSScreen.screens
        let currentIndex = screens.firstIndex { screen in
            screen.frame.intersects(window.frame)
        } ?? 0
        let target = screens[(currentIndex + 1) % screens.count].visibleFrame
        let origin = NSPoint(
            x: target.minX + 48,
            y: target.maxY - window.frame.height - 48
        )
        window.setFrameOrigin(origin)
    }

    @objc private func hangBeforeMutation() {
        let duration = faultHangDuration
        DispatchQueue.main.asyncAfter(deadline: .now() + faultArmDelay) {
            Thread.sleep(forTimeInterval: duration)
        }
    }

    @objc private func hangDuringInvoke() {
        Thread.sleep(forTimeInterval: faultHangDuration)
    }
}

private final class DisclosureButton: NSButton {
    var expanded = false
    var onAccessibilityChange: ((Bool) -> Void)?

    override func isAccessibilityExpanded() -> Bool {
        expanded
    }

    override func setAccessibilityExpanded(_ expanded: Bool) {
        self.expanded = expanded
        state = expanded ? .on : .off
        onAccessibilityChange?(expanded)
    }
}

private final class DragTokenView: NSView {
    private let onDrop: (NSPoint) -> Void

    init(onDrop: @escaping (NSPoint) -> Void) {
        self.onDrop = onDrop
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor.systemBlue.cgColor
        layer?.cornerRadius = 8
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        drawCentered("Drag Token", color: .white)
    }

    override func mouseDragged(with event: NSEvent) {
        guard let superview else { return }
        onDrop(superview.convert(event.locationInWindow, from: nil))
    }
}

private final class DropZoneView: NSView {
    var accepted = false {
        didSet { needsDisplay = true }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = 8
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        (accepted ? NSColor.systemGreen : NSColor.systemGray).setFill()
        bounds.fill()
        drawCentered(accepted ? "Dropped" : "Drop Zone", color: .white)
    }
}

private extension NSView {
    func drawCentered(_ value: String, color: NSColor) {
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 14, weight: .medium),
            .foregroundColor: color,
        ]
        let size = value.size(withAttributes: attributes)
        value.draw(
            at: NSPoint(x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2),
            withAttributes: attributes
        )
    }
}

MainActor.assumeIsolated {
    let application = NSApplication.shared
    let controller = FixtureController()
    application.setActivationPolicy(.regular)
    application.delegate = controller
    withExtendedLifetime(controller) {
        application.run()
    }
}
