// Plain Swift view of the Project doc and the Chat docs (arbos-design §5,
// crdt-sync §6). Mirrors `crates/arbos-mac/src/model.rs`; the sync layer
// fills these in and the views only read them.

import Foundation

typealias Id = String

enum BoxState: String, Codable, Equatable {
    case awake
    case waking
    case hibernatedLight = "hibernated_light"
    case hibernatedDeep = "hibernated_deep"
    case stoppedByUser = "stopped_by_user"
    case error

    var isAsleep: Bool {
        switch self {
        case .hibernatedLight, .hibernatedDeep, .stoppedByUser: return true
        case .awake, .waking, .error: return false
        }
    }

    /// Maps the relay's registry `state` plus the live `box` field.
    static func from(state: String, box: String) -> BoxState {
        switch state {
        case "awake": return .awake
        case "waking": return .waking
        case "hibernated_light": return .hibernatedLight
        case "hibernated_deep": return .hibernatedDeep
        case "stopped_by_user": return .stoppedByUser
        case "error": return .error
        default:
            switch box {
            case "awake": return .awake
            case "waking": return .waking
            case "error": return .error
            default: return .hibernatedLight
            }
        }
    }

    /// Wire `box` values: awake | waking | asleep | error (crdt-sync §8.2).
    static func fromWire(_ s: String) -> BoxState? {
        switch s {
        case "awake": return .awake
        case "waking": return .waking
        case "asleep": return .hibernatedLight
        case "error": return .error
        default: return nil
        }
    }
}

enum Effort: String, Codable, CaseIterable, Identifiable {
    case low, medium, high

    var id: String { rawValue }

    var label: String {
        switch self {
        case .low: return "Low"
        case .medium: return "Medium"
        case .high: return "High"
        }
    }

    static func from(_ s: String?) -> Effort {
        switch s ?? "" {
        case "high", "xhigh", "max": return .high
        case "medium": return .medium
        default: return .low
        }
    }
}

/// The user's one Daytona computer; projects are workspaces on it. Mirrored
/// into every project doc as `status.box { state, since }`; until the relay
/// reports it directly, the app derives it from the projects' box states.
struct Machine: Equatable {
    var state: BoxState
    var since: Int64?
    /// Human spec line, e.g. "Daytona · 4 vCPU · 8 GB"; empty when unknown.
    var spec: String

    static let unknown = Machine(state: .hibernatedLight, since: nil, spec: "")

    var label: String {
        switch state {
        case .awake: return "Awake"
        case .waking: return "Waking…"
        case .hibernatedLight, .hibernatedDeep: return "Sleeping"
        case .stoppedByUser: return "Stopped"
        case .error: return "Needs attention"
        }
    }

    /// Fold per-project box states into one machine state. A `waking` older
    /// than five minutes is a wake that never finished; it counts as asleep.
    static func aggregate(_ projects: [Project]) -> BoxState {
        let stale = nowMs() - 5 * 60 * 1000
        let states = projects.map { p -> BoxState in
            if p.state == .waking, let since = p.stateSince, since > 0, since < stale { return .hibernatedLight }
            return p.state
        }
        if states.contains(.error) { return .error }
        if states.contains(.waking) { return .waking }
        if states.contains(.awake) { return .awake }
        if !states.isEmpty && states.allSatisfy({ $0 == .stoppedByUser }) { return .stoppedByUser }
        return .hibernatedLight
    }
}

/// `status.workspace.state` of a project doc: the project's workspace on the machine.
enum WorkspaceState: String, Equatable {
    case active, idle, error
    case unknown

    static func from(_ raw: String?) -> WorkspaceState {
        switch raw ?? "" {
        case "active", "awake", "running", "busy": return .active
        case "idle", "asleep", "sleeping", "stopped": return .idle
        case "error", "failed": return .error
        default: return .unknown
        }
    }
}

