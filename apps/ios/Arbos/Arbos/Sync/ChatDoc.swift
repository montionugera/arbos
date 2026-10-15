// Chat doc on Loro, shaped as crdt-sync §6.2 (same schema `arbos-core::doc`
// writes on the box side):
//
//   meta:      Map { chatId, projectId, schemaVersion, epoch }
//   messages:  List<Map { id, role, ..., parts: List<Map { kind, text: Text, ... }> }>
//   commands:  List<Map { id, kind, payload, issuedBy, issuedAt, status }>
//
// Reading goes through `getDeepValue` → JSON → `Message`, so the schema stays
// additive. The same wrapper holds the project doc (`settings`, `status`,
// `threads`, `memory`).

import Foundation
import Loro

/// What a root-level diff touched, mapped to message indexes.
struct Touched {
    /// The `messages` list itself changed (insert/delete): re-read everything.
    var listChanged = false
    var messages = Set<Int>()
    /// Anything outside `messages` (project doc maps, `commands`).
    var other = false
}

final class ChatDoc {
    let key: Id
    let doc: LoroDoc
    private var subscriptions: [Subscription] = []

    init(key: Id) {
        self.key = key
        self.doc = LoroDoc()
    }

    var messages: LoroList { doc.getList(id: "messages") }
    var commands: LoroList { doc.getList(id: "commands") }

    var count: Int { Int(messages.len()) }

    func commit() { doc.commit() }

    func importUpdate(_ bytes: Data) throws {
        _ = try doc.import(bytes: bytes)
    }

    /// Does the replica already contain this checkpoint frontier?
    func contains(frontier: Data) -> Bool {
        guard !frontier.isEmpty, let vv = try? VersionVector.decode(bytes: frontier) else { return false }
        return doc.oplogVv().includesVv(other: vv)
    }

    // MARK: Reading

    func readMessage(_ ix: Int) -> Message? {
        guard ix >= 0, ix < count, let map = messages.get(index: UInt32(ix))?.asLoroMap() else { return nil }
        return DocJSON.message(LoroJSON.any(map.getDeepValue()), chatId: key)
    }

    func readAll() -> [Message] {
        (0..<count).compactMap(readMessage)
    }

    func mapValue(_ name: String) -> [String: Any] {
        LoroJSON.any(doc.getMap(id: name).getDeepValue()) as? [String: Any] ?? [:]
    }

    // MARK: Writing (client side only: user rows + commands, §4.3)

    /// Append a user message as nested containers. Does not commit.
    func insertUserMessage(id: Id, text: String, deviceId: String, attachments: [Attachment] = []) throws {
        let list = messages
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: id)
        try map.insert(key: "chatId", v: key)
        try map.insert(key: "role", v: "user")
        try map.insert(key: "sender", v: "user")
        try map.insert(key: "status", v: "done")
        try map.insert(key: "deviceId", v: deviceId)
        try map.insert(key: "createdAt", v: nowMs())
        try map.insert(key: "waitingUser", v: false)
        try map.insert(key: "pending", v: false)
        for k in ["turn", "decision", "subagentId", "subagentName", "inReplyTo", "usage", "returnContract", "model", "effort", "completedAt"] {
            try map.insert(key: k, v: LoroValue.null)
        }
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        for a in attachments {
            let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
            try part.insert(key: "id", v: newId())
            try part.insert(key: "kind", v: "attachment")
            let att = try part.insertContainer(key: "attachment", child: LoroMap())
            try att.insert(key: "name", v: a.name)
            try att.insert(key: "mime", v: a.mime)
            if let ref = a.blobRef { try att.insert(key: "ref", v: ref) } else { try att.insert(key: "ref", v: LoroValue.null) }
        }
        let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
        try part.insert(key: "id", v: newId())
        try part.insert(key: "kind", v: "text")
        let textC = try part.insertContainer(key: "text", child: LoroText())
        try textC.pushStr(s: text)
        _ = try map.insertContainer(key: "hiddenParts", child: LoroList())
    }

    /// Append a `commands` row (`send`, `stop`, `answer`, `retry`, `retitle`). Does not commit.
    func pushCommand(_ kind: String, payload: [String: Any], device: String) throws {
        let list = commands
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: newId())
        try map.insert(key: "kind", v: kind)
        let p = try map.insertContainer(key: "payload", child: LoroMap())
        for (k, v) in payload {
            try LoroJSON.insert(into: p, key: k, value: v)
        }
        try map.insert(key: "issuedBy", v: device)
        try map.insert(key: "issuedAt", v: nowMs())
        try map.insert(key: "status", v: "pending")
    }

    /// Mirror of the model picker into the project doc `settings` (crdt-sync §6.3).
    func setModelSettings(model: String, effort: Effort) throws {
        let settings = doc.getMap(id: "settings")
        try settings.insert(key: "agentModel", v: model)
        try settings.insert(key: "agentEffort", v: effort.rawValue)
        let models: [String: LoroValue] = [
            "talking_model": .map(value: ["id": .string(value: model), "effort": .string(value: effort.rawValue)]),
        ]
        try settings.insert(key: "models", v: LoroValue.map(value: models))
    }

    // MARK: Subscriptions

    func onChange(_ handler: @escaping (Touched) -> Void) {
        let sub = doc.subscribeRoot { event in
            handler(ChatDoc.touched(event))
        }
        subscriptions.append(sub)
    }

    func onLocalUpdate(_ handler: @escaping (Data) -> Void) {
        let sub = doc.subscribeLocalUpdate(callback: handler)
        subscriptions.append(sub)
    }

    /// Map a root diff to message indexes. Paths look like
    /// `[(root, key "messages"), (list, seq i), (map, key "parts"), ...]`.
    static func touched(_ event: DiffEvent) -> Touched {
        var out = Touched()
        for diff in event.events {
            var sawMessages = false
            var found: Int? = nil
            var broke = false
            for item in diff.path {
                switch item.index {
                case .key(let k):
                    if !sawMessages {
                        if k == "messages" {
                            sawMessages = true
                        } else {
                            broke = true
                        }
                    }
                case .seq(let i):
                    if sawMessages {
                        found = Int(i)
                    }
                case .node:
                    break
                }
                if broke || found != nil { break }
            }
            if !sawMessages {
                // A diff whose target is the root `messages` list itself has an empty path
                // only when it is the root container: check the target id.
                if case .root(let name, _) = diff.target, name == "messages" {
                    out.listChanged = true
                } else {
                    out.other = true
                }
                continue
            }
            if let i = found {
                out.messages.insert(i)
            } else {
                out.listChanged = true
            }
        }
        return out
    }
}

