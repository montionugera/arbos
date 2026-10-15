// Voice chat screen, Grok Bot layout: avatar, name, "Calling…" status, mute /
// transcript / hang-up controls. This relay has no voice endpoint yet, so the
// call never connects: after a moment the status says so and the only way
// out is Hang up. The visual is in place for when the backend lands.

import SwiftUI

struct VoiceModeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var status = "Calling…"
    @State private var muted = false
    @State private var pulse = false

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                HStack {
                    Spacer()
                    Text("Voice chat")
                        .font(.system(size: Theme.small, weight: .semibold))
                        .foregroundStyle(theme.muted)
                    Spacer()
                }
                .padding(.top, 14)
                Spacer()
                ZStack {
                    Circle()
                        .fill(theme.projectColor(project).opacity(0.18))
                        .frame(width: 220, height: 220)
                        .scaleEffect(pulse ? 1.08 : 0.94)
                    BlobAvatar(project: project, theme: theme, size: 128)
                }
                .onAppear {
                    withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) { pulse = true }
                }
                Text(project.name)
                    .font(.system(size: 28, weight: .semibold))
                    .foregroundStyle(theme.text)
                    .padding(.top, 28)
                Text(status)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.muted)
                    .padding(.top, 6)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
                Spacer()
                HStack(spacing: 28) {
                    control(icon: muted ? "mic.slash.fill" : "mic.fill", label: muted ? "Unmute" : "Mute") { muted.toggle() }
                    control(icon: "text.bubble", label: "Transcript") { }
                    Button { dismiss() } label: {
                        VStack(spacing: 8) {
                            Circle().fill(theme.danger).frame(width: 64, height: 64)
                                .overlay(Image(systemName: "phone.down.fill").font(.system(size: 24, weight: .semibold)).foregroundStyle(.white))
                            Text("Hang up").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                    }
                    .buttonStyle(.plain)
                }
                .padding(.bottom, 48)
            }
        }
        .task {
            try? await Task.sleep(nanoseconds: 2_200_000_000)
            status = "Voice chat is not available on this relay yet.\nText replies keep working."
        }
    }

    private func control(icon: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                GlassCircle(size: 64) {
                    Image(systemName: icon).font(.system(size: 22, weight: .medium)).foregroundStyle(theme.text)
                }
                Text(label).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
            }
        }
        .buttonStyle(.plain)
    }
}
