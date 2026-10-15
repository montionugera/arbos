// The message stream, Grok Bot style: assistant bubbles (#202020) left, user
// bubbles (#5C5C5C) right, centred muted system lines between them, file and
// voice-memo cards, one "<Project> is working" line while a turn runs.
// Prose only: tool calls, status noise, usage and JSON never render.

import AVFoundation
import SwiftUI
import UIKit

enum Block: Equatable, Hashable {
    case paragraph(String)
    case heading(level: Int, text: String)
    case bullets([String])
    case numbered([String])
    case code(lang: String, code: String)
    case table([String])
}

enum RowKind: Equatable {
    case user(text: String, pending: Bool)
    case assistant(blocks: [Block], live: Bool)
    /// Centred muted line: hand-offs ("Messaged ◉ Affine"), stops, dates.
    case system(icon: String?, text: String, avatarProject: Id?)
    case question(text: String)
    case file(name: String, subtitle: String, projectId: Id, ref: String?)
    case voiceMemo(name: String, projectId: Id, ref: String?, fromUser: Bool)
    case image(projectId: Id, ref: String?, fromUser: Bool)
    case error(code: String, message: String)
}

struct Row: Identifiable, Equatable {
    var id: String
    var messageId: Id
    var role: Role
    var kind: RowKind
}

