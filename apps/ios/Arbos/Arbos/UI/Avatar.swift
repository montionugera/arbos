// Grok Bot's blob avatars: a coloured shape with two white slanted "eyes".
// The shape and colour come from the project (its colour, and a stable hash
// of its id for the shape), so a project looks the same on every device.

import SwiftUI

enum BlobShape: CaseIterable {
    case circle, squircle, cloud, drop, hex, blob

    static func pick(for id: String) -> BlobShape {
        var h: UInt32 = 2166136261
        for b in id.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
        let all = BlobShape.allCases
        return all[Int(h % UInt32(all.count))]
    }
}

struct BlobAvatar: View {
    let color: Color
    let shape: BlobShape
    var size: CGFloat = Theme.avatar
    /// Machine/online dot (bottom-right), nil hides it.
    var dot: Color? = nil
    /// Ring around the dot: the surface the avatar sits on.
    var ring: Color = Color(hex: "#141414")!

    init(project: Project, theme: Theme, size: CGFloat = Theme.avatar, dot: Color? = nil) {
        self.color = theme.projectColor(project)
        self.shape = BlobShape.pick(for: project.id)
        self.size = size
        self.dot = dot
        self.ring = theme.bg
    }

    init(color: Color, shape: BlobShape, size: CGFloat = Theme.avatar, dot: Color? = nil) {
        self.color = color
        self.shape = shape
        self.size = size
        self.dot = dot
    }

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            ZStack {
                blobPath.fill(color)
                eyes
            }
            .frame(width: size, height: size)
            if let dot {
                Circle()
                    .fill(dot)
                    .frame(width: size * 0.36, height: size * 0.36)
                    .overlay(Circle().strokeBorder(ring, lineWidth: max(1.5, size * 0.06)))
                    .offset(x: size * 0.08, y: size * 0.08)
            }
        }
        .frame(width: size, height: size)
    }

    private var eyes: some View {
        HStack(spacing: size * 0.14) {
            Capsule().fill(.white).frame(width: size * 0.12, height: size * 0.34)
            Capsule().fill(.white).frame(width: size * 0.12, height: size * 0.34)
        }
        .rotationEffect(.degrees(-18))
        .offset(x: size * 0.02, y: -size * 0.02)
    }

    private var blobPath: Path {
        let r = CGRect(x: 0, y: 0, width: size, height: size)
        switch shape {
        case .circle:
            return Path(ellipseIn: r)
        case .squircle:
            return Path(roundedRect: r, cornerRadius: size * 0.3, style: .continuous)
        case .cloud:
            var p = Path()
            p.addEllipse(in: CGRect(x: size * 0.02, y: size * 0.30, width: size * 0.56, height: size * 0.56))
            p.addEllipse(in: CGRect(x: size * 0.42, y: size * 0.30, width: size * 0.56, height: size * 0.56))
            p.addEllipse(in: CGRect(x: size * 0.20, y: size * 0.08, width: size * 0.60, height: size * 0.60))
            p.addRect(CGRect(x: size * 0.25, y: size * 0.45, width: size * 0.5, height: size * 0.38))
            return p
        case .drop:
            var p = Path()
            p.move(to: CGPoint(x: size * 0.5, y: 0))
            p.addCurve(to: CGPoint(x: size * 0.98, y: size * 0.62), control1: CGPoint(x: size * 0.7, y: size * 0.2), control2: CGPoint(x: size * 0.98, y: size * 0.38))
            p.addArc(center: CGPoint(x: size * 0.5, y: size * 0.62), radius: size * 0.48, startAngle: .degrees(0), endAngle: .degrees(180), clockwise: false)
            p.addCurve(to: CGPoint(x: size * 0.5, y: 0), control1: CGPoint(x: size * 0.02, y: size * 0.38), control2: CGPoint(x: size * 0.3, y: size * 0.2))
            return p
        case .hex:
            var p = Path()
            for i in 0..<6 {
                let a = Double(i) * .pi / 3 - .pi / 6
                let pt = CGPoint(x: size / 2 + cos(a) * size / 2, y: size / 2 + sin(a) * size / 2)
                if i == 0 { p.move(to: pt) } else { p.addLine(to: pt) }
            }
            p.closeSubpath()
            return p
        case .blob:
            var p = Path()
            p.addEllipse(in: CGRect(x: 0, y: size * 0.12, width: size * 0.72, height: size * 0.76))
            p.addEllipse(in: CGRect(x: size * 0.30, y: 0, width: size * 0.70, height: size * 0.66))
            p.addEllipse(in: CGRect(x: size * 0.36, y: size * 0.40, width: size * 0.62, height: size * 0.60))
            return p
        }
    }
}

/// Small coloured dot for machine / workspace state.
func machineDotColor(_ state: BoxState, theme: Theme) -> Color {
    switch state {
    case .awake: return theme.online
    case .waking: return theme.warn
    case .hibernatedLight, .hibernatedDeep, .stoppedByUser: return theme.muted.opacity(0.6)
    case .error: return theme.danger
    }
}
