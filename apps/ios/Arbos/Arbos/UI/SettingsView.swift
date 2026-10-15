import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var relayUrl = ""
    @State private var confirmUnpair = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    LabeledContent("State") {
                        HStack(spacing: 6) {
                            if model.machine.state == .waking {
                                ProgressView().controlSize(.small)
                            }
                            Text(model.machine.label)
                        }
                    }
                    if let since = model.machine.since, since > 0 {
                        LabeledContent("Since", value: SettingsView.relative(since))
                    }
                    LabeledContent("Spec", value: model.machine.spec.isEmpty ? "Not reported yet" : model.machine.spec)
                    switch model.machine.state {
                    case .awake, .waking:
                        Button("Stop machine", role: .destructive) { model.stopMachine() }
                            .disabled(model.machineBusy)
                    case .hibernatedLight, .hibernatedDeep, .stoppedByUser, .error:
                        Button("Start machine") { model.startMachine() }
                            .disabled(model.machineBusy || model.projects.isEmpty)
                    }
                } header: {
                    Text("Machine")
                } footer: {
                    Text("Your one Daytona computer. Projects are workspaces on it; a message to any project wakes it.")
                }
                Section("Appearance") {
                    Picker("Theme", selection: $model.appearanceSetting) {
                        ForEach(Appearance.allCases) { a in Text(a.label).tag(a.rawValue) }
                    }
                    Picker("Surface", selection: $model.surfaceSetting) {
                        ForEach(Surface.allCases) { s in Text(s.label).tag(s.rawValue) }
                    }
                    .pickerStyle(.segmented)
                }
                Section {
                    TextField("https://relay.example.com", text: $relayUrl)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit(applyRelay)
                    if RelayEndpoint(relayUrl).httpUrl != model.relayUrl {
                        Button("Switch relay", action: applyRelay)
                    }
                    LabeledContent("Device") {
                        Text(model.deviceId)
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    LabeledContent("Link") {
                        Text(linkLabel)
                    }
                } header: {
                    Text("Relay")
                } footer: {
                    Text("The device token is kept in the Keychain. Switching relays signs this device out of the current one.")
                }
                Section {
                    Button("Sign out this device", role: .destructive) { confirmUnpair = true }
                }
                Section("About") {
                    LabeledContent("Version", value: SettingsView.versionLine)
                    LabeledContent("Protocol", value: "room proto \(protoVersion) · Loro")
                }
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .confirmationDialog("Sign out this device?", isPresented: $confirmUnpair, titleVisibility: .visible) {
                Button("Sign out", role: .destructive) {
                    model.unpair()
                    dismiss()
                }
            } message: {
                Text("You will need a new pairing code to connect again.")
            }
            .onAppear { relayUrl = model.relayUrl }
        }
    }

    private var linkLabel: String {
        switch model.link {
        case .connecting: return "Connecting…"
        case .live: return "Live"
        case .reconnecting: return "Reconnecting…"
        }
    }

    private func applyRelay() {
        let trimmed = relayUrl.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        model.setRelayUrl(trimmed)
        dismiss()
    }

    static func relative(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: Date())
    }

    static var versionLine: String {
        let info = Bundle.main.infoDictionary
        let v = info?["CFBundleShortVersionString"] as? String ?? "?"
        let b = info?["CFBundleVersion"] as? String ?? "?"
        return "\(v) (\(b))"
    }
}
