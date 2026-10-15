// Home, Grok Bot layout: top bar with the account circle (left) and search +
// "+" circles (right), a row of large avatars for the most recent projects,
// then the agent list (avatar, name, time, preview). One machine: its state
// is the small dot on the account circle and on each avatar (no status line
// in the list; the machine details live in Settings).

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var showSettings = false
    @State private var showNewChat = false
    @State private var searching = false
    @State private var query = ""
    @FocusState private var searchFocused: Bool

    private var projects: [Project] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        let sorted = model.projects.sorted { lastActivity($0) > lastActivity($1) }
        return q.isEmpty ? sorted : sorted.filter { $0.name.lowercased().contains(q) }
    }

    var body: some View {
        NavigationStack(path: $model.path) {
            ZStack(alignment: .top) {
                theme.bg.ignoresSafeArea()
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        Color.clear.frame(height: 66)
                        if model.projects.isEmpty && model.peopleChats.isEmpty {
                            emptyState
                        } else {
                            if query.isEmpty {
                                ForEach(model.approvals) { device in
                                    ApprovalCard(device: device)
                                }
                                favorites
                            }
                            ForEach(homeRows) { row in
                                switch row {
                                case .project(let project):
                                    Button { model.open(project: project) } label: {
                                        AgentRow(project: project, preview: preview(project), time: timeLabel(project),
                                                 streaming: model.isStreaming(project: project.id),
                                                 unread: model.chats(of: project.id).contains { $0.unread })
                                    }
                                    .buttonStyle(.plain)
                                case .people(let chat):
                                    Button { model.open(peopleChat: chat) } label: {
                                        PeopleRow(chat: chat, preview: peoplePreview(chat), time: timeLabel(ms: peopleActivity(chat)),
                                                  unread: model.peopleUnread.contains(chat.id), me: model.user?.id)
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .mask {
                    VStack(spacing: 0) {
                        LinearGradient(colors: [.clear, .black], startPoint: .top, endPoint: .bottom).frame(height: 100)
                        Color.black
                    }
                    .ignoresSafeArea()
                }
                .refreshable { await model.refreshRegistry() }
                topBar
            }
            .toolbar(.hidden, for: .navigationBar)
            .navigationDestination(for: AppModel.Route.self) { route in
                switch route {
                case .chat(let chatId, let projectId):
                    ChatView(chatId: chatId, projectId: projectId)
                }
            }
            .sheet(isPresented: $showSettings) {
                SettingsView().environmentObject(model).environment(\.theme, theme)
            }
            .sheet(isPresented: $showNewChat) {
                NewChatSheet().environmentObject(model).environment(\.theme, theme)
            }
        }
        .task {
            if DebugHooks.demo, model.isFixture {
                try? await Task.sleep(nanoseconds: 2_600_000_000)
                if let p = model.project("fx-comms") { model.open(project: p) }
            }
            if DebugHooks.liveDemo, let id = DebugHooks.openProject {
                while model.project(id) == nil || model.machine.reported == false { try? await Task.sleep(nanoseconds: 300_000_000) }
                try? await Task.sleep(nanoseconds: 5_500_000_000)
                if let p = model.project(id) { model.open(project: p) }
            }
        }
        .onAppear {
            if !DebugHooks.liveDemo, let id = DebugHooks.openProject, model.path.isEmpty, let p = model.project(id) {
                if let cid = DebugHooks.openChat, let c = model.chat(cid) { model.open(chat: c) } else { model.open(project: p) }
            }
        }
        .onChange(of: model.projects.count) { _, _ in
            if !DebugHooks.liveDemo, let id = DebugHooks.openProject, model.path.isEmpty, let p = model.project(id) {
                if let cid = DebugHooks.openChat, let c = model.chat(cid) { model.open(chat: c) } else { model.open(project: p) }
            }
        }
        .onChange(of: model.peopleChats.count) { _, _ in
            if DebugHooks.openProject == peopleSlot, model.path.isEmpty, let cid = DebugHooks.openChat, let c = model.peopleChat(cid) {
                model.open(peopleChat: c)
            }
        }
    }

    // MARK: Top bar

    private var topBar: some View {
        HStack(spacing: 10) {
            Button { showSettings = true } label: {
                GlassCircle {
                    ZStack(alignment: .bottomTrailing) {
                        Image(systemName: "person.fill")
                            .font(.system(size: 17))
                            .foregroundStyle(theme.text)
                        Circle()
                            .fill(machineDotColor(model.machine.state, theme: theme))
                            .frame(width: 11, height: 11)
                            .overlay(Circle().strokeBorder(theme.bg, lineWidth: 2))
                            .offset(x: 4, y: 4)
                    }
                    .frame(width: 24, height: 24)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Account and machine · \(model.machine.label)")
            if searching {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(theme.icon)
                    TextField("Search", text: $query)
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                        .focused($searchFocused)
                        .submitLabel(.search)
                    Button {
                        query = ""
                        searching = false
                    } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(theme.icon)
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 14)
                .frame(height: Theme.control)
                .background(Capsule().fill(theme.field))
                .overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
                .transition(.opacity)
            } else {
                Spacer()
                Button {
                    withAnimation(.easeOut(duration: 0.18)) { searching = true }
                    searchFocused = true
                } label: {
                    GlassCircle {
                        Image(systemName: "magnifyingglass")
                            .font(.system(size: 17, weight: .medium))
                            .foregroundStyle(theme.text)
                    }
                }
                .buttonStyle(.plain)
                Button { showNewChat = true } label: {
                    GlassCircle {
                        Image(systemName: "plus")
                            .font(.system(size: 19, weight: .regular))
                            .foregroundStyle(theme.text)
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 6)
    }

    // MARK: Favourites row (three most recent)

    private var favorites: some View {
        let top = Array(projects.prefix(3))
        return HStack(alignment: .top, spacing: 0) {
            ForEach(top) { project in
                Button { model.open(project: project) } label: {
                    VStack(spacing: 10) {
                        BlobAvatar(project: project, theme: theme, size: 72,
                                   dot: model.isStreaming(project: project.id) ? theme.warn : nil)
                        Text(project.name)
                            .font(.system(size: Theme.small))
                            .foregroundStyle(theme.muted)
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.vertical, 14)
    }

    // MARK: Empty / error

    private var emptyState: some View {
        VStack(spacing: 10) {
            if let err = model.registryError {
                Text("Could not reach the relay")
                    .font(.system(size: Theme.body, weight: .semibold))
                    .foregroundStyle(theme.text)
                Text(err)
                    .font(.system(size: Theme.small))
                    .foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
                Button("Try again") { Task { await model.refreshRegistry() } }
                    .font(.system(size: Theme.small, weight: .semibold))
                    .foregroundStyle(theme.text)
            } else if model.link == .connecting && !model.isFixture {
                ProgressView().tint(theme.muted)
                Text("Loading your agents…")
                    .font(.system(size: Theme.small))
                    .foregroundStyle(theme.muted)
            } else {
                Text("No agents yet")
                    .font(.system(size: Theme.body, weight: .semibold))
                    .foregroundStyle(theme.text)
                Text("Tap + to create one. The relay sets up its workspace on your machine.")
                    .font(.system(size: Theme.small))
                    .foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 160)
        .padding(.horizontal, 24)
    }

    // MARK: Row data

    enum HomeRow: Identifiable {
        case project(Project)
        case people(PeopleChat)
        var id: String {
            switch self {
            case .project(let p): return "p:" + p.id
            case .people(let c): return "c:" + c.id
            }
        }
        var activity: Int64 { 0 }
    }

    /// Projects and DMs in one list, newest activity first.
    private var homeRows: [HomeRow] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        var rows: [(Int64, HomeRow)] = projects.map { (lastActivity($0), .project($0)) }
        for c in model.peopleChats where q.isEmpty || c.title.lowercased().contains(q) || c.humans.contains { $0.handle.lowercased().contains(q) } {
            rows.append((peopleActivity(c), .people(c)))
        }
        return rows.sorted { $0.0 > $1.0 }.map(\.1)
    }

    private func peopleActivity(_ c: PeopleChat) -> Int64 {
        max(c.lastActivityAt, model.messages(of: c.id).last?.createdAt ?? 0)
    }

    private func peoplePreview(_ c: PeopleChat) -> String {
        guard let m = model.messages(of: c.id).last(where: { !$0.plainText.isEmpty }) else {
            return c.pending ? "Invited · opens when they sign in" : "Say hi"
        }
        let line = m.plainText.split(separator: "\n").first.map(String.init) ?? ""
        let who = m.sender == model.myUserId ? "You" : (m.senderHandle.map { "@\($0)" } ?? "")
        return (who.isEmpty ? "" : who + ": ") + HomeView.plain(line)
    }

    /// Newest message with prose across the project's chats.
    private func latest(_ p: Project) -> Message? {
        var best: Message? = nil
        for chat in model.chats(of: p.id) {
            if let m = model.messages(of: chat.id).last(where: { !$0.plainText.isEmpty }) {
                if best == nil || m.createdAt > best!.createdAt { best = m }
            }
        }
        return best
    }

    private func lastActivity(_ p: Project) -> Int64 {
        if let m = latest(p), m.createdAt > 0 { return m.createdAt }
        return model.chats(of: p.id).map(\.lastMessageAt).max() ?? 0
    }

    private func preview(_ p: Project) -> String {
        guard let m = latest(p) else {
            return model.isStreaming(project: p.id) ? "\(p.name) is working" : "No messages yet"
        }
        let line = m.plainText.split(separator: "\n").first.map(String.init) ?? ""
        return (m.role == .user ? "You: " : "") + HomeView.plain(line)
    }

    /// Strip inline markdown markers for one-line previews.
    static func plain(_ s: String) -> String {
        var t = s
        for mark in ["**", "__", "`", "*", "_", "#"] { t = t.replacingOccurrences(of: mark, with: "") }
        return t.trimmingCharacters(in: .whitespaces)
    }

    private func timeLabel(_ p: Project) -> String { timeLabel(ms: lastActivity(p)) }

    private func timeLabel(ms: Int64) -> String {
        guard ms > 0 else { return "" }
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let cal = Calendar.current
        if cal.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        if cal.isDateInYesterday(date) { return "Yesterday" }
        if let week = cal.date(byAdding: .day, value: -6, to: Date()), date > week {
            return date.formatted(.dateTime.weekday(.wide))
        }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }
}

/// "Approve new device" — another device of this account is waiting (RFC 8628 user code).
struct ApprovalCard: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let device: PendingDevice

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                GlassCircle(size: 36) {
                    Image(systemName: "iphone.and.arrow.right.inward").font(.system(size: 15)).foregroundStyle(theme.text)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text("Approve new device").font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.text)
                    Text("\(device.name) · code \(device.userCode)").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                }
                Spacer()
            }
            HStack(spacing: 10) {
                Button { model.deny(device) } label: {
                    Text("Deny").font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.text)
                        .frame(maxWidth: .infinity).frame(height: 38)
                        .background(Capsule().fill(theme.field)).overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
                }
                .buttonStyle(.plain)
                Button { model.approve(device) } label: {
                    Text("Approve").font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.onPrimary)
                        .frame(maxWidth: .infinity).frame(height: 38)
                        .background(Capsule().fill(theme.primary))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
        .padding(.top, 10)
    }
}

struct AgentRow: View {
    @Environment(\.theme) private var theme
    let project: Project
    let preview: String
    let time: String
    let streaming: Bool
    let unread: Bool

    var body: some View {
        HStack(spacing: 14) {
            BlobAvatar(project: project, theme: theme, size: 46)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(project.name)
                        .font(.system(size: Theme.body, weight: .semibold))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    Spacer()
                    Text(time)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.muted)
                }
                HStack(spacing: 6) {
                    Text(preview)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                    if streaming {
                        PulseDot(color: theme.warn)
                    } else if unread {
                        Circle().fill(Color(hex: "#1084FE")!).frame(width: 8, height: 8)
                    }
                }
            }
        }
        .padding(.vertical, 12)
        .contentShape(Rectangle())
    }
}

/// A DM in the home list: round avatar (vs the agents' blobs), name + @handle.
struct PeopleRow: View {
    @Environment(\.theme) private var theme
    let chat: PeopleChat
    let preview: String
    let time: String
    let unread: Bool
    let me: String?

    private var other: ChatMember? { chat.counterpart(me: me) }

    var body: some View {
        HStack(spacing: 14) {
            ZStack(alignment: .bottomTrailing) {
                PersonAvatar(url: other?.avatarUrl, handle: other?.handle ?? "?", size: 46)
                if let a = chat.agents.first?.project {
                    BlobAvatar(project: a, theme: theme, size: 18)
                        .overlay(Circle().strokeBorder(theme.bg, lineWidth: 2))
                        .offset(x: 3, y: 3)
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(other?.displayName ?? chat.title)
                        .font(.system(size: Theme.body, weight: .semibold))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    Text("@\(other?.handle ?? chat.title)")
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                    Spacer()
                    Text(time).font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                }
                HStack(spacing: 6) {
                    Text(preview)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                    if unread { Circle().fill(Color(hex: "#1084FE")!).frame(width: 8, height: 8) }
                }
            }
        }
        .padding(.vertical, 12)
        .contentShape(Rectangle())
    }
}

struct PulseDot: View {
    let color: Color
    @State private var on = false

    var body: some View {
        Circle()
            .fill(color.opacity(on ? 1 : 0.35))
            .frame(width: 7, height: 7)
            .onAppear {
                withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) { on = true }
            }
    }
}
