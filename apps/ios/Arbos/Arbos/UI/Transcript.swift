// The chat body: one row per markdown block, prose only (Grok-bot style).
// Tool calls, raw JSON, status events, usage footers and return contracts
// never render here; `ChatView` shows one working indicator while a turn
// runs. Port of `crates/arbos-mac/src/ui/transcript.rs::build_rows`.

import SwiftUI
import UIKit

enum Block: Equatable, Hashable {
    case paragraph(String)
    case heading(level: Int, text: String)
    case bullets([String])
    case numbered([String])
    case code(lang: String, code: String)
    case table([String])

    var textLength: Int {
        switch self {
        case .paragraph(let t), .heading(_, let t), .code(_, let t): return t.utf8.count
        case .bullets(let items), .numbered(let items), .table(let items): return items.reduce(0) { $0 + $1.utf8.count + 1 }
        }
    }
}

enum RowKind: Equatable {
    case user(text: String, attachments: [String], pending: Bool)
    case markdown(block: Block, live: Bool)
    /// S8: the agent asked a question.
    case askUser(text: String)
    case error(code: String, message: String)
}

struct Row: Identifiable, Equatable {
    var id: String
    var messageId: Id
    var kind: RowKind
}

enum Transcript {
    /// Prose and real attachments only. Tool calls, status events, raw JSON,
    /// usage footers and subagent names never render; `ChatView` shows one
    /// working indicator while any turn in the chat streams.
    static func rows(_ messages: [Message]) -> [Row] {
        var rows: [Row] = []
        for message in messages {
            let streaming = message.isStreaming
            switch message.role {
            case .user:
                var attachments: [String] = []
                for part in message.parts {
                    if case .attachment(_, let a) = part { attachments.append(a.name) }
                }
                rows.append(Row(id: "\(message.id)#user", messageId: message.id,
                                kind: .user(text: message.plainText, attachments: attachments, pending: message.pending)))
            case .assistant, .system, .project:
                let lastTextPart = message.parts.lastIndex { $0.textValue != nil }
                for (pix, part) in message.parts.enumerated() {
                    switch part {
                    case .text(let id, let text):
                        let blocks = Markdown.blocks(text).filter { !Markdown.isJSON($0) }
                        let isLastText = pix == lastTextPart
                        for (bix, block) in blocks.enumerated() {
                            let live = streaming && isLastText && bix + 1 == blocks.count
                            rows.append(Row(id: "\(message.id)#\(id).\(bix)", messageId: message.id,
                                            kind: .markdown(block: block, live: live)))
                        }
                    case .status(let id, .askUser, let text):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, kind: .askUser(text: text)))
                    case .error(let id, let code, let text):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, kind: .error(code: code, message: text)))
                    case .status, .tool, .subagentResult, .reasoning, .attachment, .unknown:
                        break
                    }
                }
                if message.status == .error && !message.hasErrorPart {
                    rows.append(Row(id: "\(message.id)#failed", messageId: message.id,
                                    kind: .error(code: "turn_failed", message: "The agent could not finish this turn.")))
                }
            }
        }
        return rows
    }
}

