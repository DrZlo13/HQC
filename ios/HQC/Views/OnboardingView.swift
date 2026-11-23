import SwiftUI
import UserNotifications

/// First run, and signing in again: welcome, the hub's address, sign-in through the browser,
/// notifications. A returning user whose sign-in expired sees only the sign-in step.
struct OnboardingView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.palette) private var palette

    enum Step: Int, CaseIterable { case welcome, hub, signIn, notifications }

    @State private var step: Step
    @State private var address = ""
    @State private var busy = false
    @FocusState private var focused: Bool

    init(start: Step) {
        _step = State(initialValue: start)
    }

    var body: some View {
        ZStack {
            BackdropGradient()
            VStack(spacing: 0) {
                if step != .welcome {
                    progress
                        .padding(.top, 12)
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
        }
        .animation(.snappy(duration: 0.4), value: step)
        .onChange(of: app.phase) { _, phase in
            busy = false
            switch phase {
            case .signIn: step = .signIn
            case .ready: step = .notifications
            default: break
            }
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
            Image("Logo")
                .resizable()
                .scaledToFit()
                .frame(width: 96, height: 96)
                .clipShape(.rect(cornerRadius: 22))
                .shadow(color: .black.opacity(0.2), radius: 18, y: 8)
            VStack(spacing: 10) {
                Text("HQC").font(.largeTitle.weight(.bold))
                Text("Your coding agents, on every machine, in your pocket.")
                    .font(.title3)
                    .foregroundStyle(palette.text2)
                    .multilineTextAlignment(.center)
            }
            VStack(alignment: .leading, spacing: 18) {
                feature("desktopcomputer", "Every machine, one place",
                        "Sessions on your Macs and servers, live, with what each agent is doing.")
                feature("hand.raised", "Answer from anywhere",
                        "Allow a command or answer a question right from the notification.")
                feature("person.3", "Agents that work together",
                        "Rooms, schedules and sessions that talk to each other.")
            }
            .padding(.vertical, 8)
            primary("Get started") { step = .hub }
        }
    }

    private func feature(_ icon: String, _ title: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(text).font(.subheadline).foregroundStyle(palette.text2)
            }
        }
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
            primary(busy ? "Connecting…" : "Continue", disabled: AppModel.normalize(address) == nil || busy) {
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
                   "Permission requests, questions and finished answers, with Allow and Deny right in the notification.")
            primary("Turn on notifications") {
                Task {
                    await AppDelegate.requestPush()
                    app.finishOnboarding()
                }
            }
            Button("Not now") { app.finishOnboarding() }
                .font(.subheadline)
        }
    }

    // MARK: parts

    private func header(_ icon: String, _ title: String, _ text: String) -> some View {
        VStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(.tint)
            Text(title).font(.title.weight(.bold)).multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text(text)
                .font(.body)
                .foregroundStyle(palette.text2)
                .multilineTextAlignment(.center)
        }
    }

    private func primary(_ title: String, disabled: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
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
