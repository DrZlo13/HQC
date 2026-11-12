import AppKit

// Building blocks of the browser-style tab strip: the bar background, the full-height
// horizontal tab row, and the TabItemView rebuild loop.
enum TabStrip {

    // Apply (and, on appearance changes, re-resolve) the subtle native bar background.
    // A CGColor captured once is not adaptive, so callers also invoke this from
    // viewDidChangeEffectiveAppearance.
    static func applyBackground(to view: NSView) {
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
    }

    // Configure the full-height horizontal tab row. The caller still adds it as a
    // subview and pins its constraints.
    static func configure(stack: NSStackView) {
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = 2
        stack.translatesAutoresizingMaskIntoConstraints = false
    }

    // Remove every TabItemView in `dict` from `stack` and clear the dictionary.
    static func clearItems(_ dict: inout [UUID: TabItemView], from stack: NSStackView) {
        for item in dict.values {
            stack.removeArrangedSubview(item)
            item.removeFromSuperview()
        }
        dict.removeAll()
    }

    // Build one full-height TabItemView, append it to the row, and activate a height
    // constraint so it spans the bar (reaching and covering the bottom hairline). The caller
    // wires onClick and stores it in its id-keyed dictionary.
    static func makeItem(title: String, selected: Bool,
                         in stack: NSStackView, fullHeightOf host: NSView) -> TabItemView {
        let item = TabItemView(title: title)
        item.setSelected(selected)
        stack.addArrangedSubview(item)
        item.heightAnchor.constraint(equalTo: host.heightAnchor).isActive = true
        return item
    }
}
