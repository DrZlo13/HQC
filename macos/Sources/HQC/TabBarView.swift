import AppKit

// A horizontal strip with browser-style server tabs (one per server) and a trailing
// settings button.
final class TabBarView: NSView {

    var onSelect: ((UUID) -> Void)?
    var onOpenSettings: (() -> Void)?

    private let stackView = NSStackView()
    private let settingsButton = NSButton()
    // Bottom hairline; added before the tabs so the selected tab covers it and
    // appears connected to the content area below.
    private let separator = NSBox()

    // One tab view per server, keyed by server id.
    private var tabsByID: [UUID: TabItemView] = [:]

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setup()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setup()
    }

    private func setup() {
        // Subtle native bar background.
        TabStrip.applyBackground(to: self)

        // Bottom separator first => behind the tabs (the selected tab covers it).
        separator.boxType = .separator
        separator.translatesAutoresizingMaskIntoConstraints = false
        addSubview(separator)

        // Full-height row so tabs reach the bottom edge and connect to the content.
        TabStrip.configure(stack: stackView)
        addSubview(stackView)

        settingsButton.title = "Settings…"
        settingsButton.bezelStyle = .rounded
        settingsButton.setButtonType(.momentaryPushIn)
        settingsButton.target = self
        settingsButton.action = #selector(settingsClicked)
        settingsButton.translatesAutoresizingMaskIntoConstraints = false
        addSubview(settingsButton)

        NSLayoutConstraint.activate([
            stackView.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            stackView.topAnchor.constraint(equalTo: topAnchor),
            stackView.bottomAnchor.constraint(equalTo: bottomAnchor),

            settingsButton.leadingAnchor.constraint(greaterThanOrEqualTo: stackView.trailingAnchor, constant: 8),
            settingsButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            settingsButton.centerYAnchor.constraint(equalTo: centerYAnchor),

            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.bottomAnchor.constraint(equalTo: bottomAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1)
        ])
    }

    // Rebuild the server tabs and highlight the selected one.
    func reload(servers: [Server], selectedID: UUID?) {
        TabStrip.clearItems(&tabsByID, from: stackView)

        for server in servers {
            let id = server.id
            let item = TabStrip.makeItem(title: server.name, selected: id == selectedID,
                                         in: stackView, fullHeightOf: self)
            item.onClick = { [weak self] in self?.onSelect?(id) }
            tabsByID[id] = item
        }
    }

    // Keep the layer background in sync with the system appearance; a CGColor
    // captured once is not adaptive, so re-resolve it on appearance changes.
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        TabStrip.applyBackground(to: self)
    }

    @objc private func settingsClicked() {
        onOpenSettings?()
    }
}
