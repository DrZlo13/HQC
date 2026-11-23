import SwiftUI
import WebKit

/// The agent's Markdown, drawn natively as the web client draws it (client/src/ui/Markdown.tsx):
/// headings, paragraphs, nested and task lists, quotes, rules, fenced code with its language and
/// a copy button, GFM tables with their alignment, and LaTeX, display (`$$…$$`, `\[…\]`) and
/// inline (`$…$`, `\(…\)`), typeset with the bundled KaTeX.
struct MarkdownText: View {
    @Environment(\.palette) private var palette
    private let blocks: [MDBlock]

    init(_ source: String) {
        blocks = MDParser.parse(source)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                MDBlockView(block: block)
            }
        }
        .textSelection(.enabled)
        .foregroundStyle(palette.text)
    }
}

indirect enum MDBlock: Hashable {
    case heading(Int, String)
    case paragraph(String)
    case code(lang: String, text: String)
    case math(String)
    case quote([MDBlock])
    case list([MDListItem])
    case table(header: [String], align: [MDAlign], rows: [[String]])
    case rule
}

struct MDListItem: Hashable {
    let depth: Int
    let marker: String
    /// A task list item: checked or not.
    let task: Bool?
    let text: String
}

enum MDAlign: Hashable { case leading, center, trailing }

enum MDParser {
    static func parse(_ source: String) -> [MDBlock] {
        var out: [MDBlock] = []
        let lines = source.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        var i = 0
        var para: [String] = []

        func flushPara() {
            if !para.isEmpty {
                out.append(.paragraph(para.joined(separator: "\n")))
                para = []
            }
        }

        while i < lines.count {
            let line = lines[i]
            let t = line.trimmingCharacters(in: .whitespaces)

            // Fenced code: ``` or ~~~, with an optional language.
            if t.hasPrefix("```") || t.hasPrefix("~~~") {
                flushPara()
                let fence = String(t.prefix(3))
                let lang = String(t.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                var body: [String] = []
                i += 1
                while i < lines.count, !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                    body.append(lines[i])
                    i += 1
                }
                i += 1
                out.append(.code(lang: lang, text: body.joined(separator: "\n")))
                continue
            }

            // Display math: $$ … $$ or \[ … \], on one line or several.
            if t.hasPrefix("$$") || t.hasPrefix("\\[") {
                flushPara()
                let close = t.hasPrefix("$$") ? "$$" : "\\]"
                var body = String(t.dropFirst(2))
                if let r = body.range(of: close) {
                    body = String(body[..<r.lowerBound])
                    i += 1
                } else {
                    i += 1
                    while i < lines.count {
                        let l = lines[i]
                        if let r = l.range(of: close) {
                            body += "\n" + l[..<r.lowerBound]
                            i += 1
                            break
                        }
                        body += "\n" + l
                        i += 1
                    }
                }
                out.append(.math(body.trimmingCharacters(in: .whitespacesAndNewlines)))
                continue
            }

            if t.isEmpty {
                flushPara()
                i += 1
                continue
            }

            if let h = heading(t) {
                flushPara()
                out.append(.heading(h.0, h.1))
                i += 1
                continue
            }

            if isRule(t) {
                flushPara()
                out.append(.rule)
                i += 1
                continue
            }

            // A GFM table: a row of cells, then a separator row of dashes and colons.
            if t.contains("|"), i + 1 < lines.count, let align = separator(lines[i + 1]) {
                flushPara()
                let header = cells(t)
                var rows: [[String]] = []
                i += 2
                while i < lines.count {
                    let r = lines[i].trimmingCharacters(in: .whitespaces)
                    guard r.contains("|"), !r.isEmpty else { break }
                    var c = cells(r)
                    if c.count < header.count { c += Array(repeating: "", count: header.count - c.count) }
                    rows.append(Array(c.prefix(header.count)))
                    i += 1
                }
                let a = align.count >= header.count ? Array(align.prefix(header.count))
                    : align + Array(repeating: .leading, count: header.count - align.count)
                out.append(.table(header: header, align: a, rows: rows))
                continue
            }

            if t.hasPrefix(">") {
                flushPara()
                var body: [String] = []
                while i < lines.count {
                    let l = lines[i].trimmingCharacters(in: .whitespaces)
                    guard l.hasPrefix(">") else { break }
                    body.append(String(l.dropFirst()).replacingOccurrences(of: "^ ", with: "", options: .regularExpression))
                    i += 1
                }
                out.append(.quote(parse(body.joined(separator: "\n"))))
                continue
            }

            if listItem(line) != nil {
                flushPara()
                var items: [MDListItem] = []
                while i < lines.count, let item = listItem(lines[i]) {
                    var text = item.text
                    i += 1
                    // Continuation lines of the item, indented and not an item themselves.
                    while i < lines.count, listItem(lines[i]) == nil,
                          lines[i].hasPrefix("  ") || lines[i].hasPrefix("\t"),
                          !lines[i].trimmingCharacters(in: .whitespaces).isEmpty {
                        text += "\n" + lines[i].trimmingCharacters(in: .whitespaces)
                        i += 1
                    }
                    items.append(MDListItem(depth: item.depth, marker: item.marker, task: item.task, text: text))
                }
                out.append(.list(items))
                continue
            }

            para.append(line)
            i += 1
        }
        flushPara()
        return out
    }

