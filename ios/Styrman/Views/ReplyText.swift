// A reply's text with its tables drawn as tables (inline Markdown has none).

import SwiftUI

struct ReplyText: View {
    let raw: String
    /// Select any part of it, as on a web page (drag the handles, then Copy). Off for
    /// text still being written.
    var selectable = false

    var body: some View {
        let blocks = ReplyMarkdown.blocks(raw)
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .text(let t) where selectable:
                    InlineSelectableText(text: Self.inline(t))
                case .text(let t):
                    Text(Self.inline(t)).font(Self.prose).lineSpacing(3)
                        .foregroundStyle(Theme.textPrimary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                case .table(let header, let rows):
                    TableBlock(header: header, rows: rows)
                }
            }
        }
    }

    /// Replies read as prose, in a serif.
    static let prose = Font.system(.body, design: .serif)

    static func inline(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(s)
    }
}

/// A Markdown table: header in bold, lines between rows, wide ones scroll sideways.
struct TableBlock: View {
    let header: [String]
    let rows: [[String]]

    var body: some View {
        // Scroll sideways only when it doesn't fit.
        ViewThatFits(in: .horizontal) {
            grid
            ScrollView(.horizontal, showsIndicators: false) { grid }
        }
        .font(.subheadline)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }

    private var grid: some View {
            Grid(alignment: .topLeading, horizontalSpacing: 14, verticalSpacing: 7) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { k, h in
                        cell(h, column: k).fontWeight(.semibold)
                    }
                }
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    Divider().gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { k, c in cell(c, column: k) }
                    }
                }
            }
            .padding(12)
    }

    /// Short columns take their own width; long ones wrap at a readable width.
    @ViewBuilder private func cell(_ s: String, column: Int) -> some View {
        let text = Text(ReplyText.inline(s))
        if longest(column) <= 24 {
            text.fixedSize()
        } else {
            // A set width (not a maximum): what a wrapping text "needs" would be its
            // whole length on one line, and the table would never fit.
            text.fixedSize(horizontal: false, vertical: true)
                .frame(width: header.count <= 2 ? 210 : 150, alignment: .leading)
        }
    }

    private func longest(_ column: Int) -> Int {
        ([header] + rows).map { $0.indices.contains(column) ? $0[column].count : 0 }.max() ?? 0
    }
}

/// Text in the conversation that selects like text anywhere on iOS: long-press a
/// word, drag the handles, then Copy, Look Up or Share. A UITextView sized to its
/// text (SwiftUI's Text only copies all of it).
struct InlineSelectableText: UIViewRepresentable {
    let text: AttributedString

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.isScrollEnabled = false
        v.backgroundColor = .clear
        v.textContainerInset = .zero
        v.textContainer.lineFragmentPadding = 0
        v.adjustsFontForContentSizeCategory = true
        v.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        v.attributedText = SelectableText.render(text)
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        let new = SelectableText.render(text)
        if v.attributedText != new { v.attributedText = new }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView v: UITextView, context: Context) -> CGSize? {
        let width = proposal.width ?? UIScreen.main.bounds.width - 32
        let fit = v.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: ceil(fit.height))
    }
}
