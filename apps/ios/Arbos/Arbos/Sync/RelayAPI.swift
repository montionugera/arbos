// HTTP side of `arbos-relay` (crdt-sync §9): pairing, registry, chats,
// models, wake. One bearer token per call, read from the credentials at
// call time so a re-pair takes effect at once.

import Foundation

struct RelayError: Error, LocalizedError {
    var status: Int
    var message: String

    var errorDescription: String? {
        message.isEmpty ? "HTTP \(status)" : "HTTP \(status): \(message)"
    }

    var isUnauthorized: Bool { status == 401 }
}

/// Accepts `http://`, `https://`, `ws://` or `wss://`; derives the other.
struct RelayEndpoint: Equatable {
    var httpUrl: String
    var wsUrl: String

    init(_ raw: String) {
        var url = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while url.hasSuffix("/") { url.removeLast() }
        if url.isEmpty {
            httpUrl = ""
            wsUrl = ""
        } else if url.hasPrefix("ws://") {
            httpUrl = "http://" + url.dropFirst(5)
            wsUrl = url
        } else if url.hasPrefix("wss://") {
            httpUrl = "https://" + url.dropFirst(6)
            wsUrl = url
        } else if url.hasPrefix("https://") {
            httpUrl = url
            wsUrl = "wss://" + url.dropFirst(8)
        } else if url.hasPrefix("http://") {
            httpUrl = url
            wsUrl = "ws://" + url.dropFirst(7)
        } else {
            httpUrl = "https://" + url
            wsUrl = "wss://" + url
        }
    }
}

struct RegistryProject {
    var project: Project
    var chats: [Chat]
}

final class RelayAPI {
    let endpoint: RelayEndpoint
    var token: () -> String

    private let session: URLSession

    init(endpoint: RelayEndpoint, token: @escaping () -> String) {
        self.endpoint = endpoint
        self.token = token
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 30
        config.waitsForConnectivity = false
        session = URLSession(configuration: config)
    }

    // MARK: Raw calls

