// Pair this device with the relay through a one-time six-digit code (D35).
// The device token lands in the Keychain; nothing else is stored.

import SwiftUI

struct PairingView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var code = ""
    @State private var relayUrl = ""
    @FocusState private var focus: Field?

    enum Field { case relay, code }

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 18) {
                    Spacer(minLength: 60)
                    Text("Ω")
                        .font(.system(size: 44, weight: .light, design: .rounded))
                        .foregroundStyle(theme.accent)
                    Text("Pair this iPhone")
                        .font(.system(size: 24, weight: .semibold))
                        .foregroundStyle(theme.text)
                    Text("Enter your relay address and the one-time code from the relay log or `arbosctl pair-code`. The device token is kept in your Keychain.")
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.textMuted)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 28)

                    pill {
                        Image(systemName: "network")
                            .font(.system(size: 14))
                            .foregroundStyle(theme.textFaint)
                        TextField("https://relay.example.com", text: $relayUrl)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(size: 15))
                            .foregroundStyle(theme.text)
                            .focused($focus, equals: .relay)
                            .submitLabel(.next)
                            .onSubmit { focus = .code }
                    }

                    pill {
                        Image(systemName: "key")
                            .font(.system(size: 14))
                            .foregroundStyle(theme.textFaint)
                        TextField("One-time pairing code", text: $code)
                            .keyboardType(.numberPad)
                            .textContentType(.oneTimeCode)
                            .font(.system(size: 18, weight: .medium, design: .monospaced))
                            .foregroundStyle(theme.text)
                            .focused($focus, equals: .code)
                            .onChange(of: code) { _, new in
                                let digits = new.filter(\.isNumber)
                                if digits != new || digits.count > 6 { code = String(digits.prefix(6)) }
                            }
                        Button(action: submit) {
                            Text(model.pairingBusy ? "Pairing…" : "Pair")
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundStyle(theme.bg)
                                .padding(.horizontal, 14)
                                .padding(.vertical, 8)
                                .background(Capsule().fill(theme.text))
                        }
                        .disabled(!canSubmit)
                        .opacity(canSubmit ? 1 : 0.5)
                    }
                    .overlay {
                        if model.pairingError != nil {
                            Capsule().strokeBorder(theme.danger, lineWidth: 1).padding(.horizontal, 24)
                        }
                    }

                    if let err = model.pairingError {
                        Text(err)
                            .font(.system(size: 12.5))
                            .foregroundStyle(theme.danger)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 28)
                    }
                    Spacer(minLength: 40)
                }
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .onAppear {
            relayUrl = model.relayUrl
            focus = relayUrl.isEmpty ? .relay : .code
            if let hook = DebugHooks.pairCode {
                code = hook
                submit()
            }
        }
    }

    private func pill<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 10, content: content)
            .padding(.horizontal, 16)
            .frame(height: Theme.composerHeight + 6)
            .card(radius: (Theme.composerHeight + 6) / 2)
            .padding(.horizontal, 24)
    }

    private var canSubmit: Bool {
        code.count == 6 && !relayUrl.trimmingCharacters(in: .whitespaces).isEmpty && !model.pairingBusy
    }

    private func submit() {
        guard canSubmit else { return }
        model.setRelayUrl(relayUrl)
        model.pair(code: code)
    }
}
