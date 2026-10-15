// One WebSocket per room on `arbos-relay` (crdt-sync §7–§8): dial, HELLO,
// STATE, checkpoint-on-connect, ROWS_REQ backfill, then live ROW / PUSH /
// ACK / STATUS / WAKE, forever, with backoff. The Loro doc lives in `ChatDoc`;
// this type only moves bytes.

import Foundation

@MainActor
final class Room {
    let projectId: Id
    let key: Id
    let roomChat: String
    let doc: ChatDoc

    var onLink: ((Link) -> Void)?
    var onBoxState: ((BoxState, String?) -> Void)?
    var onCaughtUp: (() -> Void)?
    var onAcked: (() -> Void)?

    private let api: RelayAPI
    private let deviceId: () -> String
    private var cursor: Int64 = 0
    /// Pushes the relay has not `ACK`ed yet: `(batchId, bytes)`.
    private var pending: [(String, Data)] = []
    private var socket: URLSessionWebSocketTask?
    private var loop: Task<Void, Never>?
    private var presence: Task<Void, Never>?
    private var wakeQueue: [String] = []
    private let session: URLSession

    private static let helloDeadline: Double = 15
    private static let presenceEvery: Double = 15
    private static let backoffMin: Double = 0.25
    private static let backoffMax: Double = 16
    private static let backoffResetAfter: Double = 30

    init(projectId: Id, key: Id, roomChat: String, doc: ChatDoc, api: RelayAPI, deviceId: @escaping () -> String) {
        self.projectId = projectId
        self.key = key
        self.roomChat = roomChat
        self.doc = doc
        self.api = api
        self.deviceId = deviceId
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = true
        session = URLSession(configuration: config)
        doc.onLocalUpdate { [weak self] bytes in
            Task { @MainActor in self?.push(bytes) }
        }
    }

