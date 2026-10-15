// Colours are paint, numbers drive layout. The values mirror
// `crates/arbos-mac/src/ui/theme.rs` (HSL → Color) so both clients look alike.
// Matte is the default; Glass uses system materials behind cards.

import SwiftUI

struct Theme {
    let isDark: Bool
    let surface: Surface

    let bg: Color
    let surface1: Color
    let surface2: Color
    let border: Color
    let borderStrong: Color
    let text: Color
    let textMuted: Color
    let textFaint: Color
    let accent: Color
    let accentSoft: Color
    let bubble: Color
    let warn: Color
    let danger: Color
    let dangerSoft: Color
    let codeBg: Color

    static let bubbleRadius: CGFloat = 14
    static let panelRadius: CGFloat = 10
    static let composerHeight: CGFloat = 48
    static let tile: CGFloat = 72
    static let body: CGFloat = 15.5
    static let bodyLine: CGFloat = 22
    static let small: CGFloat = 13
    static let tiny: CGFloat = 11.5

    static func dark(_ surface: Surface) -> Theme {
        Theme(
            isDark: true, surface: surface,
            bg: hsla(0, 0, 0.17), surface1: hsla(0, 0, 0.22), surface2: hsla(0, 0, 0.27),
            border: hsla(0, 0, 1, 0.09), borderStrong: hsla(0, 0, 1, 0.18),
            text: hsla(0, 0, 0.92), textMuted: hsla(0, 0, 0.64), textFaint: hsla(0, 0, 0.46),
            accent: hsla(0.70, 0.82, 0.74), accentSoft: hsla(0.70, 0.82, 0.74, 0.18),
            bubble: hsla(0, 0, 0.26, 0.9),
            warn: hsla(0.12, 0.85, 0.62), danger: hsla(0, 0.70, 0.62), dangerSoft: hsla(0, 0.70, 0.62, 0.14),
            codeBg: hsla(0, 0, 0, 0.25)
        )
    }

    static func light(_ surface: Surface) -> Theme {
        Theme(
            isDark: false, surface: surface,
            bg: hsla(0, 0, 0.965), surface1: hsla(0, 0, 0.93), surface2: hsla(0, 0, 0.88),
            border: hsla(0, 0, 0, 0.08), borderStrong: hsla(0, 0, 0, 0.16),
            text: hsla(0, 0, 0.12), textMuted: hsla(0, 0, 0.42), textFaint: hsla(0, 0, 0.58),
            accent: hsla(0.70, 0.70, 0.52), accentSoft: hsla(0.70, 0.70, 0.52, 0.16),
            bubble: hsla(0, 0, 0.88, 0.9),
            warn: hsla(0.10, 0.85, 0.48), danger: hsla(0, 0.65, 0.50), dangerSoft: hsla(0, 0.65, 0.50, 0.12),
            codeBg: hsla(0, 0, 0, 0.06)
        )
    }

    static func resolve(appearance: Appearance, surface: Surface, systemDark: Bool) -> Theme {
        let dark: Bool
        switch appearance {
        case .system: dark = systemDark
        case .dark: dark = true
        case .light: dark = false
        }
        return dark ? Theme.dark(surface) : Theme.light(surface)
    }

    var isGlass: Bool { surface == .glass }

    /// Floating cards: translucent tint on glass, flat card on matte (§3.11).
    var card: Color { isGlass ? surface1.opacity(0.55) : surface1 }

    /// Project accent from the registry `color`, falling back to the theme accent.
    func projectColor(_ hex: String) -> Color {
        Color(hex: hex) ?? accent
    }
}

private func hsla(_ h: Double, _ s: Double, _ l: Double, _ a: Double = 1) -> Color {
    // HSL → HSB, which `Color(hue:saturation:brightness:)` takes.
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
    static let defaultValue = Theme.dark(.matte)
}

extension EnvironmentValues {
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

/// Card background: frosted material on glass, flat tint on matte.
struct CardBackground: ViewModifier {
    @Environment(\.theme) private var theme
    var radius: CGFloat = Theme.panelRadius
    var stroke = true

    func body(content: Content) -> some View {
        content
            .background {
                if theme.isGlass {
                    RoundedRectangle(cornerRadius: radius, style: .continuous)
                        .fill(.ultraThinMaterial)
                        .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).fill(theme.card))
                } else {
                    RoundedRectangle(cornerRadius: radius, style: .continuous).fill(theme.card)
                }
            }
            .overlay {
                if stroke {
                    RoundedRectangle(cornerRadius: radius, style: .continuous)
                        .strokeBorder(theme.border, lineWidth: 1)
                }
            }
    }
}

extension View {
    func card(radius: CGFloat = Theme.panelRadius, stroke: Bool = true) -> some View {
        modifier(CardBackground(radius: radius, stroke: stroke))
    }
}