struct Project: Identifiable, Equatable {
    var id: Id
    var name: String
    /// One glyph (or two-letter monogram), drawn in the tile.
    var icon: String
    /// Hex `#rrggbb` accent for the selected tile border.
    var color: String
    var state: BoxState
    /// Relay `box_since` (ms); lets a wake that never finished age out.
    var stateSince: Int64?
    /// Human text for the S12/S13 banner, e.g. "Restoring from archive…".
    var stateDetail: String?
    var model: String
    var effort: Effort
    var spendTodayUsd: Double
    var spendCapUsd: Double
    /// `settings.modelNote` from the project doc; empty when every id is valid.
    var modelNote: String = ""
    /// `status.workspace.state` from the project doc; `.unknown` until the box writes it.
    var workspace: WorkspaceState = .unknown

    var shortModel: String { Project.shortModel(model) }

    static func shortModel(_ id: String) -> String {
        id.split(separator: "/").last.map(String.init) ?? id
    }
}

struct Chat: Identifiable, Equatable {
    var id: Id
    var projectId: Id
    var title: String
    var isPinned: Bool
    var unread: Bool
    var lastMessageAt: Int64
}

enum Role: String, Codable {
    case user, assistant, system, project
}

enum MessageStatus: String, Codable {
    case queued, streaming, done, error, cancelled
}

enum TurnKind: String, Codable {
    case reply, act, new, spawn, wake
    case subagentResult = "subagent_result"
    case other

    /// Progress messages carry status events only; no prose is expected.
    var isProgress: Bool { self == .spawn || self == .wake }
}

enum StatusKind: String, Codable {
    case deciding, working, step, queued, done, failed, stopped
    case askUser = "waiting_user"
    case handoff = "handed_off"
    case note
    case other
}

struct ToolCall: Codable, Equatable {
    var id: Id
    var name: String
}

struct Attachment: Codable, Equatable {
    var name: String
    var mime: String
    var blobRef: String?

    enum CodingKeys: String, CodingKey {
        case name, mime
        case blobRef = "ref"
    }
}

/// One row of `messages[].parts` (crdt-sync §6.2). Unknown kinds decode to
/// `.unknown` and render as nothing, per the "additive only" rule.
enum Part: Equatable {
    case text(id: Id, text: String)
    case reasoning(id: Id)
    case tool(id: Id, call: ToolCall, isError: Bool)
    case attachment(id: Id, attachment: Attachment)
    case error(id: Id, code: String, message: String)
    case status(id: Id, code: StatusKind, message: String)
    case subagentResult(id: Id)
    case unknown(id: Id)

    var id: Id {
        switch self {
        case .text(let id, _), .reasoning(let id), .tool(let id, _, _), .attachment(let id, _),
             .error(let id, _, _), .status(let id, _, _), .subagentResult(let id), .unknown(let id):
            return id
        }
    }

    var isStatus: Bool {
        if case .status = self { return true }
        return false
    }

    var textValue: String? {
        if case .text(_, let t) = self { return t }
        return nil
    }
}

struct Message: Identifiable, Equatable {
    var id: Id
    var chatId: Id
    var role: Role
    var sender: String
    var status: MessageStatus
    var turn: TurnKind?
    var decision: String?
    var subagentId: String?
    var subagentName: String?
    var inReplyTo: String?
    var usage: [String: Double]
    var model: String?
    var createdAt: Int64
    var completedAt: Int64?
    /// True until the relay `ACK`s the client's push (S3 faint echo). Client-only.
    var pending: Bool
    var parts: [Part]
    /// Count of parts the box flagged `hidden: true` (tool details).
    var hiddenCount: Int

    var isStreaming: Bool { status == .streaming }

    var plainText: String {
        parts.compactMap(\.textValue).joined(separator: "\n")
    }

    var textLength: Int {
        parts.compactMap(\.textValue).reduce(0) { $0 + $1.utf8.count }
    }

    var askUserText: String? {
        for part in parts {
            if case .status(_, .askUser, let message) = part { return message }
        }
        return nil
    }

    var hasErrorPart: Bool {
        parts.contains { if case .error = $0 { return true } else { return false } }
    }
}

/// Connection to the relay, S14.
enum Link: Equatable {
    case connecting, live, reconnecting
}

func nowMs() -> Int64 {
    Int64(Date().timeIntervalSince1970 * 1000)
}

// MARK: - JSON decoding of doc rows