enum Transcript {
    static func rows(_ messages: [Message], projectId: Id, subagentNames: [String: String] = [:]) -> [Row] {
        var rows: [Row] = []
        var lastAt: Int64 = 0
        for message in messages {
            // Date separators when the gap is over an hour (Grok: "Today 8:00 AM").
            if message.createdAt > 0 && (lastAt == 0 || message.createdAt - lastAt > 3_600_000) {
                rows.append(Row(id: "\(message.id)#date", messageId: message.id, role: .system,
                                kind: .system(icon: nil, text: dateLabel(message.createdAt), avatarProject: nil)))
            }
            if message.createdAt > 0 { lastAt = message.createdAt }
            let streaming = message.isStreaming
            switch message.role {
            case .user:
                for part in message.parts {
                    if case .attachment(let id, let a) = part {
                        rows.append(attachmentRow(id: "\(message.id)#\(id)", message: message, a: a, projectId: projectId, fromUser: true))
                    }
                }
                let text = message.plainText
                if !text.isEmpty {
                    rows.append(Row(id: "\(message.id)#user", messageId: message.id, role: .user,
                                    kind: .user(text: text, pending: message.pending)))
                }
            case .assistant, .system, .project:
                // Hand-off ack: Grok shows "Messaged ◉ <bot>" as a system line.
                if message.turn == .new, let name = message.subagentName ?? subagentNames[message.subagentId ?? ""] {
                    rows.append(Row(id: "\(message.id)#handoff", messageId: message.id, role: .system,
                                    kind: .system(icon: nil, text: "Messaged \u{FFFC}\(name)", avatarProject: projectId)))
                }
                let lastTextPart = message.parts.lastIndex { $0.textValue != nil }
                for (pix, part) in message.parts.enumerated() {
                    switch part {
                    case .text(let id, let text):
                        let blocks = Markdown.blocks(text).filter { !Markdown.isJSON($0) }
                        if blocks.isEmpty { continue }
                        let live = streaming && pix == lastTextPart
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant,
                                        kind: .assistant(blocks: blocks, live: live)))
                    case .status(let id, .askUser, let text):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant, kind: .question(text: text)))
                    case .status(let id, .handoff, let text):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .system,
                                        kind: .system(icon: nil, text: handoffText(text), avatarProject: nil)))
                    case .status(let id, .stopped, _):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .system,
                                        kind: .system(icon: "stop.circle", text: "Stopped", avatarProject: nil)))
                    case .attachment(let id, let a):
                        rows.append(attachmentRow(id: "\(message.id)#\(id)", message: message, a: a, projectId: projectId, fromUser: false))
                    case .error(let id, let code, let text):
                        rows.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant, kind: .error(code: code, message: text)))
                    case .status, .tool, .subagentResult, .reasoning, .unknown:
                        break
                    }
                }
                if message.status == .error && !message.hasErrorPart {
                    rows.append(Row(id: "\(message.id)#failed", messageId: message.id, role: .assistant,
                                    kind: .error(code: "turn_failed", message: "The agent could not finish this turn.")))
                }
            }
        }
        return rows
    }

    private static func attachmentRow(id: String, message: Message, a: Attachment, projectId: Id, fromUser: Bool) -> Row {
        if a.mime.hasPrefix("audio/") {
            return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                       kind: .voiceMemo(name: a.name, projectId: projectId, ref: a.blobRef, fromUser: fromUser))
        }
        if a.mime.hasPrefix("image/") {
            return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                       kind: .image(projectId: projectId, ref: a.blobRef, fromUser: fromUser))
        }
        return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                   kind: .file(name: a.name, subtitle: subtitle(for: a), projectId: projectId, ref: a.blobRef))
    }

    private static func subtitle(for a: Attachment) -> String {
        if let dot = a.name.lastIndex(of: ".") {
            return String(a.name[a.name.index(after: dot)...]).uppercased() + " file"
        }
        return a.mime
    }

    /// "Handed to subagent itsdangerous-tests" → "Messaged ◉ itsdangerous-tests".
    private static func handoffText(_ raw: String) -> String {
        let t = raw.trimmingCharacters(in: .whitespaces)
        for prefix in ["Handed to subagent ", "Handed to ", "Handed off to "] where t.hasPrefix(prefix) {
            return "Messaged \u{FFFC}" + t.dropFirst(prefix.count)
        }
        return t
    }

    static func dateLabel(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let cal = Calendar.current
        let time = date.formatted(date: .omitted, time: .shortened)
        if cal.isDateInToday(date) { return "Today \(time)" }
        if cal.isDateInYesterday(date) { return "Yesterday \(time)" }
        if let week = cal.date(byAdding: .day, value: -6, to: Date()), date > week {
            return date.formatted(.dateTime.weekday(.wide)) + " \(time)"
        }
        return date.formatted(date: .abbreviated, time: .shortened)
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
    let width: CGFloat

    private var maxBubble: CGFloat { max(200, width - Theme.bubbleInset) }

    var body: some View {
        switch row.kind {
        case .user(let text, let pending):
            HStack {
                Spacer(minLength: 0)
                Text(Markdown.inline(text))
                    .font(.system(size: Theme.body))
                    .lineSpacing(Theme.bodyLineSpacing)
                    .foregroundStyle(theme.userText)
                    .textSelection(.enabled)
                    .padding(.horizontal, Theme.bubbleHPad)
                    .padding(.vertical, Theme.bubbleVPad)
                    .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleUser))
                    .frame(maxWidth: maxBubble, alignment: .trailing)
                    .opacity(pending ? 0.6 : 1)
            }
        case .assistant(let blocks, let live):
            HStack {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(blocks.enumerated()), id: \.offset) { i, block in
                        BlockView(block: block, live: live && i == blocks.count - 1)
                    }
                }
                .padding(.horizontal, Theme.bubbleHPad)
                .padding(.vertical, Theme.bubbleVPad)
                .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                .frame(maxWidth: maxBubble, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .system(let icon, let text, let avatarProject):
            HStack(spacing: 5) {
                if let icon {
                    Image(systemName: icon).font(.system(size: 13, weight: .regular))
                }
                systemText(text, avatarProject: avatarProject)
            }
            .font(.system(size: Theme.small))
            .foregroundStyle(theme.muted)
            .frame(maxWidth: .infinity)
            .multilineTextAlignment(.center)
        case .question(let text):
            HStack {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Question")
                        .font(.system(size: Theme.tiny, weight: .semibold))
                        .foregroundStyle(theme.muted)
                    Text(Markdown.inline(text))
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                }
                .padding(.horizontal, Theme.bubbleHPad)
                .padding(.vertical, Theme.bubbleVPad + 2)
                .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                .overlay(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).strokeBorder(theme.warn.opacity(0.5), lineWidth: 1))
                .frame(maxWidth: maxBubble, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .file(let name, let subtitle, let projectId, let ref):
            HStack {
                if row.role == .user { Spacer(minLength: 0) }
                FileCard(name: name, subtitle: subtitle, projectId: projectId, blobRef: ref, fromUser: row.role == .user)
                    .frame(maxWidth: maxBubble, alignment: row.role == .user ? .trailing : .leading)
                if row.role != .user { Spacer(minLength: 0) }
            }
        case .voiceMemo(let name, let projectId, let ref, let fromUser):
            HStack {
                if fromUser { Spacer(minLength: 0) }
                VoiceMemoBubble(name: name, projectId: projectId, blobRef: ref, fromUser: fromUser)
                    .frame(maxWidth: maxBubble, alignment: fromUser ? .trailing : .leading)
                if !fromUser { Spacer(minLength: 0) }
            }
        case .image(let projectId, let ref, let fromUser):
            HStack {
                if fromUser { Spacer(minLength: 0) }
                Group {
                    if let ref, let url = model.blobURL(projectId: projectId, key: ref) {
                        AsyncImage(url: url) { phase in
                            switch phase {
                            case .success(let img): img.resizable().scaledToFill()
                            case .failure: Image(systemName: "photo").font(.system(size: 28)).foregroundStyle(theme.muted)
                            case .empty: ProgressView().tint(theme.muted)
                            @unknown default: Color.clear
                            }
                        }
                    } else {
                        Image(systemName: "photo").font(.system(size: 28)).foregroundStyle(theme.muted)
                    }
                }
                .frame(width: min(240, maxBubble), height: 180)
                .background(theme.bubbleAssistant)
                .clipShape(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous))
                if !fromUser { Spacer(minLength: 0) }
            }
        case .error(let code, let message):
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.circle").foregroundStyle(theme.danger)
                Text(message).font(.system(size: Theme.small)).foregroundStyle(theme.text)
                Spacer(minLength: 0)
                Text(code).font(.system(size: 11)).foregroundStyle(theme.muted)
                Button("Retry") { model.retry(chatId: chatId, messageId: row.messageId) }
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(theme.text)
            }
            .padding(.horizontal, Theme.bubbleHPad).padding(.vertical, 10)
            .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.dangerSoft))
        }
    }

    /// "Messaged \u{FFFC}Affine" draws a small avatar where the object marker is.
    @ViewBuilder
    private func systemText(_ text: String, avatarProject: Id?) -> some View {
        if let range = text.range(of: "\u{FFFC}") {
            let before = String(text[..<range.lowerBound])
            let after = String(text[range.upperBound...])
            let target = model.projects.first { $0.name.caseInsensitiveCompare(after) == .orderedSame }
            HStack(spacing: 5) {
                Text(before)
                if let target {
                    BlobAvatar(project: target, theme: theme, size: 16)
                } else {
                    BlobAvatar(color: theme.muted, shape: BlobShape.pick(for: after), size: 16)
                }
                Text(after)
            }
        } else {
            Text(text)
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
                    .lineSpacing(Theme.bodyLineSpacing)
            } else {
                Text(Markdown.inline(text))
                    .font(.system(size: Theme.body))
                    .lineSpacing(Theme.bodyLineSpacing)
                    .foregroundStyle(theme.text)
                    .textSelection(.enabled)
            }
        case .heading(_, let text):
            Text(Markdown.inline(text))
                .font(.system(size: Theme.body, weight: .semibold))
                .foregroundStyle(theme.text)
        case .bullets(let items), .numbered(let items):
            let numbered: Bool = { if case .numbered = block { return true } else { return false } }()
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(numbered ? "\(i + 1)." : "•")
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.text)
                            .frame(minWidth: 16, alignment: .trailing)
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
                            .font(.system(size: 14, weight: i == 0 ? .semibold : .regular, design: .monospaced))
                            .foregroundStyle(theme.text)
                    }
                }
                .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.bg.opacity(0.6)))
        case .code(_, let code):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(size: 14, design: .monospaced))
                    .foregroundStyle(theme.text)
                    .textSelection(.enabled)
                    .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.bg.opacity(0.6)))
        }
    }
}