    private func request(_ method: String, _ path: String, body: Any? = nil, auth: Bool = true) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: endpoint.httpUrl + path) else {
            throw RelayError(status: 0, message: "Bad relay URL")
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        if auth {
            req.setValue("Bearer \(token())", forHTTPHeaderField: "Authorization")
        }
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw RelayError(status: 0, message: "No HTTP response")
        }
        if http.statusCode >= 400 {
            let text = String(data: data, encoding: .utf8) ?? ""
            throw RelayError(status: http.statusCode, message: text.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return (data, http)
    }

    private func json(_ method: String, _ path: String, body: Any? = nil, auth: Bool = true) async throws -> Any {
        let (data, _) = try await request(method, path, body: body, auth: auth)
        if data.isEmpty { return NSNull() }
        return (try? JSONSerialization.jsonObject(with: data)) ?? NSNull()
    }

    // MARK: Pairing (D35)

    func pair(code: String, name: String) async throws -> DeviceCredentials {
        let v = try await json("POST", "/pair", body: ["code": code.trimmingCharacters(in: .whitespaces), "name": name], auth: false)
        guard let m = v as? [String: Any], let tok = m["device_token"] as? String, !tok.isEmpty else {
            throw RelayError(status: 0, message: "The relay returned no token")
        }
        return DeviceCredentials(
            relayUrl: endpoint.httpUrl,
            deviceId: m["device_id"] as? String ?? "device",
            deviceToken: tok
        )
    }

    // MARK: Registry

    /// `GET /projects` then `GET /projects/{id}` for chats, models and spend.
    func registry() async throws -> [RegistryProject] {
        guard let list = try await json("GET", "/projects") as? [Any] else { return [] }
        var out: [RegistryProject] = []
        for item in list {
            guard let item = item as? [String: Any], let id = item["id"] as? String else { continue }
            let detail = (try await json("GET", "/projects/\(id)")) as? [String: Any] ?? [:]
            let p = detail["project"] as? [String: Any] ?? item
            let models = p["models"] as? [String: Any] ?? [:]
            let talking = models["talking_model"] as? [String: Any] ?? [:]
            let boxField = (detail["box"] as? String) ?? (item["box"] as? String) ?? ""
            let project = Project(
                id: id,
                name: p["name"] as? String ?? "Project",
                icon: p["icon"] as? String ?? "Ω",
                color: p["color"] as? String ?? "#8b7cf6",
                state: BoxState.from(state: p["state"] as? String ?? "", box: boxField),
                stateDetail: nil,
                model: talking["id"] as? String ?? "anthropic/claude-haiku-5.5",
                effort: Effort.from(talking["effort"] as? String),
                spendTodayUsd: (detail["spend_today"] as? NSNumber)?.doubleValue
                    ?? (p["spend_today_usd"] as? NSNumber)?.doubleValue ?? 0,
                spendCapUsd: (p["spend_cap_usd"] as? NSNumber)?.doubleValue
                    ?? (p["spend_cap_daily"] as? NSNumber)?.doubleValue ?? 25
            )
            let chats = ((detail["chats"] as? [Any]) ?? []).compactMap(RelayAPI.parseChat)
            out.append(RegistryProject(project: project, chats: chats))
        }
        return out
    }

    static func parseChat(_ raw: Any) -> Chat? {
        guard let v = raw as? [String: Any], let id = v["id"] as? String, let pid = v["project_id"] as? String else {
            return nil
        }
        return Chat(
            id: id,
            projectId: pid,
            title: v["title"] as? String ?? "New chat",
            isPinned: v["is_pinned"] as? Bool ?? false,
            unread: false,
            lastMessageAt: (v["created_at"] as? NSNumber)?.int64Value ?? 0
        )
    }

    func createChat(projectId: Id) async throws -> Chat {
        let v = try await json("POST", "/projects/\(projectId)/chats", body: ["title": "New chat"])
        guard let m = v as? [String: Any], let chat = RelayAPI.parseChat(m["chat"] ?? NSNull()) else {
            throw RelayError(status: 0, message: "The relay returned no chat")
        }
        return chat
    }

    func closeChat(projectId: Id, chatId: Id) async throws {
        _ = try await request("DELETE", "/projects/\(projectId)/chats/\(chatId)")
    }

    func createProject(name: String) async throws {
        _ = try await json("POST", "/projects", body: ["name": name])
    }

    func setModel(projectId: Id, model: String, effort: Effort) async throws {
        let body: [String: Any] = ["models": ["talking_model": ["id": model, "effort": effort.rawValue]]]
        _ = try await request("PATCH", "/projects/\(projectId)", body: body)
    }

    func wake(projectId: Id) async throws {
        _ = try await json("POST", "/projects/\(projectId)/wake", body: [String: Any]())
    }

    /// Model ids the relay proxy can serve, filtered like the Mac client.
    func models() async throws -> [String] {
        let v = try await json("GET", "/proxy/v1/models")
        let data = (v as? [String: Any])?["data"] as? [Any] ?? []
        let allowed = ["anthropic/", "openai/", "google/", "x-ai/"]
        var ids = data.compactMap { ($0 as? [String: Any])?["id"] as? String }
            .filter { id in !id.contains(":") && allowed.contains { id.hasPrefix($0) } }
        ids.sort()
        var seen = Set<String>()
        ids = ids.filter { seen.insert($0).inserted }
        return ids
    }

    /// Room checkpoint (crdt-sync §7.3). Returns `(seq, bytes)`.
    func checkpoint(room: String) async throws -> (Int64, Data) {
        let (data, http) = try await request("GET", "/room/\(room)/checkpoint")
        let seq = Int64(http.value(forHTTPHeaderField: "x-arbos-checkpoint-seq") ?? "") ?? 0
        return (seq, data)
    }

    /// WebSocket URL for a room; the token is re-read before every dial (§10).
    func roomURL(room: String) -> URL? {
        URL(string: "\(endpoint.wsUrl)/room/\(room)?token=\(token())")
    }
}
