import AppKit

// The app's own notification cards, drawn after the macOS banners before Big Sur (10.10–10.15,
// Mojave and Catalina among them): a translucent rounded card at the top right of the screen
// with the menu bar, under it, newest on top; a permission request's Allow and Deny always
// visible in a column on the right, split off by hairlines. Each card is a borderless panel that
// never activates the app or takes the focus, over every app and full-screen space, in the
// system's light or dark appearance.
//
// The banners' measures, from screenshots: 324 × 64 pt for two lines of text, a 1 px outline, a
// 40 pt app icon, a 72 pt column of buttons with 1 pt hairlines, text lines 17 pt apart. The card
// here keeps them; it is wider, for the commands it shows, and its corners are rounder.
final class NotificationCards {

    // The card's width; how far it stays from the screen's right edge and from the menu bar,
    // and from the next card.
    static let cardWidth: CGFloat = 380
    private static let margin: CGFloat = 10
    private static let spacing: CGFloat = 10
    // How often the pointer is looked at while it is over the stack.
    private static let pointerCheck: TimeInterval = 0.2

    // On screen, newest on top; and those waiting for their turn, in the order they came.
    private var shown: [NotificationCard] = []
    private var waiting: [NotificationCard] = []

    // The pointer is over the stack: nothing moves, so what is under the pointer stays there. A
    // new card waits, a card that goes leaves its gap, a card replaced in place keeps its top
    // (keepSlot); the stack settles when the pointer leaves, or when no card is left on screen.
    // `heldArea` is where the cards were when the pointer came, gaps included.
    private var held = false
    private var heldArea = NSRect.null
    private var pointerTimer: Timer?
    // While every card's pointer state is checked at once: their changes are not taken one by one.
    private var checking = false

    // While a Focus is on or the screen is shared (Notifier): no card is on screen, also under
    // the pointer; the cards that were wait with the new ones and come in, in order, when it ends.
    var isQuiet = false {
        didSet { if isQuiet != oldValue { settle() } }
    }

    // Shows a card for an event, or puts the event into the card with the same identifier in its
    // place. A card with a lifetime goes that long after it is shown, or after the pointer last
    // left it; without one it stays until it is removed or closed.
    @discardableResult
    func show(identifier: String, event: PageEvent, lifetime: TimeInterval?) -> NotificationCard {
        if let card = (shown + waiting).first(where: { $0.identifier == identifier }) {
            card.update(event)
            if card.isShown, pointerOverStack() { keepSlot(of: card) }
            settle()
            return card
        }
        let card = NotificationCard(identifier: identifier, event: event, lifetime: lifetime)
        card.onExpire = { [weak self, weak card] in
            if let self, let card { self.remove(card) }
        }
        card.onPointer = { [weak self] in
            if let self, !self.checking { self.settle() }
        }
        waiting.append(card)
        settle()
        return card
    }

    func remove(identifier: String) {
        if let card = (shown + waiting).first(where: { $0.identifier == identifier }) { remove(card) }
    }

    func removeAll(where shouldRemove: (NotificationCard) -> Bool) {
        let goneShown = shown.filter(shouldRemove)
        let goneWaiting = waiting.filter(shouldRemove)
        guard !goneShown.isEmpty || !goneWaiting.isEmpty else { return }
        shown.removeAll { card in goneShown.contains { $0 === card } }
        waiting.removeAll { card in goneWaiting.contains { $0 === card } }
        for card in goneShown { card.slideOut(past: Self.screenEdge, leaving: true) }
        for card in goneWaiting { card.leave() }
        settle()
    }

    private func remove(_ card: NotificationCard) {
        removeAll { $0 === card }
    }

    // A card replaced in place while the stack is held keeps its top if its new height fits its
    // slot: down to the top of the next card on screen less the gap, or to the bottom of the
    // screen's visible area. Otherwise it would cover the card under it: it leaves its slot
    // empty and goes first in the queue, as a card that no longer fits does.
    private func keepSlot(of card: NotificationCard) {
        guard let index = shown.firstIndex(where: { $0 === card }),
              let area = Self.screen?.visibleFrame else { return }
        let bottom = index + 1 < shown.count
            ? shown[index + 1].place.maxY + Self.spacing
            : area.minY + Self.margin
        if card.place.maxY - card.size.height >= bottom {
            card.keepTop()
        } else {
            shown.remove(at: index)
            card.slideOut(past: Self.screenEdge, leaving: false)
            waiting.insert(card, at: 0)
        }
    }

