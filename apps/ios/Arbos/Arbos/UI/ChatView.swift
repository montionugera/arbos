// One chat: bottom-aligned transcript, banners under the title, the
// composer pill (§3.5–§3.7). An empty chat shows the composer centred with
// a hint; the first send moves it to the bottom for the life of the chat.

import SwiftUI

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let chatId: Id
    let projectId: Id

    @State private var draft = ""
    @State private var showModelPicker = false
    @State private var sentOnce = false
    @FocusState private var composerFocused: Bool
    @Namespace private var composerSlot

    private var project: Project? { model.project(projectId) }
    private var chat: Chat? { model.chat(chatId) }
    private var messages: [Message] { model.messages(of: chatId) }
    @State private var rows: [Row] = []
    private var isEmpty: Bool { messages.isEmpty && !sentOnce }
    private var needsAnswer: Bool { model.chatNeedsAnswer(chatId) }
    private var streaming: Bool { model.isStreaming(chatId) }

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                if let project {
                    BoxBanner(project: project)
                }
                if isEmpty {
                    emptyState
                } else {
                    transcript
                    composer
                        .matchedGeometryEffect(id: "composer", in: composerSlot)
                        .padding(.horizontal, 12)
                        .padding(.bottom, 8)
                        .padding(.top, 6)
                }
            }
            .animation(.easeOut(duration: 0.22), value: isEmpty)
        }
        .navigationTitle(chat?.title ?? "Chat")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    HStack(spacing: 6) {
                        if streaming { PulseDot(color: theme.accent) }
                        Text(chat?.title ?? "Chat")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(theme.text)
                            .lineLimit(1)
                    }
                    if let project {
                        Text(project.name)
                            .font(.system(size: 11))
                            .foregroundStyle(theme.textFaint)
                    }
                }
            }
        }
        .sheet(isPresented: $showModelPicker) {
            if let project {
                ModelPickerView(project: project).environmentObject(model).environment(\.theme, theme)
            }
        }
        .onAppear { model.markRead(chatId: chatId) }
        .onChange(of: messages, initial: true) { _, new in
            rows = Transcript.rows(new)
            model.markRead(chatId: chatId)
        }
        .onChange(of: model.caughtUp.contains(chatId), initial: true) { _, ready in
            if ready, !sentOnce, let text = DebugHooks.autoSend, DebugHooks.openChat == chatId {
                draft = text
                send()
            }
        }
    }

    // MARK: Transcript

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if !model.caughtUp.contains(chatId) && messages.isEmpty {
                        HStack {
                            Spacer()
                            ProgressView().tint(theme.textMuted)
                            Spacer()
                        }
                        .padding(.top, 40)
                    }
                    ForEach(rows) { row in
                        RowView(row: row, chatId: chatId)
                            .id(row.id)
                    }
                    if streaming {
                        WorkingIndicator()
                            .padding(.vertical, 8)
                            .transition(.opacity)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 16)
                .padding(.top, 8)
                .frame(maxWidth: .infinity)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: rows.last?.id) { _, _ in
                if streaming || sentOnce {
                    withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
            }
        }
    }

    // MARK: Empty state (S2)

    private var emptyState: some View {
        VStack(spacing: 14) {
            Spacer()
            Text(project?.name ?? "Arbos")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(theme.text)
            Text("Ask \(project?.name ?? "Arbos") anything. It will plan, delegate, or answer.")
                .font(.system(size: Theme.small))
                .foregroundStyle(theme.textMuted)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            composer
                .matchedGeometryEffect(id: "composer", in: composerSlot)
                .padding(.horizontal, 12)
            Spacer()
            Spacer()
        }
    }

    // MARK: Composer (§3.6)

    private var composer: some View {
        VStack(spacing: 0) {
            if model.link == .reconnecting {
                Text("Reconnecting… messages queue locally and sync when the relay is back.")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.textFaint)
                    .padding(.bottom, 4)
            }
            if let note = project?.modelNote, !note.isEmpty {
                Text(note)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.warn)
                    .padding(.bottom, 4)
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField(needsAnswer ? "Answer the agent…" : "Do anything…", text: $draft, axis: .vertical)
                    .lineLimit(1...10)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.text)
                    .focused($composerFocused)
                    .padding(.vertical, 6)
                    .submitLabel(.return)
                HStack(spacing: 6) {
                    Button { showModelPicker = true } label: {
                        HStack(spacing: 3) {
                            Text(project?.shortModel ?? "model")
                                .lineLimit(1)
                            Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                        }
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(theme.textMuted)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(theme.surface2.opacity(theme.isGlass ? 0.6 : 1)))
                    }
                    .buttonStyle(.plain)
                    Button(action: streaming ? stop : send) {
                        Image(systemName: streaming ? "stop.fill" : "arrow.up")
                            .font(.system(size: 14, weight: .bold))
                            .foregroundStyle(theme.bg)
                            .frame(width: 32, height: 32)
                            .background(Circle().fill(canSend || streaming ? theme.accent : theme.textFaint))
                    }
                    .buttonStyle(.plain)
                    .disabled(!canSend && !streaming)
                }
                .padding(.bottom, 2)
            }
            .padding(.leading, 16)
            .padding(.trailing, 8)
            .padding(.vertical, 6)
            .frame(minHeight: Theme.composerHeight)
            .card(radius: 24, stroke: false)
            .overlay(
                RoundedRectangle(cornerRadius: 24, style: .continuous)
                    .strokeBorder(needsAnswer ? theme.warn : theme.borderStrong, lineWidth: 1)
            )
        }
    }

    private var canSend: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if needsAnswer {
            model.answer(chatId: chatId, text: text)
        } else {
            model.send(chatId: chatId, text: text)
        }
        draft = ""
        sentOnce = true
    }

    private func stop() {
        model.stop(chatId: chatId)
    }
}

