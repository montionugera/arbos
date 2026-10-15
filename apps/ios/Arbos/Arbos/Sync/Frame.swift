// Binary room frames (crdt-sync §7.1–7.2), byte-compatible with
// `arbos_core::wire`: `[type: u8][headerLen: u32 LE][header: JSON][payload]`.

import Foundation

enum FrameType: UInt8 {
    case hello = 0x01
    case state = 0x02
    case rowsReq = 0x03
    case row = 0x04
    case rowsDone = 0x05
    case push = 0x06
    case ack = 0x07
    case presence = 0x08
    case probe = 0x09
    case probeOk = 0x0a
    case error = 0x0b
    case status = 0x0c
    case wake = 0x0d
}

let protoVersion = 1
let headerMax = 4096
/// Row cap (crdt-sync §7.6).
let rowMax = 1024 * 1024 - 4096
let projectDocChatId = "project"

struct Frame {
    var type: FrameType
    var header: [String: Any]
    var payload: Data

    init(_ type: FrameType, header: [String: Any] = [:], payload: Data = Data()) {
        self.type = type
        self.header = header
        self.payload = payload
    }

    func encode() -> Data {
        let json = (try? JSONSerialization.data(withJSONObject: header)) ?? Data("{}".utf8)
        var out = Data(capacity: 5 + json.count + payload.count)
        out.append(type.rawValue)
        var len = UInt32(json.count).littleEndian
        withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
        out.append(json)
        out.append(payload)
        return out
    }

    /// `nil` for an unknown type so callers skip it (crdt-sync §7.2).
    static func decode(_ data: Data) -> Frame? {
        guard data.count >= 5 else { return nil }
        let bytes = [UInt8](data.prefix(5))
        let typeByte = bytes[0]
        let len = Int(UInt32(bytes[1]) | UInt32(bytes[2]) << 8 | UInt32(bytes[3]) << 16 | UInt32(bytes[4]) << 24)
        guard len <= headerMax, data.count >= 5 + len else { return nil }
        let headerData = data.subdata(in: (data.startIndex + 5)..<(data.startIndex + 5 + len))
        let payload = data.subdata(in: (data.startIndex + 5 + len)..<data.endIndex)
        guard let type = FrameType(rawValue: typeByte) else { return nil }
        let header = (try? JSONSerialization.jsonObject(with: headerData)) as? [String: Any] ?? [:]
        return Frame(type, header: header, payload: payload)
    }

    func string(_ key: String) -> String? { header[key] as? String }

    func int(_ key: String) -> Int64? {
        if let n = header[key] as? NSNumber { return n.int64Value }
        return nil
    }
}

func roomName(_ projectId: String, _ chatId: String) -> String {
    "\(projectId)/\(chatId)"
}
