// Home, Grok Bot layout: top bar with the account circle (left) and search +
// "+" circles (right), a row of large avatars for the most recent projects,
// then the agent list (avatar, name, time, preview). One machine: its state
// is the small dot on the account circle and on each avatar.

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var showSettings = false
    @State private var showNewProject = false
    @State private var newProjectName = ""
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
                        if model.projects.isEmpty {
                            emptyState
                        } else {
                            if query.isEmpty { favorites }
                            ForEach(projects) { project in
                                Button { model.open(project: project) } label: {
                                    AgentRow(project: project, preview: preview(project), time: timeLabel(project),
                                             streaming: model.isStreaming(project: project.id),
                                             unread: model.chats(of: project.id).contains { $0.unread })
                                }
                                .buttonStyle(.plain)
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
            .alert("New project", isPresented: $showNewProject) {
                TextField("Name", text: $newProjectName)
                Button("Create") {
                    let name = newProjectName.trimmingCharacters(in: .whitespaces)
                    if !name.isEmpty { model.createProject(name: name) }
                    newProjectName = ""
                }
                Button("Cancel", role: .cancel) { newProjectName = "" }
            } message: {
                Text("The relay registers it as a workspace on your machine.")
            }
        }
        .task {
            guard DebugHooks.demo, model.isFixture else { return }
            try? await Task.sleep(nanoseconds: 2_600_000_000)
            if let p = model.project("fx-comms") { model.open(project: p) }
        }
        .onAppear {
            if let id = DebugHooks.openProject, model.path.isEmpty, let p = model.project(id) {
                if let cid = DebugHooks.openChat, let c = model.chat(cid) { model.open(chat: c) } else { model.open(project: p) }
            }
        }
        .onChange(of: model.projects.count) { _, _ in
            if let id = DebugHooks.openProject, model.path.isEmpty, let p = model.project(id) {
                if let cid = DebugHooks.openChat, let c = model.chat(cid) { model.open(chat: c) } else { model.open(project: p) }
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
                Button { showNewProject = true } label: {
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

    private func timeLabel(_ p: Project) -> String {
        let ms = lastActivity(p)
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