    // The screen with the menu bar.
    private static var screen: NSScreen? { NSScreen.screens.first }
    private static var screenEdge: CGFloat { screen?.frame.maxX ?? 0 }

    // Whether the pointer is over a card on screen, or where the cards were when it came. With no
    // card on screen nothing is held.
    private func pointerOverStack() -> Bool {
        guard !shown.isEmpty else { return false }
        if shown.contains(where: { $0.isHovered }) { return true }
        let cards = shown.reduce(NSRect.null) { $0.union($1.panel.frame) }
        return (held ? heldArea.union(cards) : cards).contains(NSEvent.mouseLocation)
    }

    // Holds the stack while the pointer is over it, looking at the pointer again every
    // `pointerCheck` (it can leave through a gap, where no card hears it, and a card that moved
    // or changed under a still pointer hears nothing either); otherwise stacks the cards.
    private func settle() {
        if !isQuiet, pointerOverStack() {
            if !held {
                held = true
                heldArea = shown.reduce(NSRect.null) { $0.union($1.panel.frame) }
            }
            guard pointerTimer == nil else { return }
            let timer = Timer(timeInterval: Self.pointerCheck, repeats: true) { [weak self] _ in
                self?.checkPointer()
            }
            RunLoop.main.add(timer, forMode: .common)
            pointerTimer = timer
        } else {
            held = false
            heldArea = .null
            pointerTimer?.invalidate()
            pointerTimer = nil
            layout()
        }
    }

    private func checkPointer() {
        checking = true
        for card in shown { card.checkPointer() }
        checking = false
        settle()
    }

    // Stacks the cards down from under the menu bar at the screen's right edge, as many as fit
    // above the bottom of the screen's visible area: the ones on screen keep their order, a card
    // that no longer fits goes back to the head of the queue, and waiting cards come in, in
    // their order, while they fit; each new one slides in from the right on top, the others move
    // to their places.
    private func layout() {
        guard let area = Self.screen?.visibleFrame else { return }
        let room = area.height - 2 * Self.margin
        var used: CGFloat = 0
        var stack: [NotificationCard] = []
        // The first card is shown whatever its height; none while quiet.
        let quiet = isQuiet
        let fits = { (card: NotificationCard) -> Bool in
            !quiet && (stack.isEmpty || used + Self.spacing + card.size.height <= room)
        }
        let take = { (card: NotificationCard) in
            used += (stack.isEmpty ? 0 : Self.spacing) + card.size.height
        }
        for card in shown {
            guard fits(card) else { break }
            take(card)
            stack.append(card)
        }
        let overflow = Array(shown.dropFirst(stack.count))
        for card in overflow { card.slideOut(past: Self.screenEdge, leaving: false) }
        waiting.insert(contentsOf: overflow.reversed(), at: 0)
        var arriving: [NotificationCard] = []
        while let card = waiting.first, fits(card) {
            waiting.removeFirst()
            take(card)
            stack.insert(card, at: 0)
            arriving.append(card)
        }
        shown = stack

        var top = area.maxY - Self.margin
        for card in shown {
            let size = card.size
            let place = NSRect(x: area.maxX - Self.margin - size.width, y: top - size.height,
                               width: size.width, height: size.height)
            if arriving.contains(where: { $0 === card }) {
                card.slideIn(to: place, from: Self.screenEdge)
            } else {
                card.move(to: place)
            }
            top = place.minY - Self.spacing
        }
    }
}

// One card: its panel and what it shows, the timer that takes it away and the pointer over it.
final class NotificationCard {

    let identifier: String
    private(set) var event: PageEvent

    // A click on the card, and Allow (true) or Deny (false).
    var onOpen: (() -> Void)?
    var onAnswer: ((_ allow: Bool) -> Void)?
    // The lifetime ran out, or the close button was clicked.
    fileprivate var onExpire: (() -> Void)?
    // The pointer came onto the card or left it.
    fileprivate var onPointer: (() -> Void)?

