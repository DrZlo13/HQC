import AppKit

// A single server tab drawn as a browser-style tab: rounded top corners and a
// flush bottom. The selected tab fills with the content background and covers the
// bar's bottom hairline, so it appears attached to the content area below.
final class TabItemView: NSView {

    var onClick: (() -> Void)?

    private let label = NSTextField(labelWithString: "")
    private var isSelectedTab = false
    private var isHovered = false
    private var trackingArea: NSTrackingArea?

    private let cornerRadius: CGFloat = 7
    private let horizontalPadding: CGFloat = 14
    // Single source of truth for the tab's width bounds (reused by the width constraints
    // and by intrinsicContentSize).
    private let minTabWidth: CGFloat = 80
    private let maxTabWidth: CGFloat = 220

    init(title: String) {
        super.init(frame: .zero)
        wantsLayer = true

        label.alignment = .center
        label.lineBreakMode = .byTruncatingTail
        label.maximumNumberOfLines = 1
        label.translatesAutoresizingMaskIntoConstraints = false
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        addSubview(label)

        // Hard width bounds. intrinsicContentSize only *suggests* a width at content-hugging
        // priority, which the pinned label overrides whenever the row has horizontal slack (the
        // label stretches the tab past the cap). A required max-width and a soft min-width make
        // the clamp actually bind, so a long title truncates instead of blowing the tab up.
        let maxWidth = widthAnchor.constraint(lessThanOrEqualToConstant: maxTabWidth)
        maxWidth.priority = .required
        let minWidth = widthAnchor.constraint(greaterThanOrEqualToConstant: minTabWidth)
        minWidth.priority = .defaultLow

        NSLayoutConstraint.activate([
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: horizontalPadding),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -horizontalPadding),
            maxWidth, minWidth,
        ])

        label.stringValue = title
        updateLabelStyle()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func setSelected(_ selected: Bool) {
        guard isSelectedTab != selected else { return }
        isSelectedTab = selected
        if selected { isHovered = false }
        updateLabelStyle()
        // The selected label is semibold (wider than regular), so the tab's intrinsic width
        // changes; AppKit does not know our intrinsicContentSize depends on the label font.
        invalidateIntrinsicContentSize()
        needsDisplay = true
    }

    // Width fits the label (clamped); height is driven by the bar via a constraint.
    override var intrinsicContentSize: NSSize {
        let width = label.intrinsicContentSize.width + horizontalPadding * 2
        return NSSize(width: min(max(width, minTabWidth), maxTabWidth), height: NSView.noIntrinsicMetric)
    }

    private func updateLabelStyle() {
        label.textColor = isSelectedTab ? .labelColor : .secondaryLabelColor
        let weight: NSFont.Weight = isSelectedTab ? .semibold : .regular
        label.font = .systemFont(ofSize: NSFont.systemFontSize, weight: weight)
    }

    // MARK: - Hover

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(rect: bounds,
                                  options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                  owner: self, userInfo: nil)
        addTrackingArea(area)
        trackingArea = area
    }

    override func mouseEntered(with event: NSEvent) {
        guard !isSelectedTab else { return }
        isHovered = true
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
        needsDisplay = true
    }

    override func mouseDown(with event: NSEvent) {
        onClick?()
    }

    // Repaint with theme-appropriate colors when the system appearance changes;
    // layer-backed drawing caches its content otherwise.
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    // MARK: - Drawing (non-flipped: origin bottom-left, top = maxY)

    // Outline of the tab: up the left side, around the rounded top, down the right
    // side. `closed` adds the bottom edge (for filling); leave open for stroking so
    // the bottom hairline is not drawn.
    private func tabPath(closed: Bool) -> NSBezierPath {
        let r = cornerRadius
        let b = bounds
        let path = NSBezierPath()
        path.move(to: NSPoint(x: b.minX, y: b.minY))
        path.line(to: NSPoint(x: b.minX, y: b.maxY - r))
        path.appendArc(withCenter: NSPoint(x: b.minX + r, y: b.maxY - r),
                       radius: r, startAngle: 180, endAngle: 90, clockwise: true)
        path.line(to: NSPoint(x: b.maxX - r, y: b.maxY))
        path.appendArc(withCenter: NSPoint(x: b.maxX - r, y: b.maxY - r),
                       radius: r, startAngle: 90, endAngle: 0, clockwise: true)
        path.line(to: NSPoint(x: b.maxX, y: b.minY))
        if closed { path.close() }
        return path
    }

    override func draw(_ dirtyRect: NSRect) {
        if isSelectedTab {
            NSColor.controlBackgroundColor.setFill()
            tabPath(closed: true).fill()
            // Hairline around the top + sides (not the bottom) for definition.
            let outline = tabPath(closed: false)
            outline.lineWidth = 1
            NSColor.separatorColor.setStroke()
            outline.stroke()
        } else if isHovered {
            let rect = NSRect(x: bounds.minX + 3, y: bounds.minY + 4,
                              width: bounds.width - 6, height: bounds.height - 4)
            let path = NSBezierPath(roundedRect: rect, xRadius: cornerRadius, yRadius: cornerRadius)
            NSColor.labelColor.withAlphaComponent(0.07).setFill()
            path.fill()
        }
    }
}
