import Foundation

struct PhoneProject: Decodable, Identifiable, Equatable {
    var id: String { path }
    let name: String
    let path: String
    let home: String
    let mainChatId: String
}

private enum PhoneError: Error {
    case closed
    case rejected
    case unreadable
}

@MainActor
final class PhoneSession: ObservableObject {
    @Published var projects: [PhoneProject] = []
    @Published var lines: [String] = []
    @Published var status = "Not connected"
    @Published var following: String?

    private var task: URLSessionWebSocketTask?
    private var listenTask: Task<Void, Never>?
    private var nextId = 1
    private var calls: [Int: CheckedContinuation<Data, Error>] = []
    private var watchId: Int?
    private var project: PhoneProject?

    func connect(url: String, accessId: String, accessSecret: String) {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let endpoint = URL(string: trimmed),
              endpoint.scheme == "ws" || endpoint.scheme == "wss" else {
            status = "The engine URL must start with ws:// or wss://"
            return
        }
        listenTask?.cancel()
        task?.cancel(with: .goingAway, reason: nil)
        failCalls()
        projects = []
        lines = []
        following = nil
        project = nil
        watchId = nil
        status = "Connecting"

        var request = URLRequest(url: endpoint)
        let clientId = accessId.trimmingCharacters(in: .whitespacesAndNewlines)
        let secret = accessSecret.trimmingCharacters(in: .whitespacesAndNewlines)
        if !clientId.isEmpty {
            request.setValue(clientId, forHTTPHeaderField: "CF-Access-Client-Id")
        }
        if !secret.isEmpty {
            request.setValue(secret, forHTTPHeaderField: "CF-Access-Client-Secret")
        }
        let socket = URLSession(configuration: .default).webSocketTask(with: request)
        task = socket
        socket.resume()
        listen()
        Task {
            do {
                let payload = try await call(method: "ArbosCall", params: ["op": "projects"])
                let rows = (payload["projects"] as? [Any]) ?? []
                let data = try JSONSerialization.data(withJSONObject: rows)
                projects = try JSONDecoder().decode([PhoneProject].self, from: data)
                status = projects.isEmpty ? "No projects" : "Connected"
            } catch {
                status = "The project list was not returned"
            }
        }
    }

    func follow(_ project: PhoneProject) {
        self.project = project
        following = project.name
        lines = []
        status = "Following \(project.name)"
        Task {
            do {
                watchId = try await subscribe(
                    method: "WatchDocMessages",
                    params: ["chatId": project.mainChatId]
                )
            } catch {
                status = "Main could not be followed"
            }
        }
    }

    func send(_ prompt: String) {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let project, !text.isEmpty else { return }
        let messageId = UUID().uuidString
        let params: [String: Any] = [
            "chatId": project.mainChatId,
            "command": [
                "kind": "run",
                "messageId": messageId,
                "request": [
                    "prompt": text,
                    "harness": "arbos-fx",
                    "model": "glm-5.3",
                    "reasoning": NSNull(),
                    "cwd": project.path,
                    "sandbox": "danger-full-access",
                    "autoApprove": true,
                    "resume": NSNull(),
                ],
            ],
        ]
        Task {
            do {
                _ = try await call(method: "QueueCommand", params: params)
                status = "Prompt queued"
            } catch {
                status = "The prompt was not queued"
            }
        }
    }

    private func listen() {
        let socket = task
        listenTask = Task {
            while !Task.isCancelled, let socket {
                do {
                    let message = try await socket.receive()
                    await MainActor.run { self.take(message) }
                } catch {
                    await MainActor.run {
                        if self.status == "Connecting" || self.status.hasPrefix("Following") {
                            self.status = "The engine connection closed"
                        }
                    }
                    return
                }
            }
        }
    }

    private func take(_ message: URLSessionWebSocketTask.Message) {
        let data: Data
        switch message {
        case .string(let text):
            data = Data(text.utf8)
        case .data(let raw):
            data = raw
        @unknown default:
            return
        }
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = intValue(json["id"]) else {
            return
        }
        if let ok = json["ok"] {
            let payload = (try? JSONSerialization.data(withJSONObject: ok)) ?? Data("{}".utf8)
            calls.removeValue(forKey: id)?.resume(returning: payload)
            return
        }
        if json["err"] != nil {
            calls.removeValue(forKey: id)?.resume(throwing: PhoneError.rejected)
            return
        }
        if id == watchId, let item = json["item"] {
            appendLines(item)
        }
    }

    private func call(method: String, params: [String: Any]) async throws -> [String: Any] {
        let id = nextId
        nextId += 1
        try await sendFrame(["id": id, "method": method, "params": params])
        let data: Data = try await withCheckedThrowingContinuation { continuation in
            calls[id] = continuation
        }
        let object = try JSONSerialization.jsonObject(with: data)
        return object as? [String: Any] ?? [:]
    }

    private func subscribe(method: String, params: [String: Any]) async throws -> Int {
        let id = nextId
        nextId += 1
        try await sendFrame(["id": id, "method": method, "params": params])
        return id
    }

    private func sendFrame(_ object: [String: Any]) async throws {
        guard let socket = task else { throw PhoneError.closed }
        let data = try JSONSerialization.data(withJSONObject: object)
        guard let text = String(data: data, encoding: .utf8) else { throw PhoneError.unreadable }
        try await socket.send(.string(text))
    }

    private func appendLines(_ value: Any) {
        var found: [String] = []
        collectText(value, into: &found)
        if found.isEmpty {
            lines.append("Main")
            return
        }
        lines.append(contentsOf: found)
    }

    private func collectText(_ value: Any, into lines: inout [String]) {
        if let object = value as? [String: Any] {
            if let text = object["text"] as? String {
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty {
                    lines.append(trimmed)
                }
            }
            for child in object.values {
                collectText(child, into: &lines)
            }
            return
        }
        if let list = value as? [Any] {
            for child in list {
                collectText(child, into: &lines)
            }
        }
    }

    private func intValue(_ value: Any?) -> Int? {
        if let number = value as? Int {
            return number
        }
        if let number = value as? NSNumber {
            return number.intValue
        }
        return nil
    }

    private func failCalls() {
        let pending = calls
        calls = [:]
        for (_, continuation) in pending {
            continuation.resume(throwing: PhoneError.closed)
        }
    }
}