/// Document card (Grok: icon, title, domain) for a non-audio attachment.
struct FileCard: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let name: String
    let subtitle: String
    let projectId: Id
    let blobRef: String?
    let fromUser: Bool

    var body: some View {
        Button {
            if let ref = blobRef, let url = model.blobURL(projectId: projectId, key: ref) { UIApplication.shared.open(url) }
        } label: {
            HStack(spacing: 10) {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color(hex: "#FFB000")!)
                    .frame(width: 22, height: 26)
                    .overlay(Image(systemName: "doc.text").font(.system(size: 12, weight: .bold)).foregroundStyle(.black))
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.system(size: Theme.body)).foregroundStyle(fromUser ? theme.userText : theme.text).lineLimit(1)
                    Text(subtitle).font(.system(size: Theme.small)).foregroundStyle(fromUser ? theme.userText.opacity(0.7) : theme.muted).lineLimit(1)
                }
            }
            .padding(.horizontal, Theme.bubbleHPad)
            .padding(.vertical, Theme.bubbleVPad)
            .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(fromUser ? theme.bubbleUser : theme.bubbleAssistant))
        }
        .buttonStyle(.plain)
    }
}

/// Voice memo bubble: play/pause, a static waveform, duration.
struct VoiceMemoBubble: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let name: String
    let projectId: Id
    let blobRef: String?
    let fromUser: Bool
    @StateObject private var player = MemoPlayer()

    var body: some View {
        HStack(spacing: 10) {
            Button {
                if let ref = blobRef, let url = model.blobURL(projectId: projectId, key: ref) {
                    player.toggle(url: url, headers: model.authHeaders)
                }
            } label: {
                Circle().fill(theme.primary).frame(width: 32, height: 32)
                    .overlay(Image(systemName: player.playing ? "pause.fill" : "play.fill").font(.system(size: 13, weight: .bold)).foregroundStyle(theme.onPrimary).offset(x: player.playing ? 0 : 1))
            }
            .buttonStyle(.plain)
            HStack(spacing: 2) {
                ForEach(0..<22, id: \.self) { i in
                    let played: Bool = Double(i) / 22.0 < player.progress
                    let barHeight: CGFloat = 6 + CGFloat((i * 7) % 13)
                    Capsule()
                        .fill((fromUser ? theme.userText : theme.text).opacity(played ? 1 : 0.35))
                        .frame(width: 3, height: barHeight)
                }
            }
            Text(player.durationLabel)
                .font(.system(size: Theme.tiny, design: .monospaced))
                .foregroundStyle(fromUser ? theme.userText.opacity(0.8) : theme.muted)
        }
        .padding(.horizontal, Theme.bubbleHPad)
        .padding(.vertical, Theme.bubbleVPad)
        .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(fromUser ? theme.bubbleUser : theme.bubbleAssistant))
    }
}