/// LoroValue ↔ Foundation JSON.
enum LoroJSON {
    static func any(_ v: LoroValue) -> Any {
        switch v {
        case .null: return NSNull()
        case .bool(let b): return b
        case .double(let d): return d
        case .i64(let i): return NSNumber(value: i)
        case .binary(let d): return d.base64EncodedString()
        case .string(let s): return s
        case .list(let items): return items.map(any)
        case .map(let m): return m.mapValues(any)
        case .container: return NSNull()
        }
    }

    /// Write a JSON-ish value under `key`. Objects become `LoroMap`, arrays `LoroList`.
    static func insert(into map: LoroMap, key: String, value: Any) throws {
        switch value {
        case is NSNull:
            try map.insert(key: key, v: LoroValue.null)
        case let b as Bool:
            try map.insert(key: key, v: b)
        case let n as NSNumber:
            if CFNumberIsFloatType(n) {
                try map.insert(key: key, v: n.doubleValue)
            } else {
                try map.insert(key: key, v: n.int64Value)
            }
        case let s as String:
            try map.insert(key: key, v: s)
        case let arr as [Any]:
            let list = try map.insertContainer(key: key, child: LoroList())
            for (i, item) in arr.enumerated() {
                try insert(into: list, pos: UInt32(i), value: item)
            }
        case let obj as [String: Any]:
            let child = try map.insertContainer(key: key, child: LoroMap())
            for (k, v) in obj {
                try insert(into: child, key: k, value: v)
            }
        default:
            try map.insert(key: key, v: String(describing: value))
        }
    }

    static func insert(into list: LoroList, pos: UInt32, value: Any) throws {
        switch value {
        case is NSNull:
            try list.insert(pos: pos, v: LoroValue.null)
        case let b as Bool:
            try list.insert(pos: pos, v: b)
        case let n as NSNumber:
            if CFNumberIsFloatType(n) {
                try list.insert(pos: pos, v: n.doubleValue)
            } else {
                try list.insert(pos: pos, v: n.int64Value)
            }
        case let s as String:
            try list.insert(pos: pos, v: s)
        case let arr as [Any]:
            let child = try list.insertContainer(pos: pos, child: LoroList())
            for (i, item) in arr.enumerated() {
                try insert(into: child, pos: UInt32(i), value: item)
            }
        case let obj as [String: Any]:
            let child = try list.insertContainer(pos: pos, child: LoroMap())
            for (k, v) in obj {
                try insert(into: child, key: k, value: v)
            }
        default:
            try list.insert(pos: pos, v: String(describing: value))
        }
    }
}
