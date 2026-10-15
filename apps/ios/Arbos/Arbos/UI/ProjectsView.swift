// Projects list: the phone's version of the Mac rail (§3.3). One tile per
// Project with its glyph, name, box state and the talking model.

import SwiftUI

struct ProjectsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var showSettings = false
    @State private var newProjectName = ""
    @State private var showNewProject = false
    @State private var path: [Id] = DebugHooks.openProject.map { [$0] } ?? []

    var body: some View {
        NavigationStack(path: $path) {
            ZStack {
                theme.bg.ignoresSafeArea()
                content
            }
            .navigationTitle("Projects")
            .navigationBarTitleDisplayMode(.large)
            .toolbarBackground(theme.bg, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showSettings = true } label: {
                        Image(systemName: "gearshape")
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showNewProject = true } label: {
                        Image(systemName: "plus")
                    }
                }
            }
            .navigationDestination(for: Id.self) { projectId in
                ChatListView(projectId: projectId)
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
                Text("The relay registers it and creates its sandbox from the image.")
            }
            .refreshable { await model.refreshRegistry() }
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.projects.isEmpty {
            VStack(spacing: 10) {
                if let err = model.registryError {
                    Text("Could not reach the relay")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(theme.text)
                    Text(err)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.textMuted)
                        .multilineTextAlignment(.center)
                    Button("Try again") { Task { await model.refreshRegistry() } }
                        .font(.system(size: Theme.small, weight: .semibold))
                } else if model.link == .connecting {
                    ProgressView().tint(theme.textMuted)
                    Text("Loading projects…")
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.textMuted)
                } else {
                    Text("No projects yet")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(theme.text)
                    Text("Tap + to create one. The relay boots a sandbox for it.")
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.textMuted)
                }
            }
            .padding(28)
        } else {
            ScrollView {
                LazyVStack(spacing: 8) {
                    LinkBanner()
                    ForEach(model.projects) { project in
                        NavigationLink(value: project.id) {
                            ProjectRow(project: project, streaming: model.isStreaming(project: project.id),
                                       unread: model.chats(of: project.id).contains { $0.unread })
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
        }
    }
}

struct ProjectRow: View {
    @Environment(\.theme) private var theme
    let project: Project
    let streaming: Bool
    let unread: Bool

    var body: some View {
        HStack(spacing: 14) {
            ProjectTile(project: project, size: 52)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(project.name)
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    if streaming {
                        PulseDot(color: theme.accent)
                    } else if unread {
                        Circle().fill(theme.accent).frame(width: 7, height: 7)
                    }
                }
                HStack(spacing: 6) {
                    Text(stateLabel)
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(stateColor)
                    Text("·").foregroundStyle(theme.textFaint)
                    Text(project.shortModel)
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.textFaint)
                        .lineLimit(1)
                }
            }
            Spacer()
            Image(systemName: "chevron.right")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(theme.textFaint)
        }
        .padding(12)
        .card(radius: 14)
    }

    private var stateLabel: String {
        switch project.state {
        case .awake: return "Awake"
        case .waking: return "Waking…"
        case .hibernatedLight, .hibernatedDeep: return "Sleeping"
        case .stoppedByUser: return "Stopped"
        case .error: return "Needs attention"
        }
    }

    private var stateColor: Color {
        switch project.state {
        case .awake: return theme.textMuted
        case .waking: return theme.accent
        case .hibernatedLight, .hibernatedDeep, .stoppedByUser: return theme.textFaint
        case .error: return theme.danger
        }
    }
}

/// Square tile: glyph on top, as the Mac rail draws it; 60 % with "zz" while asleep.
struct ProjectTile: View {
    @Environment(\.theme) private var theme
    let project: Project
    var size: CGFloat = Theme.tile

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
                .fill(theme.surface2)
                .overlay(
                    RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
                        .strokeBorder(theme.projectColor(project.color).opacity(0.7), lineWidth: 1)
                )
            Text(project.icon)
                .font(.system(size: size * 0.42, weight: .medium, design: .rounded))
                .foregroundStyle(theme.text)
                .opacity(project.state.isAsleep ? 0.6 : 1)
                .frame(width: size, height: size)
            if project.state.isAsleep {
                Text("zz")
                    .font(.system(size: 9, weight: .bold, design: .rounded))
                    .foregroundStyle(theme.textMuted)
                    .padding(4)
            }
        }
        .frame(width: size, height: size)
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

/// S14: grey "Reconnecting…" strip when the relay link is down.
struct LinkBanner: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        if model.link == .reconnecting {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small).tint(theme.textMuted)
                Text("Reconnecting… the transcript stays readable from your local replica.")
                    .font(.system(size: 12))
                    .foregroundStyle(theme.textMuted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface1))
        }
    }
}