    // While the answer goes to the hub: the buttons do nothing.
    var isAnswering = false {
        didSet { view.setButtonsEnabled(!isAnswering) }
    }

    // The pointer is over the card or its close button: the close button shows and the card
    // does not go; the lifetime starts again when the pointer leaves.
    var isHovered = false {
        didSet {
            guard isHovered != oldValue else { return }
            view.showsClose = isHovered
            refreshShadow()
            restartTimer()
            onPointer?()
        }
    }

    // On screen; a card waiting for its turn is not, and its lifetime has not started.
    fileprivate private(set) var isShown = false

    let panel: NSPanel
    private let view: CardView
    private let lifetime: TimeInterval?
    private var timer: DispatchWorkItem?
    // Where the card is on screen (the card itself, without the close button's room).
    fileprivate private(set) var place = NSRect.zero

    // The card's own size; the panel is larger by the close button's room.
    var size: NSSize { view.cardSize }

    init(identifier: String, event: PageEvent, lifetime: TimeInterval?) {
        self.identifier = identifier
        self.event = event
        self.lifetime = lifetime

        let panel = CardPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                              backing: .buffered, defer: true)
        panel.isFloatingPanel = true
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        panel.hidesOnDeactivate = false
        // Hide HQC and another app's Hide Others leave the cards on screen.
        panel.canHide = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.isReleasedWhenClosed = false
        panel.isMovable = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.animationBehavior = .none
        self.panel = panel

        view = CardView(width: NotificationCards.cardWidth)
        panel.contentView = view
        view.card = self
        view.configure(event)
    }

    // A later event with the same identifier: its content, and the lifetime from now.
    func update(_ event: PageEvent) {
        self.event = event
        view.configure(event)
        refreshShadow()
        restartTimer()
        checkPointer()
    }

    // The card's new height while the stack is held: its top stays where it is.
    fileprivate func keepTop() {
        let size = self.size
        move(to: NSRect(x: place.minX, y: place.maxY - size.height, width: size.width, height: size.height))
    }

    // Whether the pointer is over the card, from where it is now rather than from the events of
    // coming and going: a card that moved or changed under a still pointer hears none.
    fileprivate func checkPointer() {
        view.checkPointer()
    }

    // The panel's frame for the card at `place`: the close button's room on the left and on top.
    private func frame(for place: NSRect) -> NSRect {
        let room = CardView.closeRoom
        return NSRect(x: place.minX - room, y: place.minY, width: place.width + room, height: place.height + room)
    }

    // Comes on screen at `place`, sliding in from the screen's right edge; the lifetime starts.
    fileprivate func slideIn(to place: NSRect, from edge: CGFloat) {
        isShown = true
        self.place = place
        let end = frame(for: place)
        var start = end
        start.origin.x = edge
        panel.alphaValue = 1
        panel.setFrame(start, display: true)
        panel.orderFrontRegardless()
        refreshShadow()
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.3
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrame(end, display: true)
        }, completionHandler: { [weak self] in
            self?.checkPointer()
        })
        restartTimer()
    }

    fileprivate func move(to place: NSRect) {
        self.place = place
        let end = frame(for: place)
        guard end != panel.frame else { return }
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.25
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            panel.animator().setFrame(end, display: true)
        }, completionHandler: { [weak self] in
            self?.refreshShadow()
            self?.checkPointer()
        })
    }

    // Slides off to the right, fading, and leaves the screen: for good (`leaving`), or to wait
    // for its turn again when the stack has no room for it.
    fileprivate func slideOut(past edge: CGFloat, leaving: Bool) {
        isShown = false
        timer?.cancel()
        timer = nil
        if leaving { dropCallbacks() }
        let panel = self.panel
        var end = panel.frame
        end.origin.x = max(end.origin.x, edge - CardView.closeRoom)
        NSAnimationContext.runAnimationGroup({ context in
            context.duration = 0.25
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().setFrame(end, display: true)
            panel.animator().alphaValue = 0
        }, completionHandler: { [weak self] in
            // Not when it came back on screen meanwhile.
            if self?.isShown != true { panel.orderOut(nil) }
        })
    }

    // A waiting card that is removed.
    fileprivate func leave() {
        isShown = false
        timer?.cancel()
        timer = nil
        dropCallbacks()
        panel.orderOut(nil)
    }

    private func dropCallbacks() {
        onOpen = nil
        onAnswer = nil
        onExpire = nil
        onPointer = nil
    }

    // The window shadow follows what the panel draws: the card, and the close button when shown.
    private func refreshShadow() {
        view.displayIfNeeded()
        panel.invalidateShadow()
    }

    private func restartTimer() {
        timer?.cancel()
        timer = nil
        guard let lifetime, isShown, !isHovered else { return }
        let item = DispatchWorkItem { [weak self] in self?.onExpire?() }
        timer = item
        DispatchQueue.main.asyncAfter(deadline: .now() + lifetime, execute: item)
    }

    fileprivate func closeClicked() {
        onExpire?()
    }
}

