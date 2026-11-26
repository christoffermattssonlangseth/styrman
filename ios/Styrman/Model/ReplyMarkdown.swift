// Replies are Markdown, shown with SwiftUI's inline Markdown, which has no tables:
// they would show as raw `| a | b |` lines. Here tables become one readable line
// per row instead, which also suits a phone's width better than columns.

import Foundation

enum ReplyMarkdown {
    /// `text` with its tables (Markdown, or rows the live preview read off a terminal)
    /// turned into lines: two columns as "**name** — value", more as
    /// "**name** — Header: value · Header: value".
    static func readable(_ text: String) -> String {
        let lines = text.components(separatedBy: "\n")
        var out: [String] = []
        var i = 0
        while i < lines.count {
            // A table: two or more lines in a row that start and end with `|`.
            var j = i
            while j < lines.count, isRow(lines[j]) { j += 1 }
            if j - i >= 2 {
                out.append(contentsOf: table(Array(lines[i..<j])))
                i = j
            } else {
                out.append(lines[i])
                i += 1
            }
        }
        return out.joined(separator: "\n")
    }

    /// A reply split into text and tables, to draw tables as tables.
    enum Block: Equatable {
        case text(String)
        case table(header: [String], rows: [[String]])
    }

    static func blocks(_ text: String) -> [Block] {
        let lines = text.components(separatedBy: "\n")
        var out: [Block] = []
        var plain: [String] = []
        func flush() {
            let t = plain.joined(separator: "\n").trimmingCharacters(in: .newlines)
            if !t.isEmpty { out.append(.text(t)) }
            plain = []
        }
        var i = 0
        while i < lines.count {
            var j = i
            while j < lines.count, isRow(lines[j]) { j += 1 }
            var rows = lines[i..<j].map(cells).filter { !isDivider($0) }
            if j - i >= 2, !rows.isEmpty {
                let header = rows.removeFirst()
                // Every row as wide as the widest.
                let width = ([header] + rows).map(\.count).max() ?? 0
                let pad = { (r: [String]) in r + Array(repeating: "", count: max(0, width - r.count)) }
                flush()
                out.append(.table(header: pad(header), rows: rows.map(pad)))
                i = j
            } else {
                plain.append(lines[i])
                i += 1
            }
        }
        flush()
        return out
    }

    private static func isRow(_ line: String) -> Bool {
        let t = line.trimmingCharacters(in: .whitespaces)
        return t.count >= 3 && t.hasPrefix("|") && t.hasSuffix("|")
    }

    private static func cells(_ line: String) -> [String] {
        var t = line.trimmingCharacters(in: .whitespaces)
        t.removeFirst(); t.removeLast()
        return t.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// `|---|:--:|`
    private static func isDivider(_ cells: [String]) -> Bool {
        cells.allSatisfy { c in !c.isEmpty && c.allSatisfy { "-:".contains($0) } && c.contains("-") }
    }

    private static func table(_ rows: [String]) -> [String] {
        var parsed = rows.map(cells).filter { !isDivider($0) }
        guard !parsed.isEmpty else { return [] }
        let header = parsed.removeFirst()
        if parsed.isEmpty { return [header.joined(separator: " · ")] }
        return parsed.map { row in
            let name = row.first.map { $0.replacingOccurrences(of: "**", with: "") } ?? ""
            let rest = Array(row.dropFirst())
            let lead = name.isEmpty ? "" : "**\(name)**"
            guard !rest.isEmpty else { return "• \(lead)" }
            let values: String
            if header.count <= 2 {
                values = rest.joined(separator: " · ")
            } else {
                values = rest.enumerated().map { k, v in
                    let h = k + 1 < header.count ? header[k + 1] : ""
                    return h.isEmpty ? v : "\(h): \(v)"
                }.joined(separator: " · ")
            }
            return lead.isEmpty ? values : "\(lead) — \(values)"
        }
    }
}
