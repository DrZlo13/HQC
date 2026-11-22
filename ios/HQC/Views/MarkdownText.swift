import SwiftUI

/// The agent's Markdown: fenced code as code blocks, headings and list items as their own lines,
/// the rest with Foundation's inline Markdown (bold, italic, code, links).
struct MarkdownText: View {
    private let blocks: [Block]

    init(_ source: String) {
        blocks = Self.parse(source)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .code(let text):
                    CodeBlock(text: text)
                case .heading(let level, let text):
                    Text(Self.inline(text))
                        .font(level == 1 ? .title2.bold() : level == 2 ? .title3.bold() : .headline)
                        .padding(.top, 4)
                case .bullet(let text, let marker):
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(marker).foregroundStyle(.secondary).monospacedDigit()
                        Text(Self.inline(text))
                    }
                    .padding(.leading, 4)
                case .quote(let text):
                    Text(Self.inline(text))
                        .foregroundStyle(.secondary)
                        .padding(.leading, 10)
                        .overlay(alignment: .leading) {
                            Rectangle().fill(.tertiary).frame(width: 3)
                        }
                case .paragraph(let text):
                    Text(Self.inline(text))
                }
            }
        }
        .textSelection(.enabled)
    }

    enum Block {
        case code(String)
        case heading(Int, String)
        case bullet(String, marker: String)
        case quote(String)
        case paragraph(String)
    }

    static func inline(_ s: String) -> AttributedString {
        let opts = AttributedString.MarkdownParsingOptions(
            interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        return (try? AttributedString(markdown: s, options: opts)) ?? AttributedString(s)
    }

    static func parse(_ source: String) -> [Block] {
        var out: [Block] = []
        var para: [String] = []
        var code: [String]?

        func flush() {
            if !para.isEmpty {
                out.append(.paragraph(para.joined(separator: "\n")))
                para = []
            }
        }

        for line in source.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") {
                if let c = code {
                    out.append(.code(c.joined(separator: "\n")))
                    code = nil
                } else {
                    flush()
                    code = []
                }
                continue
            }
            if code != nil {
                code!.append(line)
                continue
            }
            if trimmed.isEmpty {
                flush()
            } else if let h = heading(trimmed) {
                flush()
                out.append(.heading(h.0, h.1))
            } else if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") || trimmed.hasPrefix("+ ") {
                flush()
                out.append(.bullet(String(trimmed.dropFirst(2)), marker: "•"))
            } else if let n = numbered(trimmed) {
                flush()
                out.append(.bullet(n.1, marker: n.0))
            } else if trimmed.hasPrefix(">") {
                flush()
                out.append(.quote(String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces)))
            } else {
                para.append(line)
            }
        }
        if let c = code { out.append(.code(c.joined(separator: "\n"))) }
        flush()
        return out
    }

    private static func heading(_ s: String) -> (Int, String)? {
        let hashes = s.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), s.dropFirst(hashes).first == " " else { return nil }
        return (hashes, String(s.dropFirst(hashes + 1)))
    }

    private static func numbered(_ s: String) -> (String, String)? {
        let digits = s.prefix { $0.isNumber }
        guard !digits.isEmpty, digits.count < 4 else { return nil }
        let rest = s.dropFirst(digits.count)
        guard rest.hasPrefix(". ") || rest.hasPrefix(") ") else { return nil }
        return ("\(digits).", String(rest.dropFirst(2)))
    }
}
