import SwiftUI

/// Placeholders for what has not loaded yet: shapes in the palette's own tones with a soft
/// light sweeping across them, so the app opens at once and fills in as the hub answers.
struct SkeletonBlock: View {
    @Environment(\.palette) private var palette
    var cornerRadius: CGFloat = 6

    var body: some View {
        RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            .fill(palette.text.opacity(0.08))
            .modifier(SkeletonShimmer())
            .clipShape(.rect(cornerRadius: cornerRadius, style: .continuous))
            .accessibilityHidden(true)
    }
}

/// A row: an icon (a dot when small) and lines of text, `widths` as fractions of the row.
struct SkeletonRow: View {
    var icon: CGFloat = 40
    var widths: [Double] = [0.55, 0.8]

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            if icon > 0 {
                SkeletonBlock(cornerRadius: icon < 16 ? icon / 2 : icon * 0.28)
                    .frame(width: icon, height: icon)
            }
            GeometryReader { geo in
                VStack(alignment: .leading, spacing: 7) {
                    ForEach(Array(widths.enumerated()), id: \.offset) { i, w in
                        SkeletonBlock(cornerRadius: 4)
                            .frame(width: geo.size.width * w, height: i == 0 ? 14 : 11)
                    }
                }
                .frame(maxHeight: .infinity, alignment: .center)
            }
            .frame(height: CGFloat(widths.count) * 18 + 4)
        }
        .padding(.vertical, 6)
        .accessibilityLabel("Loading")
    }
}

/// A list's worth of rows, with uneven widths so it reads as text.
struct SkeletonList: View {
    var rows = 5
    var icon: CGFloat = 40

    var body: some View {
        VStack(spacing: 4) {
            ForEach(0..<rows, id: \.self) { i in
                SkeletonRow(icon: icon, widths: [0.62 - Double(i % 3) * 0.13, 0.4 + Double(i % 2) * 0.2])
                    .padding(.horizontal, 20)
            }
            Spacer(minLength: 0)
        }
        .padding(.top, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}

/// A chat before its history arrives: a user message on the right, an answer on the left.
struct ChatSkeleton: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            ForEach(0..<2, id: \.self) { _ in
                HStack {
                    Spacer(minLength: 80)
                    SkeletonBlock(cornerRadius: 14).frame(width: 200, height: 44)
                }
                VStack(alignment: .leading, spacing: 8) {
                    SkeletonBlock(cornerRadius: 4).frame(maxWidth: 420).frame(height: 13)
                    SkeletonBlock(cornerRadius: 4).frame(maxWidth: 360).frame(height: 13)
                    SkeletonBlock(cornerRadius: 4).frame(maxWidth: 240).frame(height: 13)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 20)
        .frame(maxWidth: 780)
        .frame(maxWidth: .infinity)
        .allowsHitTesting(false)
    }
}

/// The light sweeping across a skeleton; still under Reduce Motion.
private struct SkeletonShimmer: ViewModifier {
    @Environment(\.palette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -1

    func body(content: Content) -> some View {
        content
            .overlay {
                if !reduceMotion {
                    GeometryReader { geo in
                        LinearGradient(colors: [.clear, palette.text.opacity(0.07), .clear],
                                       startPoint: .leading, endPoint: .trailing)
                            .frame(width: max(geo.size.width, 120) * 0.8)
                            .offset(x: phase * max(geo.size.width, 120) * 1.4)
                    }
                }
            }
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.linear(duration: 1.3).repeatForever(autoreverses: false)) { phase = 1.2 }
            }
    }
}
