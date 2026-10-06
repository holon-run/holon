import AuthenticationServices
import UIKit

/// Preparation is testable without presenting UI; the login coordinator owns start/cancel.
@MainActor
final class NativeBrowserAuthentication: NSObject, ASWebAuthenticationPresentationContextProviding {
    enum Failure: Error {
        case alreadyPrepared
        case cancelled
        case browserFailure
        case presentationFailed
    }

    private let anchor: ASPresentationAnchor
    private var attemptID: UUID?
    private var started = false
    private var completion: (@MainActor @Sendable (Result<String, Error>) -> Void)?
    private(set) var session: ASWebAuthenticationSession?

    init(anchor: ASPresentationAnchor) {
        self.anchor = anchor
        super.init()
    }

    func prepare(
        proof: NativeLoginProof,
        completion: @escaping @MainActor @Sendable (Result<String, Error>) -> Void
    ) throws {
        guard session == nil else { throw Failure.alreadyPrepared }
        guard proof.isValid(at: Date()) else { throw NativeLoginProof.Failure.invalidProof }
        let url = try proof.startURL()
        let id = UUID()
        let session = ASWebAuthenticationSession(url: url, callback: .customScheme(NativeLoginProof.callbackScheme)) {
            [weak self] callback, error in
            Task { @MainActor in
                let result: Result<String, Error>
                if let callback {
                    result = Result { try proof.ticket(from: callback) }
                } else if (error as? ASWebAuthenticationSessionError)?.code == .canceledLogin {
                    result = .failure(Failure.cancelled)
                } else {
                    result = .failure(Failure.browserFailure)
                }
                self?.finish(id: id, result: result)
            }
        }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = true
        attemptID = id
        self.completion = completion
        self.session = session
    }

    @discardableResult
    func start() -> Bool {
        guard !started, let session, let id = attemptID else { return false }
        started = true
        guard session.start() else {
            finish(id: id, result: .failure(Failure.presentationFailed))
            return false
        }
        return true
    }

    func cancel() {
        guard let session, let id = attemptID else { return }
        finish(id: id, result: .failure(Failure.cancelled))
        session.cancel()
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        anchor
    }

    private func finish(id: UUID, result: Result<String, Error>) {
        guard attemptID == id else { return }
        let completion = completion
        attemptID = nil
        started = false
        session = nil
        self.completion = nil
        completion?(result)
    }
}
