// Grok Bot visual language, measured from the dark-mode references in
// internal/refs/grokbot-ios (iPhone Air, 1260×2736 @3x): background #141414,
// assistant bubble #202020, user bubble #5C5C5C, 17 pt SF Pro body with the
// default 22 pt line height, 20 pt continuous corners, glass circles for the
// header controls, system lines in #95959B.

import SwiftUI

struct Theme {
    let isDark: Bool
    var surface: Surface

    let bg: Color
    /// Assistant bubble.
    let bubbleAssistant: Color
    /// User bubble.
    let bubbleUser: Color
    /// Composer field / glass circle fill.
    let field: Color
    let fieldBorder: Color
    let text: Color
    /// System lines, timestamps, "is working".
    let muted: Color
    /// Placeholder text.
    let placeholder: Color
    let icon: Color
    let online: Color
    let warn: Color
    let danger: Color
    let dangerSoft: Color
    let card: Color
    let separator: Color

    static let bubbleRadius: CGFloat = 20
    static let bubbleHPad: CGFloat = 14
    static let bubbleVPad: CGFloat = 11
    /// 17 pt SF Pro renders a 20.3 pt line in SwiftUI; Grok shows UIKit's 22 pt body pitch.
    static let bodyLineSpacing: CGFloat = 1.7
    /// Screen width minus 76 pt (16 pt own margin + 60 pt on the other side).
    static let bubbleInset: CGFloat = 76
    static let body: CGFloat = 17
    static let small: CGFloat = 15
    static let tiny: CGFloat = 13
    static let control: CGFloat = 44
    static let fieldHeight: CGFloat = 42
    static let voiceButton: CGFloat = 36
    static let plusButton: CGFloat = 43
    static let avatar: CGFloat = 28

    static let dark = Theme(
        isDark: true, surface: .matte,
        bg: Color(hex: "#141414")!,
        bubbleAssistant: Color(hex: "#202020")!,
        bubbleUser: Color(hex: "#5C5C5C")!,
        field: Color(hex: "#181818")!,
        fieldBorder: Color(hex: "#242424")!,
        text: .white,
        muted: Color(hex: "#95959B")!,
        placeholder: Color(hex: "#58585B")!,
        icon: Color(hex: "#96969C")!,
        online: Color(hex: "#34C759")!,
        warn: Color(hex: "#FFB340")!,
        danger: Color(hex: "#FF453A")!,
        dangerSoft: Color(hex: "#FF453A")!.opacity(0.14),
        card: Color(hex: "#1C1C1C")!,
        separator: Color.white.opacity(0.08)
    )

    static let light = Theme(
        isDark: false, surface: .matte,
        bg: Color(hex: "#F7F7F7")!,
        bubbleAssistant: Color(hex: "#ECECEC")!,
        bubbleUser: Color(hex: "#111111")!,
        field: .white,
        fieldBorder: Color(hex: "#E2E2E2")!,
        text: Color(hex: "#111111")!,
        muted: Color(hex: "#8A8A8E")!,
        placeholder: Color(hex: "#A0A0A5")!,
        icon: Color(hex: "#6E6E73")!,
        online: Color(hex: "#34C759")!,
        warn: Color(hex: "#E59A00")!,
        danger: Color(hex: "#D7332A")!,
        dangerSoft: Color(hex: "#D7332A")!.opacity(0.10),
        card: .white,
        separator: Color.black.opacity(0.08)
    )

    static func resolve(appearance: Appearance, surface: Surface, systemDark: Bool) -> Theme {
        let dark: Bool
        switch appearance {
        case .system: dark = systemDark
        case .dark: dark = true
        case .light: dark = false
        }
        var t = dark ? Theme.dark : Theme.light
        t.surface = surface
        return t
    }

    /// Text colour on a user bubble (white on dark grey, white on black).
    var userText: Color { .white }
    /// Primary filled control (send/voice button, main CTA): white on dark, black on light.
    var primary: Color { isDark ? .white : Color(hex: "#111111")! }
    var onPrimary: Color { isDark ? .black : .white }

    /// Grok's bot palette; a project's hex colour snaps to the nearest one.
    static let palette: [Color] = [
        Color(hex: "#1084FE")!, Color(hex: "#10B981")!, Color(hex: "#FF7A00")!, Color(hex: "#FF3B5C")!,
        Color(hex: "#FF2D9B")!, Color(hex: "#8B5CF6")!, Color(hex: "#8E5A3C")!, Color(hex: "#8E8E93")!,
        Color(hex: "#FFB000")!, Color(hex: "#14B8A6")!,
    ]

    func projectColor(_ hex: String) -> Color {
        Color(hex: hex) ?? Theme.palette[0]
    }

    /// The relay hands every project the same default colour (#8b7cf6); Grok
    /// gives each bot its own, so a default-coloured project picks a palette
    /// colour from a stable hash of its id.
    func projectColor(_ project: Project) -> Color {
        let hex = project.color.lowercased()
        if hex == "#8b7cf6" || hex.isEmpty {
            var h: UInt32 = 2166136261
            for b in project.id.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
            return Theme.palette[Int(h % UInt32(Theme.palette.count))]
        }
        return Color(hex: project.color) ?? Theme.palette[0]
    }
}

private func hsla(_ h: Double, _ s: Double, _ l: Double, _ a: Double = 1) -> Color {
    let v = l + s * min(l, 1 - l)
    let sv = v == 0 ? 0 : 2 * (1 - l / v)
    return Color(hue: h, saturation: sv, brightness: v, opacity: a)
}

extension Color {
    init?(hex: String) {
        var s = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        self.init(
            red: Double((v >> 16) & 0xff) / 255,
            green: Double((v >> 8) & 0xff) / 255,
            blue: Double(v & 0xff) / 255
        )
    }
}

private struct ThemeKey: EnvironmentKey {
    static let defaultValue = Theme.dark
}

extension EnvironmentValues {
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

/// Glass circle (header controls, composer "+"): material behind a dark tint
/// and a hairline rim, as in the references.
struct GlassCircle<Content: View>: View {
    @Environment(\.theme) private var theme
    var size: CGFloat = Theme.control
    @ViewBuilder var content: () -> Content

    var body: some View {
        ZStack {
            Circle().fill(.ultraThinMaterial)
            Circle().fill(theme.field.opacity(0.85))
            Circle().strokeBorder(theme.fieldBorder, lineWidth: 1)
            content()
        }
        .frame(width: size, height: size)
    }
}

/// Glass pill (header title).
struct GlassPill<Content: View>: View {
    @Environment(\.theme) private var theme
    @ViewBuilder var content: () -> Content

    var body: some View {
        content()
            .background {
                ZStack {
                    Capsule().fill(.ultraThinMaterial)
                    Capsule().fill(theme.field.opacity(0.85))
                    Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1)
                }
            }
    }
}
