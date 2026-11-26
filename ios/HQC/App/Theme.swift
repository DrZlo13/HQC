import SwiftUI

/// The web client's base palettes (client/src/theme.ts, T49): three dark bases and their light
/// counterparts. The choice lives on the hub (`PATCH /api/me {theme}`), the same on every
/// device; "Match system" keeps the family and follows the device's light or dark setting.
nonisolated enum ThemeName: String, CaseIterable, Identifiable, Sendable {
    case warm, graphite, contrast, warm_light, graphite_light, contrast_light
    var id: String { rawValue }

    var isLight: Bool { rawValue.hasSuffix("_light") }
    var family: String { rawValue.replacingOccurrences(of: "_light", with: "") }
    var label: String {
        switch family {
        case "graphite": "Graphite"
        case "contrast": "High contrast"
        default: "Warm"
        }
    }

    func variant(light: Bool) -> ThemeName {
        ThemeName(rawValue: light ? "\(family)_light" : family) ?? (light ? .warm_light : .warm)
    }
}

/// The colors of one base, as `theme.ts` defines them, plus the fixed status colors.
nonisolated struct Palette: Equatable, Sendable {
    let light: Bool
    let bg: Color
    let side: Color
    let rail: Color
    let surface: Color
    let border: Color
    let text: Color
    let text2: Color
    let muted: Color
    let code: Color

    let waiting: Color
    let online: Color
    let danger: Color
    let relay: Color
    let relayText: Color
    let permissionBG: Color
    let permissionBorder: Color

    static func of(_ t: ThemeName) -> Palette {
        func p(_ light: Bool, _ c: [UInt32]) -> Palette {
            Palette(
                light: light, bg: Color(hex: c[0]), side: Color(hex: c[1]), rail: Color(hex: c[2]),
                surface: Color(hex: c[3]), border: Color(hex: c[4]), text: Color(hex: c[5]),
                text2: Color(hex: c[6]), muted: Color(hex: c[7]), code: Color(hex: c[8]),
                waiting: Color(hex: light ? 0x9C5B00 : 0xF2B35B),
                online: Color(hex: light ? 0x1A7F45 : 0x5CC98A),
                danger: Color(hex: light ? 0xB83A2C : 0xF59486),
                relay: Color(hex: light ? 0x6E5BD8 : 0xA99BFF),
                relayText: Color(hex: light ? 0x5642B8 : 0xC4BAFF),
                permissionBG: Color(hex: light ? 0xFFF5E3 : 0x1F1A12),
                permissionBorder: Color(hex: light ? 0xE3C48A : 0x5A4520)
            )
        }
        switch t {
        case .warm: return p(false, [0x1D1B18, 0x191715, 0x100E0C, 0x282521, 0x3A352F, 0xF2EEE8, 0xD8D1C7, 0xADA599, 0x141210])
        case .graphite: return p(false, [0x1A1C1F, 0x16181B, 0x0E0F11, 0x25282C, 0x353A40, 0xEDEFF2, 0xCDD2D8, 0xA3A9B1, 0x121315])
        case .contrast: return p(false, [0x121110, 0x0F0E0D, 0x070706, 0x211F1C, 0x4A443C, 0xFFFFFF, 0xE9E4DC, 0xC6BEB2, 0x0A0908])
        case .warm_light: return p(true, [0xFAF7F2, 0xF2EDE5, 0xE6DFD3, 0xFFFFFF, 0xDDD5C8, 0x1F1B16, 0x3E372F, 0x6B6257, 0xF0EBE3])
        case .graphite_light: return p(true, [0xF6F7F9, 0xEDEFF2, 0xE0E3E8, 0xFFFFFF, 0xD5D9DF, 0x16191D, 0x353B43, 0x5E6670, 0xEDEFF2])
        case .contrast_light: return p(true, [0xFFFFFF, 0xF4F2EF, 0xE4DFD8, 0xFFFFFF, 0x8A8276, 0x000000, 0x1C1915, 0x463F37, 0xF1EEE9])
        }
    }
}

private struct PaletteKey: EnvironmentKey {
    static let defaultValue = Palette.of(.warm)
}

private struct DenseUIKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var palette: Palette {
        get { self[PaletteKey.self] }
        set { self[PaletteKey.self] = newValue }
    }

    /// Compact, high-density layout ("vvzvlad mode") modeled on the web client's `list-compact`.
    var denseUI: Bool {
        get { self[DenseUIKey.self] }
        set { self[DenseUIKey.self] = newValue }
    }
}

/// Applies the chosen palette: its color scheme for system controls, its colors for ours.
struct Themed: ViewModifier {
    @Environment(AppModel.self) private var app
    @Environment(\.colorScheme) private var system

    func body(content: Content) -> some View {
        let theme = app.effectiveTheme(systemIsDark: system == .dark)
        let palette = Palette.of(theme)
        content
            .environment(\.palette, palette)
            .environment(\.denseUI, app.denseUI)
            .preferredColorScheme(app.followSystem ? nil : (palette.light ? .light : .dark))
            .tint(Color(hex: 0x3FB8A8))
    }
}

extension View {
    func themed() -> some View { modifier(Themed()) }

    /// A screen's background: the palette's base, under bars and the home indicator.
    func paletteBackground(_ keyPath: KeyPath<Palette, Color> = \.bg) -> some View {
        modifier(PaletteBackground(keyPath: keyPath))
    }
}

private struct PaletteBackground: ViewModifier {
    @Environment(\.palette) private var palette
    let keyPath: KeyPath<Palette, Color>

    func body(content: Content) -> some View {
        content
            .scrollContentBackground(.hidden)
            .background(palette[keyPath: keyPath].ignoresSafeArea())
    }
}