// A panel that is never key or main: a click on a card leaves the focus where it is.
private final class CardPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

// MARK: - The card's view

// The panel's content: the card, with room on the left and on top for the close button that
// sits over the card's top left corner while the pointer is over it. Laid out top down.
private final class CardView: NSView {

    // The close button's room outside the card, its size and its centre's distance from the
    // card's corner.
    static let closeRoom: CGFloat = 8
    private static let closeSize: CGFloat = 18
    private static let closeInset: CGFloat = 4

    // The card's corner radius (its material, its outline, what it shows and so the window's
    // shadow); the project tile's size and corner radius; the paddings; the button column's width.
    private static let radius: CGFloat = 12
    private static let tileSize: CGFloat = 40
    private static let tileRadius: CGFloat = 10
    private static let pad: CGFloat = 12
    private static let top: CGFloat = 9
    private static let bottom: CGFloat = 10
    private static let columnWidth: CGFloat = 72
    // The banner's height for two lines of text: the least a card is.
    private static let minHeight: CGFloat = 64

    // Text on the project's colour, as the client's (client/src/theme.ts `--accent-ink`).
    private static let ink = NSColor(srgbRed: 0x15 / 255.0, green: 0x12 / 255.0, blue: 0x0F / 255.0, alpha: 1)
    // The client's first project colour, for a project not known.
    private static let teal = NSColor(srgbRed: 0x3F / 255.0, green: 0xB8 / 255.0, blue: 0xA8 / 255.0, alpha: 1)

    weak var card: NotificationCard?

    private let width: CGFloat
    private let body = CardBody()
    private let outline = OutlineView()
    private let close = CloseButton()
    private var buttons: [ColumnButton] = []

    private(set) var cardSize: NSSize

    override var isFlipped: Bool { true }

