import AppKit

// Settings, shown as a sheet on the main window: the server list (a table with Name/Address
// columns plus add/remove/edit), the global shortcut and how long a notification card stays.
final class SettingsWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {

    private let store: ServerStore
    private let tableView = NSTableView()

    private let removeButton = NSButton()
    private let editButton = NSButton()
    private let recorder = ShortcutRecorder()
    // How long a notification card other than a permission request stays on screen.
    private let lifetimeValue = NSTextField(labelWithString: "")
    private let lifetimeStepper = NSStepper()

    // Private pasteboard type used only for intra-table drag-to-reorder of servers.
    private let serverRowType = NSPasteboard.PasteboardType("io.github.drzlo13.hqc.settings.server-row")

    init(store: ServerStore) {
        self.store = store

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 660),
            // Not closable: it is a sheet, closed by Done; ⌘W (performClose:) must not close it.
            styleMask: [.titled, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Settings"
        window.contentMinSize = NSSize(width: 480, height: 580)

        super.init(window: window)

        buildUI()

        NotificationCenter.default.addObserver(self,
                                               selector: #selector(reloadTable),
                                               name: .serversDidChange,
                                               object: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used")
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    // MARK: - UI

    private func buildUI() {
        guard let contentView = window?.contentView else { return }

        // Scrollable table with two columns.
        let scrollView = NSScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder

        let nameColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        nameColumn.title = "Name"
        nameColumn.width = 180
        let urlColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("url"))
        urlColumn.title = "Address"
        urlColumn.width = 320

        tableView.addTableColumn(nameColumn)
        tableView.addTableColumn(urlColumn)
        tableView.dataSource = self
        tableView.delegate = self
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.target = self
        tableView.doubleAction = #selector(editSelected)
        tableView.registerForDraggedTypes([serverRowType])
        tableView.draggingDestinationFeedbackStyle = .gap
        scrollView.documentView = tableView

        // Action buttons. The trailing empty view keeps the buttons left-aligned.
        let addButton = NSButton(title: "+", target: self, action: #selector(addNew))
        addButton.bezelStyle = .rounded

        removeButton.title = "−"
        removeButton.bezelStyle = .rounded
        removeButton.target = self
        removeButton.action = #selector(removeSelected)

        editButton.title = "Edit…"
        editButton.bezelStyle = .rounded
        editButton.target = self
        editButton.action = #selector(editSelected)

        let buttonStack = NSStackView(views: [addButton, removeButton, editButton, NSView()])
        buttonStack.orientation = .horizontal
        buttonStack.spacing = 8
        buttonStack.translatesAutoresizingMaskIntoConstraints = false

        // The global shortcut: the action's name and its recorder; every other key is the
        // client's, set in the client's own Settings.
        let shortcutsLabel = NSTextField(labelWithString: "Shortcut")
        shortcutsLabel.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        shortcutsLabel.translatesAutoresizingMaskIntoConstraints = false
        let shortcutRow = NSStackView(views: [NSTextField(labelWithString: ShortcutMonitor.globalAction.title), recorder])
        shortcutRow.orientation = .horizontal
        shortcutRow.spacing = 12
        shortcutRow.alignment = .firstBaseline
        shortcutRow.translatesAutoresizingMaskIntoConstraints = false
        let shortcutsNote = NSTextField(wrappingLabelWithString:
            "Works from any app. The keys of the other File and Go items are the client's: its Settings ▸ Keys.")
        shortcutsNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        shortcutsNote.textColor = .secondaryLabelColor
        shortcutsNote.translatesAutoresizingMaskIntoConstraints = false

        // How long a notification card other than a permission request stays on screen.
        let notificationsLabel = NSTextField(labelWithString: "Notifications")
        notificationsLabel.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        notificationsLabel.translatesAutoresizingMaskIntoConstraints = false
        lifetimeStepper.minValue = Double(Notifier.lifetimeRange.lowerBound)
        lifetimeStepper.maxValue = Double(Notifier.lifetimeRange.upperBound)
        lifetimeStepper.increment = 1
        lifetimeStepper.valueWraps = false
        lifetimeStepper.integerValue = Notifier.lifetime
        lifetimeStepper.target = self
        lifetimeStepper.action = #selector(changeLifetime)
        lifetimeValue.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        lifetimeValue.alignment = .right
        lifetimeValue.stringValue = "\(Notifier.lifetime) s"
        // As wide as "120 s", so the stepper does not move with the value.
        lifetimeValue.widthAnchor.constraint(equalToConstant: 44).isActive = true
        let lifetimeRow = NSStackView(views: [NSTextField(labelWithString: "Stay on screen"), lifetimeValue,
                                              lifetimeStepper])
        lifetimeRow.orientation = .horizontal
        lifetimeRow.spacing = 8
        lifetimeRow.translatesAutoresizingMaskIntoConstraints = false

        // "Done" closes the settings sheet.
        let closeButton = NSButton(title: "Done", target: self, action: #selector(closeSettings))
        closeButton.bezelStyle = .rounded
        closeButton.keyEquivalent = "\r"
        closeButton.translatesAutoresizingMaskIntoConstraints = false

        contentView.addSubview(scrollView)
        contentView.addSubview(buttonStack)
        contentView.addSubview(shortcutsLabel)
        contentView.addSubview(shortcutRow)
        contentView.addSubview(shortcutsNote)
        contentView.addSubview(notificationsLabel)
        contentView.addSubview(lifetimeRow)
        contentView.addSubview(closeButton)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 16),
            scrollView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            scrollView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),

            buttonStack.topAnchor.constraint(equalTo: scrollView.bottomAnchor, constant: 10),
            buttonStack.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            buttonStack.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),

            shortcutsLabel.topAnchor.constraint(equalTo: buttonStack.bottomAnchor, constant: 20),
            shortcutsLabel.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),

