// App state: the relay registry, one Loro replica per open room, and the
// view of them the screens read. Everything runs on the main actor; Loro
// frames are ~130 bytes every 120 ms, so there is nothing to move off it.

import Foundation
import SwiftUI
import UIKit

enum Appearance: String, CaseIterable, Identifiable {
    case system, dark, light
    var id: String { rawValue }
    var label: String {
        switch self {
        case .system: return "System"
        case .dark: return "Dark"
        case .light: return "Light"
        }
    }
}

enum Surface: String, CaseIterable, Identifiable {
    case matte, glass
    var id: String { rawValue }
    var label: String {
        switch self {
        case .matte: return "Matte"
        case .glass: return "Glass"
        }
    }
}

struct Notice: Identifiable, Equatable {
    let id = UUID()
    var text: String
}

@MainActor
final class AppModel: ObservableObject {
    static let defaultRelayUrl = ""

    @Published var appearanceSetting: String = UserDefaults.standard.string(forKey: "appearance") ?? Appearance.system.rawValue {
        didSet { UserDefaults.standard.set(appearanceSetting, forKey: "appearance") }
    }
    @Published var surfaceSetting: String = UserDefaults.standard.string(forKey: "surface") ?? Surface.matte.rawValue {
        didSet { UserDefaults.standard.set(surfaceSetting, forKey: "surface") }
    }

    @Published private(set) var credentials: DeviceCredentials?
    @Published var projects: [Project] = []
    @Published var chats: [Id: [Chat]] = [:]
    @Published var messages: [Id: [Message]] = [:]
    @Published var caughtUp: Set<Id> = []
    @Published var link: Link = .connecting
    @Published private(set) var models: [String] = []
    @Published var machine: Machine = .unknown
    @Published private(set) var machineBusy = false
    /// Navigation: home → chat. Shared so menus anywhere can open a chat.
    @Published var path: [Route] = []
    /// True when the Debug fixture replaced the relay (screenshots).
    var isFixture = false

    enum Route: Hashable {
        case chat(chatId: Id, projectId: Id)
    }

    func open(chat: Chat) {
        path = [.chat(chatId: chat.id, projectId: chat.projectId)]
    }

    func open(project: Project) {
        if let chat = mainChat(of: project.id) {
            open(chat: chat)
        } else {
            Task { if let c = await newChat(projectId: project.id) { open(chat: c) } }
        }
    }
    /// `status.box.since` per project doc; the newest one is the machine's.
    private var machineSince: [Id: Int64] = [:]
    @Published var notice: Notice?
    @Published private(set) var pairingBusy = false
    @Published private(set) var pairingError: String?
    @Published private(set) var registryError: String?

    private var api: RelayAPI
    private var docs: [Id: ChatDoc] = [:]
    private var rooms: [Id: Room] = [:]
    /// Client-minted message ids whose push the relay has not `ACK`ed (S3).
    private var unacked: Set<Id> = []
    private var registryTask: Task<Void, Never>?

    var appearance: Appearance { Appearance(rawValue: appearanceSetting) ?? .system }
    var surface: Surface { Surface(rawValue: surfaceSetting) ?? .matte }
    var isPaired: Bool { credentials != nil }
    var relayUrl: String { api.endpoint.httpUrl }