    private static func heading(_ s: String) -> (Int, String)? {
        let hashes = s.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), s.dropFirst(hashes).first == " " else { return nil }
        return (hashes, String(s.dropFirst(hashes + 1)).trimmingCharacters(in: CharacterSet(charactersIn: " #")))
    }

    private static func isRule(_ s: String) -> Bool {
        let c = s.replacingOccurrences(of: " ", with: "")
        guard c.count >= 3, let f = c.first, "-*_".contains(f) else { return false }
        return c.allSatisfy { $0 == f }
    }

    private static func cells(_ row: String) -> [String] {
        var r = row.trimmingCharacters(in: .whitespaces)
        if r.hasPrefix("|") { r.removeFirst() }
        if r.hasSuffix("|") && !r.hasSuffix("\\|") { r.removeLast() }
        // `\|` is a pipe inside a cell.
        let marker = "\u{0}"
        return r.replacingOccurrences(of: "\\|", with: marker)
            .components(separatedBy: "|")
            .map { $0.replacingOccurrences(of: marker, with: "|").trimmingCharacters(in: .whitespaces) }
    }

    private static func separator(_ line: String) -> [MDAlign]? {
        let parts = cells(line)
        guard !parts.isEmpty else { return nil }
        var out: [MDAlign] = []
        for p in parts {
            let core = p.trimmingCharacters(in: CharacterSet(charactersIn: ":"))
            guard !core.isEmpty, core.allSatisfy({ $0 == "-" }) else { return nil }
            switch (p.hasPrefix(":"), p.hasSuffix(":")) {
            case (true, true): out.append(.center)
            case (false, true): out.append(.trailing)
            default: out.append(.leading)
            }
        }
        return out
    }

    private static func listItem(_ line: String) -> (depth: Int, marker: String, task: Bool?, text: String)? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }
            .reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
        let s = line.trimmingCharacters(in: .whitespaces)
        var marker: String
        var rest: Substring
        if let f = s.first, "-*+".contains(f), s.dropFirst().first == " " {
            marker = "•"
            rest = s.dropFirst(2)
        } else {
            let digits = s.prefix { $0.isNumber }
            guard !digits.isEmpty, digits.count < 4 else { return nil }
            let after = s.dropFirst(digits.count)
            guard after.hasPrefix(". ") || after.hasPrefix(") ") else { return nil }
            marker = "\(digits)."
            rest = after.dropFirst(2)
        }
        var task: Bool?
        if rest.hasPrefix("[ ] ") { task = false; rest = rest.dropFirst(4) }
        else if rest.lowercased().hasPrefix("[x] ") { task = true; rest = rest.dropFirst(4) }
        return (indent / 2, marker, task, String(rest))
    }

    /// Inline math in a text: `$…$` (not `$$`, no space just inside, no digit right after, as
    /// KaTeX's auto-render reads it, so "$5 and $10" stays text) or `\(…\)`.
    static func hasInlineMath(_ s: String) -> Bool {
        if s.contains("\\(") && s.contains("\\)") { return true }
        return s.range(of: #"(?<![\\$])\$(?!\s)[^$\n]+?(?<!\s)\$(?![\d$])"#, options: .regularExpression) != nil
    }
}