/// Minimal block splitter: fences, headings, bullets, numbered lists, tables, paragraphs.
enum Markdown {
    static func blocks(_ text: String) -> [Block] {
        var out: [Block] = []
        var paragraph: [String] = []
        var bullets: [String] = []
        var numbered: [String] = []
        var table: [String] = []
        var code: [String]? = nil
        var lang = ""

        func flush() {
            if !paragraph.isEmpty { out.append(.paragraph(paragraph.joined(separator: " "))); paragraph = [] }
            if !bullets.isEmpty { out.append(.bullets(bullets)); bullets = [] }
            if !numbered.isEmpty { out.append(.numbered(numbered)); numbered = [] }
            if !table.isEmpty { out.append(.table(table)); table = [] }
        }

        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if let c = code {
                if trimmed.hasPrefix("```") {
                    out.append(.code(lang: lang, code: c.joined(separator: "\n")))
                    code = nil
                } else {
                    code!.append(line)
                }
                continue
            }
            if trimmed.hasPrefix("```") {
                flush()
                lang = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                code = []
                continue
            }
            if trimmed.isEmpty { flush(); continue }
            if trimmed.hasPrefix("#") {
                flush()
                let level = trimmed.prefix { $0 == "#" }.count
                out.append(.heading(level: min(level, 3), text: trimmed.drop { $0 == "#" }.trimmingCharacters(in: .whitespaces)))
                continue
            }
            if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") || trimmed.hasPrefix("• ") {
                if !paragraph.isEmpty || !numbered.isEmpty || !table.isEmpty { flush() }
                bullets.append(String(trimmed.dropFirst(2)))
                continue
            }
            if let dot = trimmed.firstIndex(of: "."), dot > trimmed.startIndex, trimmed[trimmed.startIndex..<dot].allSatisfy(\.isNumber),
               trimmed.distance(from: trimmed.startIndex, to: dot) <= 3, trimmed[dot...].hasPrefix(". ") {
                if !paragraph.isEmpty || !bullets.isEmpty || !table.isEmpty { flush() }
                numbered.append(trimmed[trimmed.index(dot, offsetBy: 2)...].trimmingCharacters(in: .whitespaces))
                continue
            }
            if trimmed.hasPrefix("|") {
                if !paragraph.isEmpty || !bullets.isEmpty || !numbered.isEmpty { flush() }
                if !trimmed.replacingOccurrences(of: "|", with: "").replacingOccurrences(of: "-", with: "").replacingOccurrences(of: ":", with: "").trimmingCharacters(in: .whitespaces).isEmpty {
                    table.append(trimmed)
                }
                continue
            }
            if !bullets.isEmpty || !numbered.isEmpty || !table.isEmpty { flush() }
            paragraph.append(trimmed)
        }
        if let c = code { out.append(.code(lang: lang, code: c.joined(separator: "\n"))) }
        flush()
        return out
    }

    /// A block that is really machine output (a JSON object/array) is not prose.
    static func isJSON(_ block: Block) -> Bool {
        let text: String
        switch block {
        case .paragraph(let t): text = t
        case .code(let lang, let c) where lang.isEmpty || lang.lowercased() == "json": text = c
        default: return false
        }
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard t.hasPrefix("{") || t.hasPrefix("["), let data = t.data(using: .utf8) else { return false }
        return (try? JSONSerialization.jsonObject(with: data)) != nil
    }

    static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }
}

// MARK: - Row views

struct RowView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let row: Row
    let chatId: Id

    var body: some View {
        switch row.kind {
        case .user(let text, let attachments, let pending):
            HStack {
                Spacer(minLength: 60)
                VStack(alignment: .leading, spacing: 6) {
                    if !attachments.isEmpty {
                        HStack(spacing: 6) {
                            ForEach(attachments, id: \.self) { name in
                                Text(name)
                                    .font(.system(size: Theme.tiny))
                                    .foregroundStyle(theme.textMuted)
                                    .padding(.horizontal, 8).padding(.vertical, 2)
                                    .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface2))
                            }
                        }
                    }
                    Text(text)
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                        .textSelection(.enabled)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubble))
                .opacity(pending ? 0.6 : 1)
            }
            .padding(.vertical, 6)
        case .markdown(let block, let live):
            BlockView(block: block, live: live)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
        case .askUser(let text):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 1).fill(theme.warn).frame(width: 2)
                VStack(alignment: .leading, spacing: 4) {
                    Text("WAITING FOR YOU")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(theme.warn)
                    Text(Markdown.inline(text))
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .card()
            .padding(.vertical, 4)
        case .error(let code, let message):
            HStack(spacing: 10) {
                Text("✗").foregroundStyle(theme.danger).font(.system(size: 12))
                Text(message).font(.system(size: 13)).foregroundStyle(theme.text)
                Spacer(minLength: 0)
                Text(code).font(.system(size: 11)).foregroundStyle(theme.textFaint)
                Button("Retry") { model.retry(chatId: chatId, messageId: row.messageId) }
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(theme.accent)
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: Theme.panelRadius).fill(theme.dangerSoft))
            .overlay(RoundedRectangle(cornerRadius: Theme.panelRadius).strokeBorder(theme.danger.opacity(0.4), lineWidth: 1))
            .padding(.vertical, 4)
        }
    }
}

struct BlockView: View {
    @Environment(\.theme) private var theme
    let block: Block
    let live: Bool

