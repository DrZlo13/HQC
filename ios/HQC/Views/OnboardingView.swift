import QuartzCore
import SwiftUI
import UserNotifications

/// First run, and signing in again: welcome, the hub's address, sign-in through the browser,
/// notifications. A returning user whose sign-in expired sees only the sign-in step.
struct OnboardingView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    enum Step: Int, CaseIterable { case welcome, hub, signIn, notifications }

    @State private var step: Step
    @State private var address = ""
    @State private var busy = false
    /// How many of the welcome screen's parts are in: logo, title, three features, button.
    @State private var revealed = 0
    @State private var bell = 0
    @FocusState private var focused: Bool
    /// False on a first launch while the app boots: the deck shows the launch moment itself
    /// (as SwiftBooking's deck does), the logo centred at launch size on the launch colour,
    /// breathing. Flipped inside an animation, the logo flies up into the welcome screen.
    let isRevealed: Bool
    /// The breathing of the launch state, on its own scale so its repeating animation cannot
    /// fight the flight.
    @State private var pulse: CGFloat = 1
    @State private var pulseStart: CFTimeInterval?
    /// Where the welcome logo sits, and the whole screen, in window coordinates: the launch
    /// state offsets the logo from its slot to the screen's centre, where the OS put it.
    @State private var logoSlot: CGRect = .zero
    @State private var screen: CGRect = .zero

    init(start: Step, revealed: Bool = true) {
        _step = State(initialValue: start)
        isRevealed = revealed
    }

    private var launchOffset: CGSize {
        CGSize(width: screen.midX - logoSlot.midX, height: screen.midY - logoSlot.midY)
    }

    var body: some View {
        ZStack {
            BackdropGradient()
            // The OS launch screen's colour, fading into the gradient as the deck arrives.
            Color("LaunchBackground").ignoresSafeArea()
                .opacity(isRevealed ? 0 : 1)
            VStack(spacing: 0) {
                if step != .welcome {
                    progress
                        .padding(.top, 12)
                        .opacity(isRevealed ? 1 : 0)
                }
                Spacer(minLength: 20)
                Group {
                    switch step {
                    case .welcome: welcome
                    case .hub: hub
                    case .signIn: signIn
                    case .notifications: notifications
                    }
                }
                .frame(maxWidth: 460)
                .transition(.asymmetric(insertion: .move(edge: .trailing).combined(with: .opacity),
                                        removal: .move(edge: .leading).combined(with: .opacity)))
                .id(step)
                Spacer(minLength: 20)
            }
            .padding(.horizontal, 28)

            // The launch logo, exactly where and as large as the OS launch screen draws it, until
            // the reveal; then the welcome logo, starting from this very spot, flies up.
            if !isRevealed {
                Image("LaunchLogo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: SplashLogo.size, height: SplashLogo.size)
                    .scaleEffect(pulse)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .ignoresSafeArea()
                    .transition(.identity)
                    .accessibilityHidden(true)
            }
        }
        .background {
            Color.clear.ignoresSafeArea()
                .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { screen = $0 }
        }
        .breathingHaptic(isActive: !isRevealed, pulseStart: pulseStart)
        .onAppear {
            guard !isRevealed, !reduceMotion else { return }
            pulseStart = CACurrentMediaTime()
            withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) { pulse = 1.05 }
        }
        .onChange(of: isRevealed) { _, revealed in
            guard revealed else { return }
            SplashHaptics.shared.playResolved()
            Feedback.play(.arrive, haptic: false)
            withAnimation(.easeOut(duration: 0.25)) { pulse = 1 }
        }
        .animation(.snappy(duration: 0.4), value: step)
        .onChange(of: step) { old, new in
            Feedback.play(new.rawValue > old.rawValue ? .step : .tap)
        }
        .onChange(of: app.phase) { old, phase in
            busy = false
            switch phase {
            case .signIn: step = .signIn
            case .ready:
                if old != .ready { Feedback.play(.success) }
                step = .notifications
            default: break
            }
        }
        .onChange(of: app.lastError) { _, e in
            if e != nil { Feedback.play(.error) }
        }
    }

    private var progress: some View {
        HStack(spacing: 6) {
            ForEach(Step.allCases.dropFirst(), id: \.self) { s in
                Capsule()
                    .fill(s.rawValue <= step.rawValue ? Color.accentColor : palette.border)
                    .frame(width: s == step ? 22 : 8, height: 8)
            }
        }
        .animation(.smooth, value: step)
    }

    // MARK: steps

    private var welcome: some View {
        VStack(spacing: 30) {
            // The slot is measured untransformed; the logo in it starts, at the reveal, where
            // the launch logo is (the screen's centre, launch size) and flies into the slot.
            Color.clear
                .frame(width: 96, height: 96)
                .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { logoSlot = $0 }
                .overlay {
                    Image("Logo")
                        .resizable()
                        .scaledToFit()
                        .frame(width: 96, height: 96)
                        .clipShape(.rect(cornerRadius: 22))
                        // Appears in the same frame the launch logo goes, without a fade, so
                        // the two read as one.
                        .opacity(isRevealed ? 1 : 0)
                        .animation(nil, value: isRevealed)
                        .shadow(color: .black.opacity(isRevealed ? 0.2 : 0), radius: 18, y: 8)
                        .scaleEffect(pulse)
                        .scaleEffect(isRevealed ? 1 : SplashLogo.size / 96)
                        .offset(isRevealed ? .zero : launchOffset)
                }
            VStack(spacing: 10) {
                Text("HQC").font(.largeTitle.weight(.bold))
                Text("Your coding agents, on every machine, in your pocket.")
                    .font(.title3)
                    .foregroundStyle(palette.text2)
                    .multilineTextAlignment(.center)
            }
            .appear(revealed >= 2)
            VStack(alignment: .leading, spacing: 18) {
                feature("desktopcomputer", "Every machine, one place",
                        "Sessions on your Macs and servers, live, with what each agent is doing.", shown: revealed >= 3)
                feature("hand.raised", "Answer from anywhere",
                        "Allow a command or answer a question right from the notification.", shown: revealed >= 4)
                feature("person.3", "Agents that work together",
                        "Rooms, schedules and sessions that talk to each other.", shown: revealed >= 5)
            }
            .padding(.vertical, 8)
            primary("Get started") { step = .hub }
                .appear(revealed >= 6)
        }
        .task(id: isRevealed) { await revealWelcome() }
    }

    /// Once the logo is on its way, the welcome screen builds itself part by part, a tick and
    /// a rising note with each.
    private func revealWelcome() async {
        guard isRevealed, revealed == 0 else { return }
        revealed = 1
        for i in 2...6 {
            try? await Task.sleep(for: .milliseconds(i == 2 ? 260 : i == 6 ? 260 : 170))
            guard !Task.isCancelled else { return }
            withAnimation(.spring(duration: 0.55, bounce: i == 1 ? 0.45 : 0.25)) { revealed = i }
            Feedback.play(.reveal(i - 1))
        }
    }

    private func feature(_ icon: String, _ title: String, _ text: String, shown: Bool) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 32)
                .symbolEffect(.bounce, value: shown)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(text).font(.subheadline).foregroundStyle(palette.text2)
            }
        }
        .appear(shown, from: .leading)
    }

    private var hub: some View {
        VStack(spacing: 26) {
            header("server.rack", "Where is your hub?",
                   "The address you open hqc at in the browser.")
            VStack(spacing: 10) {
                TextField("hqc.example.com", text: $address)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .focused($focused)
                    .submitLabel(.continue)
                    .onSubmit(connect)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 14)
                    .glassEffect(.regular, in: .capsule)
                if let e = app.lastError, !busy {
                    Text(e).font(.footnote).foregroundStyle(palette.danger).multilineTextAlignment(.center)
                }
            }
            let connecting = busy || app.phase == .connecting
            primary(connecting ? "Connecting…" : "Continue", disabled: AppModel.normalize(address) == nil || connecting) {
                connect()
            }
            Link(destination: URL(string: "https://github.com/DrZlo13/HQC/blob/master/docs/RUNBOOK.md")!) {
                Label("No hub yet? How to set one up", systemImage: "book")
                    .font(.footnote)
            }
        }
        .onAppear {
            if address.isEmpty, let h = app.hubURL { address = h.host() ?? "" }
            focused = true
        }
    }

    private var signIn: some View {
        VStack(spacing: 26) {
            header("person.badge.key", "Sign in",
                   "\(app.hubURL?.host() ?? "Your hub") signs you in in the browser, with the account you use for it there.")
            if let e = app.lastError, !busy {
                Text(e).font(.footnote).foregroundStyle(palette.danger).multilineTextAlignment(.center)
                Button("Try again") { Task { await app.connect() } }.font(.subheadline)
            }
            primary(busy ? "Waiting for the browser…" : "Sign in", disabled: busy) {
                busy = true
                Task {
                    await app.signInWithBrowser()
                    busy = false
                }
            }
            Button("Use another hub") {
                app.forgetHub()
                step = .hub
            }
            .font(.footnote)
        }
    }

    private var notifications: some View {
        VStack(spacing: 26) {
            header("bell.badge", "Know when an agent needs you",
                   "Permission requests, questions and finished answers, with Allow and Deny right in the notification.",
                   wiggle: bell)
                .task {
                    // The bell rings twice, with the approval knock, to show what a notice feels like.
                    try? await Task.sleep(for: .milliseconds(450))
                    bell += 1
                    Feedback.play(.approval)
                }
            primary("Turn on notifications") {
                Task {
                    await AppDelegate.requestPush()
                    withAnimation(.spring(response: 0.75, dampingFraction: 0.98)) { app.finishOnboarding() }
                }
            }
            Button("Not now") { withAnimation(.spring(response: 0.75, dampingFraction: 0.98)) { app.finishOnboarding() } }
                .font(.subheadline)
        }
    }

    // MARK: parts

    private func header(_ icon: String, _ title: String, _ text: String, wiggle: Int = 0) -> some View {
        VStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(.tint)
                .symbolEffect(.bounce.up, options: .nonRepeating, value: step)
                .symbolEffect(.wiggle, options: .repeat(2), value: wiggle)
            Text(title).font(.title.weight(.bold)).multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text(text)
                .font(.body)
                .foregroundStyle(palette.text2)
                .multilineTextAlignment(.center)
        }
    }

    private func primary(_ title: String, disabled: Bool = false, action: @escaping () -> Void) -> some View {
        Button {
            Feedback.play(.tap)
            action()
        } label: {
            Text(title)
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 4)
        }
        .buttonStyle(.glassProminent)
        .controlSize(.large)
        .disabled(disabled)
    }

    private func connect() {
        guard let url = AppModel.normalize(address) else { return }
        busy = true
        app.lastError = nil
        app.setHub(url)
    }
}

/// A soft wash of the accent color behind onboarding, for the glass to refract.
struct BackdropGradient: View {
    @Environment(\.palette) private var palette

    var body: some View {
        MeshGradient(
            width: 3, height: 3,
            points: [[0, 0], [0.5, 0], [1, 0], [0, 0.5], [0.6, 0.45], [1, 0.5], [0, 1], [0.5, 1], [1, 1]],
            colors: [
                .accentColor.opacity(0.32), .clear, .blue.opacity(0.18),
                .clear, .accentColor.opacity(0.12), .clear,
                .purple.opacity(0.16), .clear, .accentColor.opacity(0.22),
            ]
        )
        .ignoresSafeArea()
        .background(palette.bg.ignoresSafeArea())
    }
}

private extension View {
    /// Fades and slides a part in when `shown` turns true.
    func appear(_ shown: Bool, from edge: Edge = .bottom) -> some View {
        self
            .opacity(shown ? 1 : 0)
            .offset(x: shown || edge != .leading ? 0 : -24, y: shown || edge != .bottom ? 0 : 18)
            .blur(radius: shown ? 0 : 4)
    }
}