/// Decodes a `messages[i]` map (as Foundation JSON) into a `Message`.
/// Tolerant: every field but `id` has a default, so the schema stays additive.
enum DocJSON {
    static func message(_ raw: Any, chatId: Id) -> Message? {
        guard let m = raw as? [String: Any], let id = m["id"] as? String else { return nil }
        let role = Role(rawValue: str(m["role"]) ?? "") ?? .assistant
        let status = MessageStatus(rawValue: str(m["status"]) ?? "") ?? .done
        var turn: TurnKind? = nil
        if let t = m["turn"] as? String {
            turn = TurnKind(rawValue: t) ?? .other
        } else if let t = m["turn"] as? [String: Any], let k = t["kind"] as? String {
            turn = TurnKind(rawValue: k) ?? .other
        }
        var parts: [Part] = []
        var hidden = 0
        for p in (m["parts"] as? [Any]) ?? [] {
            guard let pm = p as? [String: Any] else { continue }
            if (pm["hidden"] as? Bool) == true || (pm["hidden"] as? NSNumber)?.boolValue == true {
                hidden += 1
                continue
            }
            parts.append(part(pm))
        }
        var usage: [String: Double] = [:]
        if let u = m["usage"] as? [String: Any] {
            for (k, v) in u {
                if let n = v as? NSNumber { usage[k] = n.doubleValue }
            }
        }
        return Message(
            id: id,
            chatId: str(m["chatId"]) ?? chatId,
            role: role,
            sender: str(m["sender"]) ?? "",
            status: status,
            turn: turn,
            decision: str(m["decision"]),
            subagentId: str(m["subagentId"]),
            subagentName: str(m["subagentName"]),
            inReplyTo: str(m["inReplyTo"]),
            usage: usage,
            model: str(m["model"]),
            createdAt: int(m["createdAt"]) ?? 0,
            completedAt: int(m["completedAt"]),
            pending: false,
            parts: parts,
            hiddenCount: hidden
        )
    }

    static func part(_ pm: [String: Any]) -> Part {
        let id = str(pm["id"]) ?? UUID().uuidString
        switch str(pm["kind"]) ?? "" {
        case "text":
            return .text(id: id, text: str(pm["text"]) ?? "")
        case "reasoning":
            return .reasoning(id: id)
        case "tool":
            let call = pm["call"] as? [String: Any]
            return .tool(
                id: id,
                call: ToolCall(id: str(call?["id"]) ?? "", name: str(call?["name"]) ?? "tool"),
                isError: (pm["isError"] as? Bool) ?? false
            )
        case "attachment":
            let a = pm["attachment"] as? [String: Any]
            return .attachment(
                id: id,
                attachment: Attachment(
                    name: str(a?["name"]) ?? "attachment",
                    mime: str(a?["mime"]) ?? "application/octet-stream",
                    blobRef: str(a?["ref"])
                )
            )
        case "error":
            return .error(id: id, code: str(pm["code"]) ?? "error", message: str(pm["message"]) ?? "")
        case "status":
            let code = StatusKind(rawValue: str(pm["code"]) ?? "") ?? .other
            return .status(id: id, code: code, message: str(pm["message"]) ?? "")
        case "subagent_result":
            return .subagentResult(id: id)
        default:
            return .unknown(id: id)
        }
    }

    static func str(_ v: Any?) -> String? {
        if let s = v as? String { return s }
        return nil
    }

    static func int(_ v: Any?) -> Int64? {
        if let n = v as? NSNumber { return n.int64Value }
        if let i = v as? Int64 { return i }
        if let i = v as? Int { return Int64(i) }
        if let d = v as? Double { return Int64(d) }
        return nil
    }
}

func newId() -> Id {
    ULID.generate()
}

/// Crockford base32 ULID: 48-bit ms timestamp + 80 random bits (26 chars).
enum ULID {
    private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

    static func generate() -> String {
        var out = [Character](repeating: "0", count: 26)
        var t = UInt64(Date().timeIntervalSince1970 * 1000)
        for i in stride(from: 9, through: 0, by: -1) {
            out[i] = alphabet[Int(t & 0x1f)]
            t >>= 5
        }
        var rng = SystemRandomNumberGenerator()
        for i in 10..<26 {
            out[i] = alphabet[Int(rng.next() % 32)]
        }
        return String(out)
    }
}