    init() {
        var saved = UserDefaults.standard.string(forKey: "relayUrl") ?? AppModel.defaultRelayUrl
        var creds: DeviceCredentials?
        #if DEBUG
        // Simulator runs: `SIMCTL_CHILD_ARBOS_RELAY_URL` / `_TOKEN` skip the pairing screen.
        let env = ProcessInfo.processInfo.environment
        if let url = env["ARBOS_RELAY_URL"], !url.isEmpty { saved = url }
        if let tok = env["ARBOS_RELAY_TOKEN"], !tok.isEmpty {
            creds = DeviceCredentials(relayUrl: RelayEndpoint(saved).httpUrl, deviceId: env["ARBOS_DEVICE_ID"] ?? "ios:env", deviceToken: tok)
        }
        #endif
        #if DEBUG
        if env["ARBOS_FIXTURE"] == "1" {
            api = RelayAPI(endpoint: RelayEndpoint("http://fixture.invalid")) { "" }
            credentials = DeviceCredentials(relayUrl: "http://fixture.invalid", deviceId: "ios:fixture", deviceToken: "fixture")
            loadFixture()
            return
        }
        #endif
        let endpoint = RelayEndpoint(saved)
        if !endpoint.httpUrl.isEmpty {
            UserDefaults.standard.set(endpoint.httpUrl, forKey: "relayUrl")
        }
        api = RelayAPI(endpoint: endpoint) { "" }
        if creds == nil { creds = Keychain.load(relayUrl: endpoint.httpUrl) }
        credentials = creds
        api.token = { [weak self] in self?.credentials?.deviceToken ?? "" }
        if creds != nil {
            startAfterPairing()
        }
    }

    var deviceId: String { credentials?.deviceId ?? "ios:unpaired" }

    // MARK: Pairing

    func setRelayUrl(_ raw: String) {
        let endpoint = RelayEndpoint(raw)
        UserDefaults.standard.set(endpoint.httpUrl, forKey: "relayUrl")
        guard endpoint != api.endpoint else { return }
        unpair(forget: false)
        api = RelayAPI(endpoint: endpoint) { [weak self] in self?.credentials?.deviceToken ?? "" }
        credentials = Keychain.load(relayUrl: endpoint.httpUrl)
        if credentials != nil {
            startAfterPairing()
        }
    }

    func pair(code: String) {
        guard !pairingBusy else { return }
        pairingBusy = true
        pairingError = nil
        Task {
            defer { pairingBusy = false }
            do {
                let creds = try await api.pair(code: code, name: UIDevice.current.name)
                do {
                    try Keychain.store(creds)
                } catch {
                    notice = Notice(text: "Token kept in memory only: \(error.localizedDescription)")
                }
                credentials = creds
                startAfterPairing()
            } catch {
                pairingError = (error as? RelayError)?.isUnauthorized == true
                    ? "That code was not accepted. Codes expire after 10 minutes."
                    : error.localizedDescription
            }
        }
    }

    func unpair(forget: Bool = true) {
        registryTask?.cancel()
        registryTask = nil
        for room in rooms.values { room.stop() }
        rooms.removeAll()
        docs.removeAll()
        projects = []
        chats = [:]
        messages = [:]
        caughtUp = []
        unacked = []
        if forget, let creds = credentials {
            Keychain.delete(relayUrl: creds.relayUrl)
        }
        credentials = nil
        link = .connecting
    }