    var body: some View {
        switch block {
        case .paragraph(let text):
            if live {
                StreamingText(text: text, font: .system(size: Theme.body), color: theme.text)
            } else {
                Text(Markdown.inline(text))
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.text)
                    .lineSpacing(Theme.bodyLine - Theme.body - 4)
                    .textSelection(.enabled)
            }
        case .heading(let level, let text):
            Text(Markdown.inline(text))
                .font(.system(size: level == 1 ? 22 : level == 2 ? 19 : 17, weight: .semibold))
                .foregroundStyle(theme.text)
                .padding(.top, 6)
        case .bullets(let items), .numbered(let items):
            let numbered: Bool = { if case .numbered = block { return true } else { return false } }()
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(numbered ? "\(i + 1)." : "•")
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.textMuted)
                            .frame(width: 18, alignment: .trailing)
                        if live && i == items.count - 1 {
                            StreamingText(text: item, font: .system(size: Theme.body), color: theme.text)
                        } else {
                            Text(Markdown.inline(item))
                                .font(.system(size: Theme.body))
                                .foregroundStyle(theme.text)
                        }
                    }
                }
            }
        case .table(let lines):
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { i, line in
                        Text(line.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }.joined(separator: "    "))
                            .font(.system(size: 13, weight: i == 0 ? .semibold : .regular, design: .monospaced))
                            .foregroundStyle(i == 0 ? theme.text : theme.textMuted)
                    }
                }
                .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.codeBg))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(theme.border, lineWidth: 1))
        case .code(let lang, let code):
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text(lang.isEmpty ? "code" : lang)
                    Spacer()
                    Button("Copy") { UIPasteboard.general.string = code }
                }
                .font(.system(size: 11))
                .foregroundStyle(theme.textFaint)
                .padding(.horizontal, 10).padding(.vertical, 5)
                .overlay(alignment: .bottom) { Rectangle().fill(theme.border).frame(height: 1) }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(code)
                        .font(.system(size: 13, design: .monospaced))
                        .foregroundStyle(theme.text)
                        .textSelection(.enabled)
                        .padding(10)
                }
            }
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.codeBg))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(theme.border, lineWidth: 1))
        }
    }
}

/// Streaming fade (zeron "veil"): each appended chunk fades in by alpha only,
/// so the layout never jumps. Fade length tracks the arrival cadence
/// (3× the gap, clamped to 120–400 ms).
struct StreamingText: View {
    let text: String
    let font: Font
    let color: Color

    @State private var chunks: [(start: Int, end: Int, at: Date)] = []
    @State private var seen = 0
    @State private var lastArrival: Date?
    @State private var emaGapMs: Double = 120

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: chunks.isEmpty)) { context in
            render(now: context.date)
        }
        .onAppear { observe(text) }
        .onChange(of: text) { _, new in observe(new) }
    }

    private var fadeSeconds: Double { min(max(emaGapMs * 3, 120), 400) / 1000 }

    private func observe(_ new: String) {
        let len = new.utf8.count
        let now = Date()
        if len <= seen {
            seen = len
            chunks.removeAll { $0.start >= len }
            return
        }
        if let last = lastArrival {
            let gap = now.timeIntervalSince(last) * 1000
            emaGapMs = emaGapMs * 0.7 + gap * 0.3
        }
        lastArrival = now
        chunks.append((start: seen, end: len, at: now))
        seen = len
        let fade = fadeSeconds
        chunks.removeAll { now.timeIntervalSince($0.at) >= fade }
    }

    private func render(now: Date) -> Text {
        let fade = fadeSeconds
        let bytes = Array(text.utf8)
        var pieces = Text("")
        var cursor = 0
        let live = chunks.filter { now.timeIntervalSince($0.at) < fade && $0.start < bytes.count }
        for chunk in live.sorted(by: { $0.start < $1.start }) {
            let start = max(chunk.start, cursor)
            let end = min(chunk.end, bytes.count)
            if start > cursor {
                pieces = pieces + Text(string(bytes, cursor, start)).foregroundColor(color)
            }
            if end > start {
                let p = now.timeIntervalSince(chunk.at) / fade
                let alpha = 1 - pow(1 - p, 1.6)
                pieces = pieces + Text(string(bytes, start, end)).foregroundColor(color.opacity(alpha))
            }
            cursor = max(cursor, end)
        }
        if cursor < bytes.count {
            pieces = pieces + Text(string(bytes, cursor, bytes.count)).foregroundColor(color)
        }
        return pieces.font(font)
    }

    private func string(_ bytes: [UInt8], _ from: Int, _ to: Int) -> String {
        String(decoding: bytes[from..<to], as: UTF8.self)
    }
}