    init(width: CGFloat) {
        self.width = width
        cardSize = NSSize(width: width, height: Self.minHeight)
        super.init(frame: .zero)

        body.material = .popover
        body.blendingMode = .behindWindow
        body.state = .active
        body.maskImage = Self.roundedMask(radius: Self.radius)
        // The mask shapes the material only: the layer cuts the buttons' pressed shade too.
        body.wantsLayer = true
        body.layer?.cornerRadius = Self.radius
        body.layer?.masksToBounds = true
        body.onPointer = { [weak self] in self?.checkPointer() }
        body.onClick = { [weak self] in self?.card?.onOpen?() }
        addSubview(body)

        outline.radius = Self.radius

        close.isHidden = true
        close.onPointer = { [weak self] in self?.checkPointer() }
        close.onClick = { [weak self] in self?.card?.closeClicked() }
        addSubview(close)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    var showsClose: Bool {
        get { !close.isHidden }
        set { close.isHidden = !newValue }
    }

    func setButtonsEnabled(_ enabled: Bool) {
        for button in buttons { button.isEnabled = enabled }
    }

    // The pointer is over the card, or over the close button while it shows; the events of the
    // pointer coming and going only say when to look.
    func checkPointer() {
        guard let window, window.isVisible else {
            card?.isHovered = false
            return
        }
        let point = NSEvent.mouseLocation
        let onScreen = { (view: NSView) in window.convertToScreen(view.convert(view.bounds, to: nil)) }
        card?.isHovered = onScreen(body).contains(point) || (showsClose && onScreen(close).contains(point))
    }

    // What happened, by the event's type.
    private static func kind(_ type: String) -> String {
        switch type {
        case "approval.request": return "Permission"
        case "clarify.request": return "Question"
        case "agent.message": return "Reply"
        case "peer.message": return "Message"
        case "system": return "Failed"
        default: return ""
        }
    }

    // Lays the card out for an event: on the left the project's tile; line 1 the project's name
    // and, on the right, what happened; line 2 the session's title; then the tool and its
    // command (up to 6 lines) of a permission request, or the question (and how many more) or
    // the message (up to 4 lines); on the right Allow over Deny, for a permission request with
    // options.
    func configure(_ event: PageEvent) {
        body.subviews.forEach { $0.removeFromSuperview() }
        buttons = []

        let hasButtons = event.type == "approval.request" && event.requestID != nil
        let textX = Self.pad + Self.tileSize + 10
        let textRight = width - (hasButtons ? Self.columnWidth : 0) - Self.pad
        let textWidth = textRight - textX

        let tile = TileView(color: event.color ?? Self.teal, glyph: event.glyph, ink: Self.ink)
        tile.frame = NSRect(x: Self.pad, y: 12, width: Self.tileSize, height: Self.tileSize)
        tile.radius = Self.tileRadius
        body.addSubview(tile)

        var y = Self.top
        let name = Self.line(event.title, font: .systemFont(ofSize: 13, weight: .bold), color: .labelColor)
        let kind = Self.line(Self.kind(event.type), font: .systemFont(ofSize: 11), color: .secondaryLabelColor)
        // A label's intrinsic width leaves out the 2 pt it keeps on each side; sizeToFit does not.
        kind.sizeToFit()
        let kindSize = kind.frame.size
        let nameHeight = ceil(name.intrinsicContentSize.height)
        name.frame = NSRect(x: textX, y: y, width: max(0, textWidth - kindSize.width - 4), height: nameHeight)
        let baseline = name.firstBaselineOffsetFromTop - kind.firstBaselineOffsetFromTop
        kind.frame = NSRect(x: textRight - kindSize.width, y: y + baseline,
                            width: kindSize.width, height: kindSize.height)
        body.addSubview(name)
        body.addSubview(kind)
        y += 17

        if !event.subtitle.isEmpty {
            let title = Self.line(event.subtitle, font: .systemFont(ofSize: 13), color: .labelColor)
            title.frame = NSRect(x: textX, y: y, width: textWidth, height: ceil(title.intrinsicContentSize.height))
            body.addSubview(title)
            y += 17
        }
        y += 2

        if event.type == "approval.request" {
            let mono = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
            let tool = Self.line(event.tool ?? "", font: .monospacedSystemFont(ofSize: 11, weight: .semibold),
                                 color: .secondaryLabelColor)
            y = Self.place(tool, at: y, x: textX, width: textWidth, in: body)
            if !event.body.isEmpty {
                let command = Self.text(event.body, font: mono, color: .labelColor, lines: 6, wrap: .byCharWrapping)
                y = Self.place(command, at: y + 1, x: textX, width: textWidth, in: body)
            }
        } else {
            if !event.body.isEmpty {
                let text = Self.text(event.body, font: .systemFont(ofSize: 12), color: .secondaryLabelColor,
                                     lines: 4, wrap: .byWordWrapping)
                y = Self.place(text, at: y, x: textX, width: textWidth, in: body)
            }
            if event.type == "clarify.request", event.more > 0 {
                let more = Self.line("+\(event.more) more", font: .systemFont(ofSize: 11), color: .tertiaryLabelColor)
                y = Self.place(more, at: y + 1, x: textX, width: textWidth, in: body)
            }
        }
        let height = max(Self.minHeight, ceil(y + Self.bottom), 12 + Self.tileSize + 12)

        if hasButtons {
            let column = width - Self.columnWidth
            let allow = ColumnButton(title: "Allow") { [weak self] in self?.card?.onAnswer?(true) }
            let deny = ColumnButton(title: "Deny") { [weak self] in self?.card?.onAnswer?(false) }
            let half = (height / 2).rounded()
            allow.frame = NSRect(x: column + 1, y: 0, width: Self.columnWidth - 1, height: half)
            deny.frame = NSRect(x: column + 1, y: half + 1, width: Self.columnWidth - 1, height: height - half - 1)
            buttons = [allow, deny]
            buttons.forEach(body.addSubview)
            let vertical = HairlineView(frame: NSRect(x: column, y: 0, width: 1, height: height))
            let horizontal = HairlineView(frame: NSRect(x: column + 1, y: half, width: Self.columnWidth - 1, height: 1))
            body.addSubview(vertical)
            body.addSubview(horizontal)
        }

        cardSize = NSSize(width: width, height: height)
        let room = Self.closeRoom
        frame = NSRect(x: 0, y: 0, width: width + room, height: height + room)
        body.frame = NSRect(x: room, y: room, width: width, height: height)
        outline.frame = body.bounds
        body.addSubview(outline)
        let corner = room + Self.closeInset - Self.closeSize / 2
        close.frame = NSRect(x: corner, y: corner, width: Self.closeSize, height: Self.closeSize)
        setButtonsEnabled(card?.isAnswering != true)
    }

    // A one-line label, cut with an ellipsis.
    private static func line(_ string: String, font: NSFont, color: NSColor) -> NSTextField {
        let field = NSTextField(labelWithString: string)
        field.font = font
        field.textColor = color
        field.lineBreakMode = .byTruncatingTail
        field.maximumNumberOfLines = 1
        return field
    }

    // A label of up to `lines` lines, the last one cut with an ellipsis.
    private static func text(_ string: String, font: NSFont, color: NSColor, lines: Int,
                             wrap: NSLineBreakMode) -> NSTextField {
        let field = NSTextField(wrappingLabelWithString: string)
        field.font = font
        field.textColor = color
        field.isSelectable = false
        field.maximumNumberOfLines = lines
        field.lineBreakMode = wrap
        field.cell?.truncatesLastVisibleLine = true
        return field
    }

    // Puts a label at `y`, as tall as its text at that width; returns the y under it. A label
    // draws its text 2 pt in from each side of its frame.
    private static func place(_ field: NSTextField, at y: CGFloat, x: CGFloat, width: CGFloat,
                              in parent: NSView) -> CGFloat {
        field.preferredMaxLayoutWidth = width - 4
        let height = ceil(field.intrinsicContentSize.height)
        field.frame = NSRect(x: x, y: y, width: width, height: height)
        parent.addSubview(field)
        return y + height
    }

    // A rounded rectangle that stretches from its corners, as the card's material mask.
    private static func roundedMask(radius: CGFloat) -> NSImage {
        let edge = radius * 2 + 1
        let image = NSImage(size: NSSize(width: edge, height: edge), flipped: false) { rect in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
            return true
        }
        image.capInsets = NSEdgeInsets(top: radius, left: radius, bottom: radius, right: radius)
        image.resizingMode = .stretch
        return image
    }
}

// The card's translucent material. A click anywhere on it but a button is the card's; it tells
// when the pointer comes and goes, also while the app is not active.
private final class CardBody: NSVisualEffectView {
    var onPointer: (() -> Void)?
    var onClick: (() -> Void)?
    private var pressed = false

