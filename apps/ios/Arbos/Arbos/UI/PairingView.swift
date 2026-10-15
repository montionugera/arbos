// Pair this device with the relay through a one-time six-digit code (D35),
// in the Grok Bot welcome style: calm centre, two rounded fields, one
// white primary button. The device token lands in the Keychain.

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
                VStack(spacing: 16) {
                    Spacer(minLength: 110)
                    HStack(spacing: -10) {
                        BlobAvatar(color: Theme.palette[1], shape: .blob, size: 54)
                        BlobAvatar(color: Theme.palette[0], shape: .drop, size: 54)
                        BlobAvatar(color: Theme.palette[2], shape: .cloud, size: 54)
                    }
                    .padding(.bottom, 10)
                    Text("Arbos")
                        .font(.system(size: 30, weight: .semibold))
                        .foregroundStyle(theme.text)
                    Text("Your team of always-on agents\nthat finish the work.")
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.muted)
                        .multilineTextAlignment(.center)
                        .padding(.bottom, 22)

                    field {
                        Image(systemName: "network").font(.system(size: 15)).foregroundStyle(theme.icon)
                        TextField("Relay address", text: $relayUrl, prompt: Text("Relay address").foregroundStyle(theme.placeholder))
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.text)
                            .focused($focus, equals: .relay)
                            .submitLabel(.next)
                            .onSubmit { focus = .code }
                    }
                    field {
                        Image(systemName: "key").font(.system(size: 15)).foregroundStyle(theme.icon)
                        TextField("One-time code", text: $code, prompt: Text("One-time code").foregroundStyle(theme.placeholder))
                            .keyboardType(.numberPad)
                            .textContentType(.oneTimeCode)
                            .font(.system(size: Theme.body, design: .monospaced))
                            .foregroundStyle(theme.text)
                            .focused($focus, equals: .code)
                            .onChange(of: code) { _, new in
                                let digits = new.filter(\.isNumber)
                                if digits != new || digits.count > 6 { code = String(digits.prefix(6)) }
                            }
                            .onSubmit(submit)
                    }

                    Button(action: submit) {
                        Text(model.pairingBusy ? "Pairing…" : "Pair this iPhone")
                            .font(.system(size: Theme.body, weight: .semibold))
                            .foregroundStyle(theme.onPrimary)
                            .frame(maxWidth: .infinity)
                            .frame(height: 50)
                            .background(Capsule().fill(theme.primary))
                    }
                    .buttonStyle(.plain)
                    .disabled(!canSubmit)
                    .opacity(canSubmit ? 1 : 0.45)
                    .padding(.horizontal, 24)
                    .padding(.top, 6)

                    if let err = model.pairingError {
                        Text(err)
                            .font(.system(size: Theme.small))
                            .foregroundStyle(theme.danger)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 28)
                    }
                    Text("The code comes from the relay log or `arbosctl pair-code`. Your device token stays in the Keychain.")
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.muted)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 36)
                        .padding(.top, 8)
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

    private func field<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 10, content: content)
            .padding(.horizontal, 16)
            .frame(height: 50)
            .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
            .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
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
