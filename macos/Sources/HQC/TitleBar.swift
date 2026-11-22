import AppKit

// The window's bar, in the window's own title bar (28 pt, no toolbar): after the traffic lights
// the page's ‹ › and the open session's or project's breadcrumbs in small grey text; with two
// or more servers, a switch between them on the right. Both sides are titlebar accessories as
// wide as what they hold, so the rest of the bar is the title bar itself: it drags the window
// and zooms it on a double-click.
final class TitleBar: NSObject {

    var onBack: (() -> Void)?
    var onForward: (() -> Void)?
    var onSelectServer: ((_ index: Int) -> Void)?

    private let backButton = TitleBar.arrow("chevron.left", label: "Back")
    private let forwardButton = TitleBar.arrow("chevron.right", label: "Forward")
    private let titleLabel = NSTextField(labelWithString: "")
    private let serverSwitch = NSSegmentedControl()

    private let leading = NSTitlebarAccessoryViewController()
    private let trailing = NSTitlebarAccessoryViewController()
    private let leadingStack: NSStackView
    private let trailingStack: NSStackView

    private weak var window: NSWindow?

    override init() {
        leadingStack = NSStackView(views: [backButton, forwardButton, titleLabel])
        trailingStack = NSStackView(views: [serverSwitch])
        super.init()

        backButton.target = self
        backButton.action = #selector(backClicked)
        forwardButton.target = self
        forwardButton.action = #selector(forwardClicked)

        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 420).isActive = true

        serverSwitch.controlSize = .small
        serverSwitch.trackingMode = .selectOne
        serverSwitch.target = self
        serverSwitch.action = #selector(serverClicked)

        for stack in [leadingStack, trailingStack] {
            stack.orientation = .horizontal
            stack.alignment = .centerY
            stack.spacing = 0
        }
        leadingStack.setCustomSpacing(8, after: forwardButton)
        leadingStack.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 0, right: 0)
        trailingStack.edgeInsets = NSEdgeInsets(top: 0, left: 0, bottom: 0, right: 10)

        leading.layoutAttribute = .leading
        leading.view = leadingStack
        trailing.layoutAttribute = .trailing
        trailing.view = trailingStack
        fit(leadingStack)
    }

    // A borderless SF Symbol button, as the bar's arrows.
    private static func arrow(_ symbol: String, label: String) -> NSButton {
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)!
            .withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: 13, weight: .medium))!
        let button = NSButton(image: image, target: nil, action: nil)
        button.isBordered = false
        button.contentTintColor = .secondaryLabelColor
        button.toolTip = label
        button.widthAnchor.constraint(equalToConstant: 24).isActive = true
        button.heightAnchor.constraint(equalToConstant: 24).isActive = true
        return button
    }

    // Puts the bar into the window's title bar.
    func install(in window: NSWindow) {
        self.window = window
        window.addTitlebarAccessoryViewController(leading)
        if serverSwitch.segmentCount > 1 { window.addTitlebarAccessoryViewController(trailing) }
    }

    // An accessory follows its view's frame: as wide as what it holds, as high as the bar.
    private func fit(_ stack: NSStackView) {
        stack.setFrameSize(NSSize(width: stack.fittingSize.width, height: 28))
    }

    // MARK: - State

    func setNavigation(canGoBack: Bool, canGoForward: Bool) {
        backButton.isEnabled = canGoBack
        forwardButton.isEnabled = canGoForward
    }

    // The breadcrumbs, `project › session › status`, the separators dimmer than the names.
    func setCrumbs(_ crumbs: [String]) {
        let font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        let name: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.secondaryLabelColor,
                                                    .paragraphStyle: paragraph]
        var separator = name
        separator[.foregroundColor] = NSColor.tertiaryLabelColor

        let text = NSMutableAttributedString()
        for (i, crumb) in crumbs.enumerated() {
            if i > 0 { text.append(NSAttributedString(string: " › ", attributes: separator)) }
            text.append(NSAttributedString(string: crumb, attributes: name))
        }
        titleLabel.attributedStringValue = text
        titleLabel.toolTip = crumbs.isEmpty ? nil : crumbs.joined(separator: " › ")
        fit(leadingStack)
    }

    // The server switch: shown with two or more servers, `selected` highlighted.
    func setServers(_ names: [String], selected: Int?) {
        serverSwitch.segmentCount = names.count
        for (i, name) in names.enumerated() {
            serverSwitch.setLabel(name, forSegment: i)
            serverSwitch.setWidth(0, forSegment: i)
        }
        if let selected, names.indices.contains(selected) {
            serverSwitch.selectedSegment = selected
        }
        fit(trailingStack)

        guard let window else { return }
        let index = window.titlebarAccessoryViewControllers.firstIndex(of: trailing)
        if names.count > 1, index == nil {
            window.addTitlebarAccessoryViewController(trailing)
        } else if names.count <= 1, let index {
            window.removeTitlebarAccessoryViewController(at: index)
        }
    }

    // MARK: - Actions

    @objc private func backClicked() { onBack?() }
    @objc private func forwardClicked() { onForward?() }
    @objc private func serverClicked() { onSelectServer?(serverSwitch.selectedSegment) }
}