    override var isFlipped: Bool { true }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let hit = super.hitTest(point) else { return nil }
        return hit is ColumnButton ? hit : self
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { onPointer?() }
    override func mouseExited(with event: NSEvent) { onPointer?() }

    override func mouseDown(with event: NSEvent) { pressed = true }

    override func mouseUp(with event: NSEvent) {
        defer { pressed = false }
        if pressed, bounds.contains(convert(event.locationInWindow, from: nil)) { onClick?() }
    }
}

// The card's 1 px outline: dark in the light appearance, a light rim in the dark one.
private final class OutlineView: NSView {
    var radius: CGFloat = 6

    private static let color = NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 1, alpha: 0.14)
            : NSColor(white: 0, alpha: 0.12)
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        let line = 1 / (window?.backingScaleFactor ?? 2)
        let rect = bounds.insetBy(dx: line / 2, dy: line / 2)
        let path = NSBezierPath(roundedRect: rect, xRadius: radius - line / 2, yRadius: radius - line / 2)
        path.lineWidth = line
        Self.color.setStroke()
        path.stroke()
    }
}

// A 1 pt line between the content and the buttons, and between the buttons.
private final class HairlineView: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.separatorColor.setFill()
        bounds.fill()
    }
}

// The project's tile: its colour, with its letters or emoji.
private final class TileView: NSView {
    var radius: CGFloat = 10
    private let color: NSColor
    private let glyph: String
    private let ink: NSColor

