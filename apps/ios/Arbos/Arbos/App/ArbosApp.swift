import SwiftUI

@main
struct ArbosApp: App {
    @StateObject private var model = AppModel()
    @Environment(\.colorScheme) private var systemScheme

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
        }
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.colorScheme) private var systemScheme

    private var theme: Theme {
        Theme.resolve(appearance: model.appearance, surface: model.surface, systemDark: systemScheme == .dark)
    }

    var body: some View {
        Group {
            if model.isPaired {
                ProjectsView()
            } else {
                PairingView()
            }
        }
        .environment(\.theme, theme)
        .preferredColorScheme(preferredScheme)
        .tint(theme.accent)
        .alert(item: $model.notice) { n in
            Alert(title: Text("Arbos"), message: Text(n.text), dismissButton: .default(Text("OK")))
        }
    }

    private var preferredScheme: ColorScheme? {
        switch model.appearance {
        case .system: return nil
        case .dark: return .dark
        case .light: return .light
        }
    }
}