/// The one working indicator (Jacob's transcript rule): three soft dots that
/// breathe while any turn in the chat runs. No labels, no step names.
struct WorkingIndicator: View {
    @Environment(\.theme) private var theme
    @State private var phase = 0

    var body: some View {
        HStack(spacing: 5) {
            ForEach(0..<3, id: \.self) { i in
                Circle()
                    .fill(theme.accent.opacity(phase == i ? 0.95 : 0.3))
                    .frame(width: 6, height: 6)
                    .scaleEffect(phase == i ? 1.15 : 0.9)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.leading, 4)
        .onReceive(Timer.publish(every: 0.3, on: .main, in: .common).autoconnect()) { _ in
            withAnimation(.easeInOut(duration: 0.28)) { phase = (phase + 1) % 3 }
        }
        .accessibilityLabel("Arbos is working")
    }
}

/// Talking-model picker: a frosted list of the ids the relay proxy serves,
/// plus the effort level (Low / Medium / High → pi thinking levels).
struct ModelPickerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var query = ""
    @State private var effort: Effort = .low

    private var ids: [String] {
        var all = model.models
        if !all.contains(project.model) { all.insert(project.model, at: 0) }
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        return q.isEmpty ? all : all.filter { $0.lowercased().contains(q) }
    }

    var body: some View {
        NavigationStack {
            List {
                Section("Reasoning") {
                    Picker("Effort", selection: $effort) {
                        ForEach(Effort.allCases) { e in Text(e.label).tag(e) }
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: effort) { _, new in
                        model.setModel(projectId: project.id, model: project.model, effort: new)
                    }
                }
                Section("Talking model") {
                    if model.models.isEmpty {
                        Text("Loading models from the relay…")
                            .foregroundStyle(theme.textMuted)
                    }
                    ForEach(ids, id: \.self) { id in
                        Button {
                            model.setModel(projectId: project.id, model: id, effort: effort)
                            dismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Project.shortModel(id)).foregroundStyle(theme.text)
                                    Text(id).font(.system(size: 11)).foregroundStyle(theme.textFaint)
                                }
                                Spacer()
                                if id == project.model {
                                    Image(systemName: "checkmark").foregroundStyle(theme.accent)
                                }
                            }
                        }
                    }
                }
                if !project.modelNote.isEmpty {
                    Section {
                        Text(project.modelNote).font(.system(size: 12.5)).foregroundStyle(theme.warn)
                    }
                }
            }
            .searchable(text: $query, prompt: "Filter models")
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
            .onAppear { effort = project.effort }
        }
        .presentationDetents([.medium, .large])
    }
}