    private func startAfterPairing() {
        link = .connecting
        Task {
            if let ids = try? await api.models(), !ids.isEmpty {
                models = ids
            }
        }
        registryTask?.cancel()
        registryTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshRegistry()
                try? await Task.sleep(nanoseconds: 15_000_000_000)
            }
        }
    }

    // MARK: Registry

    func refreshRegistry() async {
        do {
            let items = try await api.registry()
            registryError = nil
            var newProjects: [Project] = []
            var newChats: [Id: [Chat]] = [:]
            for item in items {
                var p = item.project
                if let prev = projects.first(where: { $0.id == p.id }) {
                    p.modelNote = prev.modelNote
                    if p.state == prev.state { p.stateDetail = prev.stateDetail }
                }
                newProjects.append(p)
                var mine = item.chats
                let prevChats = chats[p.id] ?? []
                for i in mine.indices {
                    if let prev = prevChats.first(where: { $0.id == mine[i].id }) {
                        mine[i].unread = prev.unread
                        mine[i].lastMessageAt = max(mine[i].lastMessageAt, prev.lastMessageAt)
                        if mine[i].title == "New chat" && prev.title != "New chat" {
                            mine[i].title = prev.title
                        }
                    }
                }
                newChats[p.id] = AppModel.sorted(mine)
            }
            for i in newProjects.indices {
                if let prev = projects.first(where: { $0.id == newProjects[i].id }) {
                    newProjects[i].workspace = prev.workspace
                }
            }
            projects = newProjects.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            chats = newChats
            recomputeMachine()
            for p in newProjects {
                ensureRoom(projectId: p.id, key: projectDocKey(p.id), roomChat: projectDocChatId)
            }
            for list in newChats.values {
                for c in list {
                    ensureRoom(projectId: c.projectId, key: c.id, roomChat: c.id)
                }
            }
        } catch {
            if (error as? RelayError)?.isUnauthorized == true {
                // Token revoked or relay re-initialised: pair again.
                unpair(forget: true)
                pairingError = "This device was signed out by the relay. Pair again."
                return
            }
            registryError = error.localizedDescription
            link = .reconnecting
        }
    }

    static func sorted(_ chats: [Chat]) -> [Chat] {
        chats.sorted { a, b in
            if a.isPinned != b.isPinned { return a.isPinned }
            return a.lastMessageAt > b.lastMessageAt
        }
    }

    private func projectDocKey(_ projectId: Id) -> String { "proj:\(projectId)" }

    // MARK: Rooms

    private func ensureRoom(projectId: Id, key: Id, roomChat: String) {
        guard rooms[key] == nil else { return }
        let doc = ChatDoc(key: key)
        let isProjectDoc = roomChat == projectDocChatId
        docs[key] = doc
        doc.onChange { [weak self] touched in
            Task { @MainActor in
                guard let self else { return }
                if isProjectDoc {
                    self.applyProjectDoc(projectId)
                } else if touched.listChanged {
                    self.emitSnapshot(key)
                    self.markActivity(chatId: key)
                } else if !touched.messages.isEmpty {
                    self.emitMessages(key, indexes: touched.messages)
                }
            }
        }
        let room = Room(projectId: projectId, key: key, roomChat: roomChat, doc: doc, api: api) { [weak self] in
            self?.deviceId ?? "ios:unpaired"
        }
        room.onLink = { [weak self] l in self?.link = l }
        room.onBoxState = { [weak self] state, detail in
            self?.setProjectState(projectId, state, detail: detail)
        }
        room.onCaughtUp = { [weak self] in
            guard let self else { return }
            if isProjectDoc {
                self.applyProjectDoc(projectId)
            } else {
                self.emitSnapshot(key)
                self.caughtUp.insert(key)
            }
        }
        room.onAcked = { [weak self] in
            guard let self, !self.unacked.isEmpty else { return }
            self.unacked.removeAll()
            if !isProjectDoc { self.emitSnapshot(key) }
        }
        rooms[key] = room
        room.start()
    }

    private func emitSnapshot(_ chatId: Id) {
        guard let doc = docs[chatId] else { return }
        var list = doc.readAll()
        for i in list.indices where list[i].role == .user {
            list[i].pending = unacked.contains(list[i].id)
        }
        messages[chatId] = list
    }

    private func emitMessages(_ chatId: Id, indexes: Set<Int>) {
        guard let doc = docs[chatId] else { return }
        var list = messages[chatId] ?? []
        var needSnapshot = false
        for ix in indexes.sorted() {
            guard var m = doc.readMessage(ix) else { continue }
            if m.role == .user { m.pending = unacked.contains(m.id) }
            if ix < list.count, list[ix].id == m.id {
                list[ix] = m
            } else if ix == list.count {
                list.append(m)
            } else {
                needSnapshot = true
            }
        }
        if needSnapshot {
            emitSnapshot(chatId)
        } else {
            messages[chatId] = list
        }
    }

    private func markActivity(chatId: Id) {
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }) {
                var updated = list
                updated[i].lastMessageAt = nowMs()
                chats[pid] = AppModel.sorted(updated)
                return
            }
        }
    }

    /// Project doc (crdt-sync §6.3): `settings.modelNote`, `status.workspace
    /// {state}` and `status.box {state, since}` (the machine mirror). The
    /// machine *state* still comes from the relay (registry + STATUS frames):
    /// a dead box cannot rewrite its own stale `awake`. The doc gives `since`.
    private func applyProjectDoc(_ projectId: Id) {
        guard let doc = docs[projectDocKey(projectId)] else { return }
        let settings = doc.mapValue("settings")
        let note = settings["modelNote"] as? String ?? ""
        let status = doc.mapValue("status")
        let workspace = WorkspaceState.from((status["workspace"] as? [String: Any])?["state"] as? String)
        if let box = status["box"] as? [String: Any], let since = DocJSON.int(box["since"]) {
            machineSince[projectId] = since
        } else if let since = DocJSON.int(status["since"]) {
            machineSince[projectId] = since
        }
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            if projects[i].modelNote != note { projects[i].modelNote = note }
            if projects[i].workspace != workspace { projects[i].workspace = workspace }
        }
        recomputeMachine()
    }

    private func setProjectState(_ projectId: Id, _ state: BoxState, detail: String?) {
        guard let i = projects.firstIndex(where: { $0.id == projectId }) else { return }
        if projects[i].state == state && projects[i].stateDetail == detail { return }
        if projects[i].state != state { projects[i].stateSince = nowMs() }
        projects[i].state = state
        projects[i].stateDetail = detail
        recomputeMachine()
    }

    /// One machine per user: fold the projects' box states until the relay
    /// reports the machine itself.
    private func recomputeMachine() {
        let state = Machine.aggregate(projects)
        let since = machineSince.values.max()
        let spec = projects.isEmpty ? "" : "Daytona · \(projects.count) workspace\(projects.count == 1 ? "" : "s")"
        let next = Machine(state: state, since: since, spec: spec)
        if next != machine { machine = next }
    }

    /// Machine-level error text (the first project that reports one).
    var machineDetail: String? {
        projects.first { $0.state == .error }?.stateDetail
    }

    // MARK: Machine control (per-project endpoints until the relay has a machine API)

    func startMachine() {
        guard !machineBusy else { return }
        machineBusy = true
        let ids = projects.filter { $0.state.isAsleep || $0.state == .error }.map(\.id)
        for id in ids { setProjectState(id, .waking, detail: nil) }
        Task {
            defer { machineBusy = false }
            for id in ids {
                do { try await api.start(projectId: id) } catch {
                    notice = Notice(text: "Start failed: \(error.localizedDescription)")
                    setProjectState(id, .error, detail: error.localizedDescription)
                }
            }
            await refreshRegistry()
        }
    }

    func stopMachine() {
        guard !machineBusy else { return }
        machineBusy = true
        let ids = projects.filter { $0.state == .awake || $0.state == .waking }.map(\.id)
        Task {
            defer { machineBusy = false }
            for id in ids {
                do { try await api.stop(projectId: id) } catch {
                    notice = Notice(text: "Stop failed: \(error.localizedDescription)")
                }
            }
            for id in ids { setProjectState(id, .stoppedByUser, detail: nil) }
            await refreshRegistry()
        }
    }

    // MARK: Lookups

    /// Relay blob URL with the device token as a query parameter (the relay
    /// accepts `?token=` on every route), so AVPlayer and Safari can open it.
    func blobURL(projectId: Id, key: String) -> URL? {
        let base = api.endpoint.httpUrl
        guard !base.isEmpty else { return nil }
        return URL(string: "\(base)/blob/\(projectId)/\(key)?token=\(credentials?.deviceToken ?? "")")
    }

    var authHeaders: [String: String] {
        ["Authorization": "Bearer \(credentials?.deviceToken ?? "")"]
    }

    /// Pinned ("main") chat of a project, the one the agent list opens.
    func mainChat(of projectId: Id) -> Chat? {
        chats(of: projectId).first { $0.isPinned } ?? chats(of: projectId).first
    }

    func project(_ id: Id) -> Project? { projects.first { $0.id == id } }

    func chat(_ id: Id) -> Chat? {
        for list in chats.values {
            if let c = list.first(where: { $0.id == id }) { return c }
        }
        return nil
    }

    func chats(of projectId: Id) -> [Chat] { chats[projectId] ?? [] }

    func messages(of chatId: Id) -> [Message] { messages[chatId] ?? [] }

    /// S8: the last assistant message asked a question and nothing was sent since.
    func chatNeedsAnswer(_ chatId: Id) -> Bool {
        guard let last = messages(of: chatId).last, last.role == .assistant, !last.isStreaming else { return false }
        return last.askUserText != nil
    }

    /// A turn is running: the last assistant message streams, or any recent
    /// one does. A message left `streaming` by a box that died hours ago
    /// must not pin the indicator forever.
    func isStreaming(_ chatId: Id) -> Bool {
        let list = messages(of: chatId)
        if let last = list.last(where: { $0.role == .assistant }), last.isStreaming { return true }
        let recent = nowMs() - 15 * 60 * 1000
        return list.contains { $0.role == .assistant && $0.isStreaming && $0.createdAt >= recent }
    }

    func isStreaming(project projectId: Id) -> Bool {
        chats(of: projectId).contains { isStreaming($0.id) }
    }

    // MARK: Commands (crdt-sync §6.2: the client writes rows and commands)

    /// An attachment staged in the composer, uploaded on send.
    struct PendingAttachment: Identifiable, Equatable {
        let id = UUID()
        var name: String
        var mime: String
        var data: Data
    }

    func send(chatId: Id, text: String, attachments: [PendingAttachment] = []) {
        if attachments.isEmpty {
            write(chatId: chatId, text: text, command: "send", attachments: [])
            return
        }
        guard let chat = chat(chatId) else { return }
        if isFixture {
            write(chatId: chatId, text: text, command: "send", attachments: attachments.map { Attachment(name: $0.name, mime: $0.mime, blobRef: nil) })
            return
        }
        Task {
            var uploaded: [Attachment] = []
            for a in attachments {
                let key = "att-\(newId().lowercased())-\(AppModel.safeName(a.name))"
                do {
                    _ = try await api.putBlob(projectId: chat.projectId, key: key, mime: a.mime, data: a.data)
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: key))
                } catch {
                    notice = Notice(text: "Attachment upload failed: \(error.localizedDescription)")
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: nil))
                }
            }
            write(chatId: chatId, text: text, command: "send", attachments: uploaded)
        }
    }

    static func safeName(_ name: String) -> String {
        String(name.map { $0.isLetter || $0.isNumber || $0 == "." || $0 == "-" || $0 == "_" ? $0 : "_" })
    }

    func answer(chatId: Id, text: String) {
        write(chatId: chatId, text: text, command: "answer", attachments: [])
    }

    private func write(chatId: Id, text: String, command: String, attachments: [Attachment]) {
        guard let doc = docs[chatId], let chat = chat(chatId) else {
            #if DEBUG
            if isFixture { fixtureAppend(chatId: chatId, text: text, attachments: attachments) }
            #endif
            return
        }
        let messageId = newId()
        unacked.insert(messageId)
        do {
            try doc.insertUserMessage(id: messageId, text: text, deviceId: deviceId, attachments: attachments)
            try doc.pushCommand(command, payload: ["messageId": messageId], device: deviceId)
            if !chat.isPinned && chat.title == "New chat" {
                let title = AppModel.title(from: text)
                try doc.pushCommand("retitle", payload: ["title": title], device: deviceId)
                retitleLocally(chatId: chatId, title: title)
            }
        } catch {
            notice = Notice(text: "Could not write the message: \(error.localizedDescription)")
            return
        }
        doc.commit()
        markActivity(chatId: chatId)
        if let p = project(chat.projectId), p.state.isAsleep || p.state == .error {
            wake(projectId: p.id, reason: "send")
        }
    }

    func stop(chatId: Id) {
        guard let doc = docs[chatId] else { return }
        try? doc.pushCommand("stop", payload: [:], device: deviceId)
        doc.commit()
    }

    func retry(chatId: Id, messageId: Id) {
        guard let doc = docs[chatId] else { return }
        try? doc.pushCommand("retry", payload: ["messageId": messageId], device: deviceId)
        doc.commit()
    }

    private func retitleLocally(chatId: Id, title: String) {
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }) {
                var updated = list
                updated[i].title = title
                chats[pid] = updated
                return
            }
        }
    }

    static func title(from text: String) -> String {
        let line = text.split(separator: "\n", omittingEmptySubsequences: true).first.map(String.init) ?? ""
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return "New chat" }
        if trimmed.count > 34 { return String(trimmed.prefix(34)) + "…" }
        return trimmed
    }

    func wake(projectId: Id, reason: String) {
        rooms[projectDocKey(projectId)]?.wake(reason: reason)
        setProjectState(projectId, .waking, detail: "Waking sandbox…")
        Task {
            do {
                try await api.wake(projectId: projectId)
            } catch {
                notice = Notice(text: "Wake failed: \(error.localizedDescription)")
                setProjectState(projectId, .error, detail: error.localizedDescription)
            }
        }
    }

    /// Returns the new chat once the relay has created it.
    func newChat(projectId: Id) async -> Chat? {
        do {
            let chat = try await api.createChat(projectId: projectId)
            var list = chats[projectId] ?? []
            list.append(chat)
            chats[projectId] = AppModel.sorted(list)
            ensureRoom(projectId: projectId, key: chat.id, roomChat: chat.id)
            return chat
        } catch {
            notice = Notice(text: "New chat failed: \(error.localizedDescription)")
            return nil
        }
    }

    func closeChat(_ chat: Chat) {
        guard !chat.isPinned else { return }
        chats[chat.projectId] = (chats[chat.projectId] ?? []).filter { $0.id != chat.id }
        rooms[chat.id]?.stop()
        rooms[chat.id] = nil
        docs[chat.id] = nil
        messages[chat.id] = nil
        Task {
            do {
                try await api.closeChat(projectId: chat.projectId, chatId: chat.id)
            } catch {
                if (error as? RelayError)?.status == 409 {
                    notice = Notice(text: "The pinned chat cannot be closed")
                } else if (error as? RelayError)?.status != 404 {
                    notice = Notice(text: "Close chat: \(error.localizedDescription)")
                }
                await refreshRegistry()
            }
        }
    }

    func markRead(chatId: Id) {
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }), list[i].unread {
                var updated = list
                updated[i].unread = false
                chats[pid] = updated
                return
            }
        }
    }

    func setModel(projectId: Id, model: String, effort: Effort) {
        if let doc = docs[projectDocKey(projectId)] {
            try? doc.setModelSettings(model: model, effort: effort)
            doc.commit()
        }
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            projects[i].model = model
            projects[i].effort = effort
        }
        Task {
            do {
                try await api.setModel(projectId: projectId, model: model, effort: effort)
            } catch {
                notice = Notice(text: "Model change not saved: \(error.localizedDescription)")
            }
        }
    }

    func createProject(name: String) {
        Task {
            do {
                try await api.createProject(name: name)
                await refreshRegistry()
            } catch {
                notice = Notice(text: "New project failed: \(error.localizedDescription)")
            }
        }
    }
}
