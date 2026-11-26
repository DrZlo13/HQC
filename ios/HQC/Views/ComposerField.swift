import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// The composer's text field: a `UITextView`, because a SwiftUI `TextField` takes only text.
/// It declares that it accepts images, PDFs, movies and files (`pasteConfiguration`), so
/// iOS offers the keyboard's paste suggestion for them ("Paste from Screenshots") and the edit
/// menu and ⌘V paste them; those items go to `onItems` as attachments, text is inserted as text.
/// It grows from one line to `maxLines`, then scrolls.
struct ComposerField: UIViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    var placeholder: String
    var textColor: UIColor
    var placeholderColor: UIColor
    var dense = false
    var maxLines = 8
    let onItems: ([NSItemProvider]) -> Void

    /// What the field takes from the pasteboard besides text.
    static let attachmentTypes: [UTType] = [.image, .movie, .pdf, .fileURL, .data]

    func makeUIView(context: Context) -> PastingTextView {
        let v = PastingTextView()
        v.delegate = context.coordinator
        v.applyDensity(dense)
        v.backgroundColor = .clear
        v.textContainer.lineFragmentPadding = 4
        v.isScrollEnabled = false
        v.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        v.pasteConfiguration = UIPasteConfiguration(
            acceptableTypeIdentifiers: (Self.attachmentTypes + [.plainText, .utf8PlainText, .url]).map(\.identifier))
        v.onItems = { onItems($0) }
        v.placeholder.text = placeholder
        v.placeholder.textColor = placeholderColor
        return v
    }

    func updateUIView(_ v: PastingTextView, context: Context) {
        if v.text != text {
            v.text = text
            v.placeholder.isHidden = !text.isEmpty
        }
        v.applyDensity(dense)
        v.textColor = textColor
        v.placeholder.textColor = placeholderColor
        v.onItems = { onItems($0) }
        // Focus follows the binding, both ways.
        if focused, !v.isFirstResponder {
            DispatchQueue.main.async { v.becomeFirstResponder() }
        } else if !focused, v.isFirstResponder {
            DispatchQueue.main.async { v.resignFirstResponder() }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView v: PastingTextView, context: Context) -> CGSize? {
        let width = proposal.width ?? 280
        let fit = v.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let line = v.font?.lineHeight ?? 20
        let maxHeight = line * CGFloat(maxLines) + v.textContainerInset.top + v.textContainerInset.bottom
        v.isScrollEnabled = fit.height > maxHeight
        return CGSize(width: width, height: min(fit.height, maxHeight))
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ComposerField
        init(_ parent: ComposerField) { self.parent = parent }

        func textViewDidChange(_ v: UITextView) {
            parent.text = v.text
            (v as? PastingTextView)?.placeholder.isHidden = !v.text.isEmpty
            v.invalidateIntrinsicContentSize()
        }

        func textViewDidBeginEditing(_ v: UITextView) {
            if !parent.focused { parent.focused = true }
        }

        func textViewDidEndEditing(_ v: UITextView) {
            if parent.focused { parent.focused = false }
        }
    }
}

/// A text view that pastes attachments: items that are not text go to `onItems`.
final class PastingTextView: UITextView {
    var onItems: (([NSItemProvider]) -> Void)?
    let placeholder = UILabel()
    private var placeholderTop: NSLayoutConstraint?
    private var currentDense: Bool?

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        placeholder.translatesAutoresizingMaskIntoConstraints = false
        addSubview(placeholder)
        let top = placeholder.topAnchor.constraint(equalTo: topAnchor, constant: 8)
        placeholderTop = top
        NSLayoutConstraint.activate([
            placeholder.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            top,
        ])
        applyDensity(false)
    }

    func applyDensity(_ dense: Bool) {
        guard currentDense != dense else { return }
        currentDense = dense
        let f = dense ? UIFont.systemFont(ofSize: 14.5) : UIFont.preferredFont(forTextStyle: .body)
        font = f
        adjustsFontForContentSizeCategory = !dense
        placeholder.font = f
        placeholder.adjustsFontForContentSizeCategory = !dense
        let vPad: CGFloat = dense ? 6 : 8
        textContainerInset = UIEdgeInsets(top: vPad, left: 10, bottom: vPad, right: 10)
        placeholderTop?.constant = vPad
        invalidateIntrinsicContentSize()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private static func isAttachment(_ p: NSItemProvider) -> Bool {
        // Text and links stay text; anything else (an image, a PDF, a file) is an attachment.
        let textual = [UTType.plainText, .utf8PlainText, .text, .url].map(\.identifier)
        return !p.registeredTypeIdentifiers.allSatisfy { id in
            textual.contains { UTType(id)?.conforms(to: UTType($0)!) ?? false }
        } && p.registeredTypeIdentifiers.contains { id in
            ComposerField.attachmentTypes.contains { UTType(id)?.conforms(to: $0) ?? false }
        }
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)),
           UIPasteboard.general.itemProviders.contains(where: Self.isAttachment) {
            return true
        }
        return super.canPerformAction(action, withSender: sender)
    }

    /// The edit menu's Paste and ⌘V.
    override func paste(_ sender: Any?) {
        let providers = UIPasteboard.general.itemProviders
        let attachments = providers.filter(Self.isAttachment)
        guard !attachments.isEmpty else {
            super.paste(sender)
            return
        }
        onItems?(attachments)
    }

    /// The keyboard's paste suggestion and drops matching `pasteConfiguration`.
    override func paste(itemProviders: [NSItemProvider]) {
        let attachments = itemProviders.filter(Self.isAttachment)
        if !attachments.isEmpty { onItems?(attachments) }
        for p in itemProviders where !Self.isAttachment(p) && p.canLoadObject(ofClass: String.self) {
            _ = p.loadObject(ofClass: String.self) { [weak self] s, _ in
                guard let s else { return }
                DispatchQueue.main.async {
                    self?.insertText(s)
                }
            }
        }
    }
}