            shortcutRow.topAnchor.constraint(equalTo: shortcutsLabel.bottomAnchor, constant: 8),
            shortcutRow.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            shortcutRow.trailingAnchor.constraint(lessThanOrEqualTo: contentView.trailingAnchor, constant: -16),

            shortcutsNote.topAnchor.constraint(equalTo: shortcutRow.bottomAnchor, constant: 6),
            shortcutsNote.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            shortcutsNote.trailingAnchor.constraint(lessThanOrEqualTo: contentView.trailingAnchor, constant: -16),

            notificationsLabel.topAnchor.constraint(equalTo: shortcutsNote.bottomAnchor, constant: 20),
            notificationsLabel.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),

            lifetimeRow.topAnchor.constraint(equalTo: notificationsLabel.bottomAnchor, constant: 8),
            lifetimeRow.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),

            closeButton.topAnchor.constraint(equalTo: lifetimeRow.bottomAnchor, constant: 20),
            closeButton.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),
            closeButton.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -14)
        ])

        updateButtonState()
    }

    // MARK: - Table data source / delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        return store.servers.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard store.servers.indices.contains(row), let column = tableColumn else { return nil }
        let server = store.servers[row]

        let identifier = NSUserInterfaceItemIdentifier("cell_\(column.identifier.rawValue)")
        let cell: NSTableCellView
        if let reused = tableView.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView {
            cell = reused
        } else {
            cell = NSTableCellView()
            let textField = NSTextField(labelWithString: "")
            textField.translatesAutoresizingMaskIntoConstraints = false
            textField.lineBreakMode = .byTruncatingTail
            cell.addSubview(textField)
            cell.textField = textField
            cell.identifier = identifier
            NSLayoutConstraint.activate([
                textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
                textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
                textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor)
            ])
        }

        if column.identifier.rawValue == "name" {
            cell.textField?.stringValue = server.name
        } else {
            cell.textField?.stringValue = server.url.absoluteString
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        updateButtonState()
    }

    // MARK: - Drag-to-reorder

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
        // Encode the source row so acceptDrop knows which server is being moved.
        let item = NSPasteboardItem()
        item.setString(String(row), forType: serverRowType)
        return item
    }

    func tableView(_ tableView: NSTableView,
                   validateDrop info: NSDraggingInfo,
                   proposedRow row: Int,
                   proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
        // Allow dropping only between rows (a gap), never onto a row.
        return dropOperation == .above ? .move : []
    }

    func tableView(_ tableView: NSTableView,
                   acceptDrop info: NSDraggingInfo,
                   row: Int,
                   dropOperation: NSTableView.DropOperation) -> Bool {
        guard let item = info.draggingPasteboard.pasteboardItems?.first,
              let raw = item.string(forType: serverRowType),
              let sourceRow = Int(raw) else { return false }
        // Ignore drops that would not change the order (same slot, or the gap right after itself).
        guard sourceRow != row, sourceRow != row - 1 else { return false }

        // Remember the dragged server so we can keep it selected after the reload.
        let movedID = store.servers.indices.contains(sourceRow) ? store.servers[sourceRow].id : nil
        store.move(from: sourceRow, to: row)   // posts .serversDidChange -> reloadTable()
        if let movedID, let newIndex = store.servers.firstIndex(where: { $0.id == movedID }) {
            tableView.selectRowIndexes(IndexSet(integer: newIndex), byExtendingSelection: false)
        }
        return true
    }

    private func updateButtonState() {
        let hasSelection = tableView.selectedRow >= 0
        removeButton.isEnabled = hasSelection
        editButton.isEnabled = hasSelection
    }

    @objc private func reloadTable() {
        tableView.reloadData()
        updateButtonState()
    }

    // MARK: - Notifications

    // The stepper saves the time a card stays; the next card takes it.
    @objc private func changeLifetime() {
        Notifier.lifetime = lifetimeStepper.integerValue
        lifetimeValue.stringValue = "\(Notifier.lifetime) s"
    }

    // MARK: - Actions

    @objc private func addNew() {
        presentEditor(title: "New Server", name: "", urlString: "") { [weak self] name, urlString in
            self?.store.add(name: name, urlString: urlString)
        }
    }

    @objc private func editSelected() {
        let row = tableView.selectedRow
        guard store.servers.indices.contains(row) else { return }
        let server = store.servers[row]
        presentEditor(title: "Edit Server",
                      name: server.name,
                      urlString: server.url.absoluteString) { [weak self] name, urlString in
            guard let url = ServerStore.normalizeURL(urlString) else { return }
            let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
            let finalName = trimmed.isEmpty ? (url.host ?? urlString) : trimmed
            self?.store.update(Server(id: server.id, name: finalName, url: url))
        }
    }

    @objc private func removeSelected() {
        let row = tableView.selectedRow
        guard store.servers.indices.contains(row) else { return }
        let server = store.servers[row]

        let alert = NSAlert()
        alert.messageText = "Delete server “\(server.name)”?"
        alert.informativeText = "This action cannot be undone."
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.alertStyle = .warning

        let respond: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            if response == .alertFirstButtonReturn {
                self?.store.remove(id: server.id)
            }
        }

        if let window = window {
            alert.beginSheetModal(for: window, completionHandler: respond)
        } else {
            respond(alert.runModal())
        }
    }

    @objc private func closeSettings() {
        // A recording left on would keep the shortcut paused and its key monitor alive.
        recorder.stopRecording()
        guard let window = window else { return }
        if let sheetParent = window.sheetParent {
            sheetParent.endSheet(window)
        } else {
            window.close()
        }
    }

    // MARK: - Add / edit sheet

    // Presents a small form with Name and Address fields. An address that does not normalize
    // is not saved: the form comes back with what was typed and the error under its title.
    private func presentEditor(title: String,
                               name: String,
                               urlString: String,
                               error: String? = nil,
                               onConfirm: @escaping (String, String) -> Void) {
        let alert = NSAlert()
        alert.messageText = title
        if let error {
            alert.informativeText = error
            alert.alertStyle = .warning
        }
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")

        let nameField = NSTextField(frame: NSRect(x: 0, y: 32, width: 320, height: 24))
        nameField.placeholderString = "Name"
        nameField.stringValue = name

        let urlField = NSTextField(frame: NSRect(x: 0, y: 0, width: 320, height: 24))
        urlField.placeholderString = "https://hqc.example.com"
        urlField.stringValue = urlString

        let accessory = NSView(frame: NSRect(x: 0, y: 0, width: 320, height: 56))
        accessory.addSubview(nameField)
        accessory.addSubview(urlField)
        alert.accessoryView = accessory

        // Put keyboard focus on the field to fix: the address after an error, else the name.
        alert.window.initialFirstResponder = error == nil ? nameField : urlField

        let handleResponse: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            let enteredName = nameField.stringValue
            let enteredURL = urlField.stringValue
            guard ServerStore.normalizeURL(enteredURL) != nil else {
                // Re-present once this sheet has finished closing.
                DispatchQueue.main.async {
                    self?.presentEditor(title: title,
                                        name: enteredName,
                                        urlString: enteredURL,
                                        error: "Enter an http or https address with a host.",
                                        onConfirm: onConfirm)
                }
                return
            }
            onConfirm(enteredName, enteredURL)
        }

        if let window = window {
            alert.beginSheetModal(for: window, completionHandler: handleResponse)
        } else {
            handleResponse(alert.runModal())
        }
    }
}