@MainActor
final class MemoPlayer: ObservableObject {
    @Published var playing = false
    @Published var progress: Double = 0
    @Published var durationLabel = "0:00"
    private var player: AVPlayer?
    private var timeObserver: Any?

    func toggle(url: URL, headers: [String: String]) {
        if let player {
            if playing { player.pause(); playing = false } else { player.play(); playing = true }
            return
        }
        let asset = AVURLAsset(url: url, options: ["AVURLAssetHTTPHeaderFieldsKey": headers])
        let item = AVPlayerItem(asset: asset)
        let p = AVPlayer(playerItem: item)
        player = p
        timeObserver = p.addPeriodicTimeObserver(forInterval: CMTime(seconds: 0.25, preferredTimescale: 600), queue: .main) { [weak self] t in
            guard let self, let d = p.currentItem?.duration.seconds, d.isFinite, d > 0 else { return }
            self.progress = t.seconds / d
            let remaining = Int(max(0, d - t.seconds))
            self.durationLabel = String(format: "%d:%02d", remaining / 60, remaining % 60)
            if t.seconds >= d - 0.05 { self.playing = false; p.seek(to: .zero); self.progress = 0 }
        }
        p.play()
        playing = true
    }
}

/// Streaming fade (the "veil"): each appended chunk fades in by alpha only.
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