    func start() {
        guard loop == nil else { return }
        loop = Task { [weak self] in
            await self?.run()
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
        presence?.cancel()
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
    }

    /// `WAKE{reason}` on the project doc room (§8.4); queued until the socket is up.
    func wake(reason: String) {
        wakeQueue.append(reason)
        flushWake()
    }

    // MARK: Session loop

    private func run() async {
        var backoff = Room.backoffMin
        while !Task.isCancelled {
            let connectedAt = Date()
            do {
                try await runSession()
            } catch {
                if Task.isCancelled { return }
                NSLog("relay room \(projectId)/\(roomChat): \(error.localizedDescription)")
            }
            presence?.cancel()
            socket?.cancel(with: .goingAway, reason: nil)
            socket = nil
            onLink?(.reconnecting)
            if Date().timeIntervalSince(connectedAt) > Room.backoffResetAfter {
                backoff = Room.backoffMin
            }
            try? await Task.sleep(nanoseconds: UInt64(backoff * 1_000_000_000))
            backoff = min(backoff * 2, Room.backoffMax)
        }
    }

    private func runSession() async throws {
        let room = roomName(projectId, roomChat)
        guard let url = api.roomURL(room: room) else {
            throw RelayError(status: 0, message: "bad room URL")
        }
        let ws = session.webSocketTask(with: url)
        ws.maximumMessageSize = rowMax + 8192
        socket = ws
        ws.resume()

        try await send(Frame(.hello, header: [
            "cursor": cursor,
            "device": deviceId(),
            "room": room,
            "proto": protoVersion,
        ]))

        let state = try await withTimeout(Room.helloDeadline) { [self] in
            while true {
                let frame = try await self.receive(ws)
                if frame.type == .state { return frame }
            }
        }
        if let b = state.string("box").flatMap(BoxState.fromWire) {
            onBoxState?(b, nil)
        }
        let checkpointSeq = state.int("checkpointSeq") ?? 0

        // Checkpoint-on-connect (§7.3): fetch when the replica does not contain
        // the checkpoint frontier, or when our cursor is below it.
        if checkpointSeq > 0 && (!doc.contains(frontier: state.payload) || cursor < checkpointSeq) {
            do {
                let (seq, bytes) = try await api.checkpoint(room: room)
                try doc.importUpdate(bytes)
                cursor = max(cursor, max(seq, checkpointSeq))
            } catch {
                NSLog("relay room \(room): checkpoint failed: \(error.localizedDescription)")
            }
        }

        try await send(Frame(.rowsReq, header: ["after": cursor, "excludeOwn": true]))
        for (batchId, bytes) in pending {
            try await send(Frame(.push, header: ["batchId": batchId], payload: bytes))
        }
        flushWake()

        presence?.cancel()
        presence = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Room.presenceEvery * 1_000_000_000))
                guard let self, !Task.isCancelled else { return }
                try? await self.send(Frame(.presence, header: ["device": self.deviceId(), "at": nowMs()]))
            }
        }

        while !Task.isCancelled {
            let frame = try await receive(ws)
            switch frame.type {
            case .row:
                do {
                    try doc.importUpdate(frame.payload)
                } catch {
                    NSLog("relay room \(room): import failed: \(error.localizedDescription)")
                }
                cursor = max(cursor, frame.int("seq") ?? cursor)
            case .rowsDone:
                cursor = max(cursor, frame.int("headSeq") ?? cursor)
                onLink?(.live)
                onCaughtUp?()
            case .ack:
                let batchId = frame.string("batchId") ?? ""
                pending.removeAll { $0.0 == batchId }
                cursor = max(cursor, frame.int("seq") ?? cursor)
                onAcked?()
            case .status:
                if let b = frame.string("box").flatMap(BoxState.fromWire) {
                    onBoxState?(b, frame.string("detail"))
                }
            case .error:
                let code = frame.string("code") ?? ""
                if code == "lagging" {
                    try await send(Frame(.rowsReq, header: ["after": cursor, "excludeOwn": true]))
                } else {
                    throw RelayError(status: 0, message: "relay error \(code): \(frame.string("message") ?? "")")
                }
            case .hello, .state, .rowsReq, .push, .presence, .probe, .probeOk, .wake:
                break
            }
        }
    }

    private func receive(_ ws: URLSessionWebSocketTask) async throws -> Frame {
        while true {
            let message = try await ws.receive()
            switch message {
            case .data(let data):
                if let frame = Frame.decode(data) { return frame }
            case .string(let s):
                if let data = Data(base64Encoded: s), let frame = Frame.decode(data) { return frame }
            @unknown default:
                continue
            }
        }
    }

    private func send(_ frame: Frame) async throws {
        guard let ws = socket else { throw RelayError(status: 0, message: "socket closed") }
        try await ws.send(.data(frame.encode()))
    }

    private func push(_ bytes: Data) {
        if bytes.count > rowMax {
            NSLog("relay room \(roomChat): local update \(bytes.count) bytes exceeds row cap; dropped")
            return
        }
        let batchId = newId()
        pending.append((batchId, bytes))
        let frame = Frame(.push, header: ["batchId": batchId], payload: bytes)
        Task { [weak self] in
            try? await self?.send(frame)
        }
    }

    private func flushWake() {
        guard socket != nil, !wakeQueue.isEmpty else { return }
        let reasons = wakeQueue
        wakeQueue.removeAll()
        Task { [weak self] in
            for reason in reasons {
                try? await self?.send(Frame(.wake, header: ["reason": reason]))
            }
        }
    }
}

struct TimeoutError: Error, LocalizedError {
    var errorDescription: String? { "timed out" }
}

func withTimeout<T>(_ seconds: Double, _ body: @escaping () async throws -> T) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await body() }
        group.addTask {
            try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            throw TimeoutError()
        }
        guard let first = try await group.next() else { throw TimeoutError() }
        group.cancelAll()
        return first
    }
}
