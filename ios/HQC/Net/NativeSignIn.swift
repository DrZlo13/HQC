import AuthenticationServices
import CryptoKit
import Foundation
import UIKit

/// Sign-in through the system browser (SPEC §6 "Native client sign-in"): Cloudflare Access
/// runs there with the person's own browser session, so Google and other providers that refuse
/// embedded web views work. The hub hands back a one-time code, which the app exchanges with
/// a verifier only it holds for the Access token.
@MainActor
enum NativeSignIn {
    enum Failure: LocalizedError {
        case cancelled
        case badCallback
        case hubTooOld

        var errorDescription: String? {
            switch self {
            case .cancelled: "Sign-in was cancelled."
            case .badCallback: "The hub's answer to the sign-in did not match. Try again."
            case .hubTooOld: "This hub is older than the app: update it to sign in (SPEC 0.215)."
            }
        }
    }

    static func run(hub: URL) async throws -> String {
        let verifier = random(48)
        let challenge = Data(SHA256.hash(data: Data(verifier.utf8))).base64URL
        let state = random(16)
        var c = URLComponents(url: hub, resolvingAgainstBaseURL: false)!
        c.path = "/api/native/login"
        c.queryItems = [URLQueryItem(name: "challenge", value: challenge), URLQueryItem(name: "state", value: state)]
        guard let url = c.url else { throw Failure.badCallback }

        let callback: URL = try await withCheckedThrowingContinuation { cont in
            let session = ASWebAuthenticationSession(url: url, callback: .customScheme("hqc-app")) { url, error in
                if let url { cont.resume(returning: url) } else if let e = error as? ASWebAuthenticationSessionError,
                    e.code == .canceledLogin {
                    cont.resume(throwing: Failure.cancelled)
                } else {
                    cont.resume(throwing: error ?? Failure.badCallback)
                }
            }
            // Keep the browser's Access session: the next sign-in is one tap.
            session.prefersEphemeralWebBrowserSession = false
            session.presentationContextProvider = Anchor.shared
            if !session.start() { cont.resume(throwing: Failure.badCallback) }
        }

        let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
        guard items.first(where: { $0.name == "state" })?.value == state,
              let code = items.first(where: { $0.name == "code" })?.value else {
            throw Failure.badCallback
        }
        let client = HubClient(endpoint: HubEndpoint(baseURL: hub, accessToken: nil))
        do {
            return try await client.nativeToken(code: code, verifier: verifier)
        } catch HubError.http(let status, _, _) where status == 404 || status == 405 {
            throw Failure.hubTooOld
        }
    }

    /// URL-safe random text of `bytes` random bytes.
    private static func random(_ bytes: Int) -> String {
        var b = [UInt8](repeating: 0, count: bytes)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes, &b)
        return Data(b).base64URL
    }

    private final class Anchor: NSObject, ASWebAuthenticationPresentationContextProviding {
        static let shared = Anchor()

        func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
            MainActor.assumeIsolated {
                let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
                if let key = scenes.flatMap(\.windows).first(where: \.isKeyWindow) { return key }
                return ASPresentationAnchor(windowScene: scenes[0])
            }
        }
    }
}

extension Data {
    var base64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
