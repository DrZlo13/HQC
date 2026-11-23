import SwiftUI

/// The stripe that runs around a working project's tile (the web client's `RailRun`): one lap
/// in 18 s while its agents write nothing, faster the more output tokens per second they write,
/// `min(6, 1 + log2(1 + rate / 40))` times: twice at 40 tokens/s, about 2.8 times at 100. A
/// change of rate changes the speed from where the stripe is, never making it jump.
struct RunRing: View {
    @Environment(\.palette) private var palette
    let rate: Double
    var cornerRadius: CGFloat = 12
    var lineWidth: CGFloat = 2
    @State private var clock = RingClock()

    static func speed(_ rate: Double) -> Double { min(6, 1 + log2(1 + max(0, rate) / 40)) }

    var body: some View {
        TimelineView(.animation) { context in
            let turn = clock.advance(to: context.date, speed: Self.speed(rate))
            RoundedRectangle(cornerRadius: cornerRadius)
                .strokeBorder(
                    AngularGradient(
                        stops: [
                            .init(color: .clear, location: 0),
                            .init(color: .clear, location: 220.0 / 360),
                            .init(color: palette.text2, location: 1),
                        ],
                        center: .center,
                        angle: .degrees(turn)
                    ),
                    lineWidth: lineWidth
                )
        }
        .allowsHitTesting(false)
        .accessibilityLabel("Working, \(Int(rate)) tokens per second")
    }
}

/// The ring's angle, advanced by the time since the last frame at the current speed.
@MainActor
final class RingClock {
    private var angle: Double = 0
    private var last: Date?

    func advance(to now: Date, speed: Double) -> Double {
        if let last {
            angle += now.timeIntervalSince(last) * speed * 360 / 18
            angle.formTruncatingRemainder(dividingBy: 360)
        }
        last = now
        return angle
    }
}

/// A project's tile with the run ring while its sessions work.
struct ProjectBadge: View {
    @Environment(AppModel.self) private var app
    let project: Project
    var size: CGFloat = 40

    var body: some View {
        let rate = app.projectRate(project.id)
        let working = app.projectWorking(project.id)
        ProjectTile(project: project, size: size)
            .padding(4)
            .overlay {
                if working {
                    RunRing(rate: rate, cornerRadius: size * 0.28 + 4)
                        .transition(.opacity)
                }
            }
            .animation(.smooth, value: working)
    }
}

extension AppModel {
    /// The output tokens per second of a project's working sessions and their subagents.
    func projectRate(_ projectID: String) -> Double {
        summaries.values.filter { $0.project_id == projectID && $0.state.isBusy }
            .reduce(0) { $0 + ($1.output_per_s ?? 0) }
    }

    func projectWorking(_ projectID: String) -> Bool {
        summaries.values.contains { $0.project_id == projectID && $0.state.isBusy }
    }

    func projectAgents(_ projectID: String) -> Int {
        summaries.values.filter { $0.project_id == projectID && $0.state.isBusy }
            .reduce(0) { $0 + 1 + ($1.subagents ?? 0) }
    }
}