struct MDBlockView: View {
    @Environment(\.palette) private var palette
    let block: MDBlock

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(MDInline.render(text, palette: palette))
                .font(level == 1 ? .title2.weight(.bold) : level == 2 ? .title3.weight(.semibold) : .headline)
                .padding(.top, level <= 2 ? 6 : 2)
        case .paragraph(let text):
            if MDParser.hasInlineMath(text) {
                MathView(source: text, display: false)
            } else {
                Text(MDInline.render(text, palette: palette))
                    .fixedSize(horizontal: false, vertical: true)
            }
        case .code(let lang, let text):
            CodeBlockView(language: lang, code: text)
        case .math(let tex):
            MathView(source: tex, display: true)
        case .quote(let inner):
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(inner.enumerated()), id: \.offset) { _, b in MDBlockView(block: b) }
            }
            .foregroundStyle(palette.text2)
            .padding(.leading, 12)
            .overlay(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1.5).fill(palette.border).frame(width: 3)
            }
        case .list(let items):
            VStack(alignment: .leading, spacing: 5) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        if let done = item.task {
                            Image(systemName: done ? "checkmark.square.fill" : "square")
                                .foregroundStyle(done ? Color.accentColor : palette.muted)
                                .font(.callout)
                        } else {
                            Text(item.depth > 0 && item.marker == "•" ? "◦" : item.marker)
                                .foregroundStyle(palette.muted)
                                .monospacedDigit()
                                .frame(minWidth: 14, alignment: .trailing)
                        }
                        if MDParser.hasInlineMath(item.text) {
                            MathView(source: item.text, display: false)
                        } else {
                            Text(MDInline.render(item.text, palette: palette))
                                .strikethrough(item.task == true, color: palette.muted)
                                .foregroundStyle(item.task == true ? palette.muted : palette.text)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(.leading, CGFloat(item.depth) * 18)
                }
            }
        case .table(let header, let align, let rows):
            MDTableView(header: header, align: align, rows: rows)
        case .rule:
            Rectangle().fill(palette.border).frame(height: 1).padding(.vertical, 4)
        }
    }
}

enum MDInline {
    /// Inline Markdown (bold, italic, strikethrough, code, links) with the web client's look:
    /// code in the monospaced face on the code background, links in the accent color.
    static func render(_ s: String, palette: Palette) -> AttributedString {
        let opts = AttributedString.MarkdownParsingOptions(
            interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        var a = (try? AttributedString(markdown: s, options: opts)) ?? AttributedString(s)
        for run in a.runs {
            if let intent = run.inlinePresentationIntent, intent.contains(.code) {
                a[run.range].font = .system(.callout, design: .monospaced)
                a[run.range].foregroundColor = palette.text2
                a[run.range].backgroundColor = palette.code
            }
            if let link = run.link {
                // Only links a browser should open; `javascript:` and the like stay text, as in
                // the web client.
                if ["http", "https", "mailto"].contains(link.scheme?.lowercased() ?? "") {
                    a[run.range].foregroundColor = Color.accentColor
                    a[run.range].underlineStyle = .single
                } else {
                    a[run.range].link = nil
                }
            }
        }
        return a
    }
}

/// A fenced code block: the language and a copy button above, the code scrolling sideways.
struct CodeBlockView: View {
    @Environment(\.palette) private var palette
    let language: String
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "code" : language)
                    .font(.caption.monospaced())
                    .foregroundStyle(palette.muted)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .font(.caption)
                        .labelStyle(.titleAndIcon)
                        .contentTransition(.symbolEffect(.replace))
                }
                .buttonStyle(.plain)
                .foregroundStyle(palette.muted)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            Rectangle().fill(palette.border).frame(height: 1)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(palette.text)
                    .padding(12)
                    .textSelection(.enabled)
            }
        }
        .background(palette.code, in: .rect(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(palette.border))
    }
}

/// A GFM table with its header, borders and column alignment, scrolling sideways when wide.
struct MDTableView: View {
    @Environment(\.palette) private var palette
    let header: [String]
    let align: [MDAlign]
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { i, h in
                        cell(h, column: i).font(.subheadline.weight(.semibold))
                    }
                }
                .background(palette.surface)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { i, c in
                            cell(c, column: i).font(.subheadline)
                        }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(palette.border))
            .clipShape(.rect(cornerRadius: 6))
        }
    }

    private func cell(_ s: String, column i: Int) -> some View {
        let a = i < align.count ? align[i] : .leading
        return Text(MDInline.render(s, palette: palette))
            .frame(minWidth: 44, maxWidth: 280,
                   alignment: a == .center ? .center : a == .trailing ? .trailing : .leading)
            .multilineTextAlignment(a == .center ? .center : a == .trailing ? .trailing : .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .gridColumnAlignment(a == .center ? .center : a == .trailing ? .trailing : .leading)
            .border(palette.border, width: 0.5)
    }
}

/// LaTeX through the bundled KaTeX in a transparent web view sized to its content: a display
/// formula, or a paragraph with inline math (its other Markdown as HTML).
struct MathView: View {
    @Environment(\.palette) private var palette
    let source: String
    let display: Bool
    @State private var height: CGFloat = 24