    init(color: NSColor, glyph: String, ink: NSColor) {
        self.color = color
        self.glyph = glyph
        self.ink = ink
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        color.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: radius, yRadius: radius).fill()
        // An emoji draws larger than letters, as on the client's tiles.
        let emoji = glyph.unicodeScalars.contains { $0.properties.isEmojiPresentation || $0 == "\u{FE0F}" }
        let font = NSFont.systemFont(ofSize: emoji ? 22 : 16, weight: .bold)
        let text = NSAttributedString(string: glyph, attributes: [.font: font, .foregroundColor: ink])
        let size = text.size()
        text.draw(at: NSPoint(x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2))
    }
}

// Allow or Deny: the column's half, its title centred; darker while pressed.
private final class ColumnButton: NSView {
    private let title: String
    private let action: () -> Void
    private var pressed = false { didSet { needsDisplay = true } }
    var isEnabled = true { didSet { needsDisplay = true } }

    init(title: String, action: @escaping () -> Void) {
        self.title = title
        self.action = action
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        if pressed {
            NSColor.labelColor.withAlphaComponent(0.1).setFill()
            bounds.fill()
        }
        let color: NSColor = isEnabled ? .labelColor : .tertiaryLabelColor
        let text = NSAttributedString(string: title, attributes: [.font: NSFont.systemFont(ofSize: 13),
                                                                 .foregroundColor: color])
        let size = text.size()
        text.draw(at: NSPoint(x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2))
    }

    override func mouseDown(with event: NSEvent) {
        pressed = isEnabled
    }

    override func mouseDragged(with event: NSEvent) {
        pressed = isEnabled && bounds.contains(convert(event.locationInWindow, from: nil))
    }

    override func mouseUp(with event: NSEvent) {
        let fire = pressed && bounds.contains(convert(event.locationInWindow, from: nil))
        pressed = false
        if fire { action() }
    }
}

// The round × over the card's top left corner, shown while the pointer is over the card: closes
// it without an answer.
private final class CloseButton: NSView {
    var onPointer: (() -> Void)?
    var onClick: (() -> Void)?
    private var pressed = false { didSet { needsDisplay = true } }

    private static let fill = NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.32, alpha: 1)
            : NSColor(white: 0.97, alpha: 1)
    }

    override var isFlipped: Bool { true }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? {
        isHidden ? nil : super.hitTest(point)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                       owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { onPointer?() }
    override func mouseExited(with event: NSEvent) { onPointer?() }

    override func draw(_ dirtyRect: NSRect) {
        let line = 1 / (window?.backingScaleFactor ?? 2)
        let circle = NSBezierPath(ovalIn: bounds.insetBy(dx: line / 2, dy: line / 2))
        Self.fill.setFill()
        circle.fill()
        if pressed {
            NSColor.labelColor.withAlphaComponent(0.12).setFill()
            circle.fill()
        }
        circle.lineWidth = line
        NSColor.separatorColor.setStroke()
        circle.stroke()
        let cross = NSBezierPath()
        let inset = bounds.width * 0.34
        cross.move(to: NSPoint(x: inset, y: inset))
        cross.line(to: NSPoint(x: bounds.width - inset, y: bounds.height - inset))
        cross.move(to: NSPoint(x: bounds.width - inset, y: inset))
        cross.line(to: NSPoint(x: inset, y: bounds.height - inset))
        cross.lineWidth = 1.5
        cross.lineCapStyle = .round
        NSColor.secondaryLabelColor.setStroke()
        cross.stroke()
    }

    override func mouseDown(with event: NSEvent) { pressed = true }

    override func mouseUp(with event: NSEvent) {
        let fire = pressed && bounds.contains(convert(event.locationInWindow, from: nil))
        pressed = false
        if fire { onClick?() }
    }
}
