// Chats of one Project: the Mac tab bar as a messenger list (§3.4). The
// pinned first chat carries the Project glyph and cannot be closed.

import SwiftUI

struct ChatListView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let projectId: Id
    @State private var openChat: Chat?
    @State private var creating = false

    private var project: Project? { model.project(projectId) }

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                MachineBanner()
                List {
                    ForEach(model.chats(of: projectId)) { chat in
                        Button { openChat = chat } label: {
                            ChatRow(chat: chat, project: project,
                                    preview: preview(of: chat),
                                    streaming: model.isStreaming(chat.id))
                        }
                        .listRowBackground(theme.bg)
                        .listRowSeparatorTint(theme.border)
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            if !chat.isPinned {
                                Button(role: .destructive) { model.closeChat(chat) } label: {
                                    Label("Close", systemImage: "xmark")
                                }
                            }
                        }
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
            }
        }
        .navigationTitle(project?.name ?? "Project")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    guard !creating else { return }
                    creating = true
                    Task {
                        if let chat = await model.newChat(projectId: projectId) { openChat = chat }
                        creating = false
                    }
                } label: {
                    if creating { ProgressView() } else { Image(systemName: "plus") }
                }
            }
        }
        .navigationDestination(item: $openChat) { chat in
            ChatView(chatId: chat.id, projectId: projectId)
        }
        .onChange(of: model.chats(of: projectId).count, initial: true) { _, _ in
            if openChat == nil, let id = DebugHooks.openChat, let chat = model.chat(id), chat.projectId == projectId {
                openChat = chat
            }
        }
    }

    private func preview(of chat: Chat) -> String {
        let msgs = model.messages(of: chat.id)
        guard let last = msgs.last(where: { !$0.plainText.isEmpty }) else {
            return msgs.isEmpty ? "" : "…"
        }
        let line = last.plainText.split(separator: "\n").first.map(String.init) ?? ""
        return (last.role == .user ? "You: " : "") + line
    }
}

extension Chat: Hashable {
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

struct ChatRow: View {
    @Environment(\.theme) private var theme
    let chat: Chat
    let project: Project?
    let preview: String
    let streaming: Bool

    var body: some View {
        HStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous).fill(theme.surface1)
                if chat.isPinned, let project {
                    Text(project.icon)
                        .font(.system(size: 17, weight: .medium, design: .rounded))
                        .foregroundStyle(theme.text)
                } else {
                    Image(systemName: "bubble.left")
                        .font(.system(size: 15))
                        .foregroundStyle(theme.textMuted)
                }
            }
            .frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(chat.title)
                        .font(.system(size: 15.5, weight: chat.unread ? .semibold : .medium))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    if streaming { PulseDot(color: theme.accent) }
                    if chat.isPinned {
                        Image(systemName: "pin.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(theme.textFaint)
                    }
                }
                if !preview.isEmpty {
                    Text(preview)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.textMuted)
                        .lineLimit(1)
                }
            }
            Spacer()
            if chat.unread {
                Circle().fill(theme.accent).frame(width: 8, height: 8)
            }
        }
        .padding(.vertical, 4)
    }
}

/// One machine-level banner, shown only while the machine wakes or errored;
/// "Reconnecting…" when the relay link is down. Sleeping is silent: the
/// next message wakes the machine.
struct MachineBanner: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        switch model.machine.state {
        case .waking:
            strip {
                ProgressView().controlSize(.small).tint(theme.accent)
                Text("Waking your machine…").foregroundStyle(theme.accent)
                Spacer()
            }
        case .error:
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(theme.danger)
                Text(model.machineDetail.map { "Machine: \($0)" } ?? "Could not wake your machine.")
                    .foregroundStyle(theme.text)
                    .lineLimit(2)
                Spacer()
                Button("Retry") { model.startMachine() }
                    .font(.system(size: 12.5, weight: .semibold))
            }
            .font(.system(size: 12.5))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.dangerSoft))
            .padding(.horizontal, 12)
            .padding(.top, 6)
        case .awake, .hibernatedLight, .hibernatedDeep, .stoppedByUser:
            LinkBanner().padding(.horizontal, 12).padding(.top, 6)
        }
    }

    private func strip<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 8, content: content)
            .font(.system(size: 12))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface1))
            .padding(.horizontal, 12)
            .padding(.top, 6)
    }
}