    var body: some View {
        MathWebView(html: html, height: $height)
            .frame(height: height)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var html: String {
        let color = palette.light ? "#1F1B16" : "#F2EEE8"
        let code = palette.light ? "#F0EBE3" : "#141210"
        let body: String
        if display {
            body = "<div id=m class=display></div><script>try{katex.render(\(jsString(source)),document.getElementById('m'),{displayMode:true,throwOnError:false})}catch(e){document.getElementById('m').textContent=\(jsString(source))}</script>"
        } else {
            body = "<div id=m>\(inlineHTML(source))</div><script>renderMath(document.getElementById('m'))</script>"
        }
        return """
        <!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1">
        <link rel=stylesheet href="katex.min.css"><script src="katex.min.js"></script>
        <style>html,body{margin:0;padding:0;background:transparent;color:\(color);
        font:-apple-system-body;font-family:-apple-system,system-ui;-webkit-text-size-adjust:100%}
        .display{overflow-x:auto;overflow-y:hidden;padding:2px 0}
        code{font-family:ui-monospace,Menlo,monospace;font-size:.9em;background:\(code);padding:1px 4px;border-radius:4px}
        .katex{font-size:1.08em}</style>
        <script>
        function renderMath(el){
          const t=el.innerHTML;
          el.innerHTML=t.replace(/\\\\\\((.+?)\\\\\\)|(^|[^\\\\$])\\$(?!\\s)([^$\\n]+?)(?<!\\s)\\$(?![\\d$])/g,function(m,a,pre,b){
            const tex=(a!==undefined?a:b).replace(/&lt;/g,'<').replace(/&gt;/g,'>').replace(/&amp;/g,'&');
            let out;try{out=katex.renderToString(tex,{throwOnError:false})}catch(e){out=tex}
            return (pre||'')+out;});
        }
        </script></head><body>\(body)
        <script>function h(){window.webkit.messageHandlers.size.postMessage(document.body.scrollHeight)}
        h();document.fonts.ready.then(h);new ResizeObserver(h).observe(document.body)</script></body></html>
        """
    }

    private func jsString(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s])) ?? Data("[\"\"]".utf8)
        let arr = String(data: data, encoding: .utf8) ?? "[\"\"]"
        return "\(arr)[0]"
    }

    /// The paragraph's inline Markdown as HTML: escaped, then bold, italic, code and links.
    private func inlineHTML(_ s: String) -> String {
        var t = s.replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
        let rules: [(String, String)] = [
            (#"`([^`]+)`"#, "<code>$1</code>"),
            (#"\*\*([^*]+)\*\*"#, "<b>$1</b>"),
            (#"(?<![*\w])\*([^*\n]+)\*(?!\w)"#, "<i>$1</i>"),
            (#"~~([^~]+)~~"#, "<s>$1</s>"),
            (#"\[([^\]]+)\]\((https?://[^)\s]+)\)"#, "<a href=\"$2\">$1</a>"),
        ]
        for (p, r) in rules {
            t = t.replacingOccurrences(of: p, with: r, options: .regularExpression)
        }
        return t.replacingOccurrences(of: "\n", with: "<br>")
    }
}

private struct MathWebView: UIViewRepresentable {
    let html: String
    @Binding var height: CGFloat

    func makeCoordinator() -> Coordinator { Coordinator(height: $height) }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.userContentController.add(context.coordinator, name: "size")
        let web = WKWebView(frame: .zero, configuration: config)
        web.isOpaque = false
        web.backgroundColor = .clear
        web.scrollView.backgroundColor = .clear
        web.scrollView.isScrollEnabled = false
        web.scrollView.bounces = false
        web.navigationDelegate = context.coordinator
        context.coordinator.load(html, in: web)
        return web
    }

    func updateUIView(_ web: WKWebView, context: Context) {
        context.coordinator.load(html, in: web)
    }

    final class Coordinator: NSObject, WKScriptMessageHandler, WKNavigationDelegate {
        var height: Binding<CGFloat>
        private var loaded: String?

        init(height: Binding<CGFloat>) { self.height = height }

        func load(_ html: String, in web: WKWebView) {
            guard html != loaded else { return }
            loaded = html
            web.loadHTMLString(html, baseURL: Bundle.main.resourceURL)
        }

        func userContentController(_ c: WKUserContentController, didReceive message: WKScriptMessage) {
            if let h = message.body as? Double, h > 0, abs(height.wrappedValue - h) > 0.5 {
                height.wrappedValue = ceil(h)
            }
        }

        /// Links open in the browser, never inside the formula's view.
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            if action.navigationType == .linkActivated, let url = action.request.url {
                await UIApplication.shared.open(url)
                return .cancel
            }
            return .allow
        }
    }
}
