import UIKit
import WebKit

/// One disposable identity lease: a fresh non-persistent data store per open,
/// discarded with the last referencing web view (#3603). It never persists a
/// page, profile, or credential. The unit-test host cannot run WebKit
/// networking (probe evidence, 2026-10-19), so this layer stays thin and every
/// decision routes through `RuntimeBrowserPolicy`.
@MainActor
final class RuntimeWebPage: NSObject {
    private let baseURL: String
    private let isCurrent: () -> Bool
    private let bindCookie: (@escaping (String?) -> Void) -> Void
    private let onExternal: (String) -> Void
    private let onAuthenticationFailure: () -> Void
    private let onChanged: () -> Void

    private(set) var error: String?
    private(set) var loading = true
    private(set) var title = ""
    private(set) var url: String

    let webView: WKWebView
    private var closed = false
    private var titleObservation: NSKeyValueObservation?

    init(initialURL: String,
         baseURL: String,
         isCurrent: @escaping () -> Bool,
         bindCookie: @escaping (@escaping (String?) -> Void) -> Void,
         onExternal: @escaping (String) -> Void,
         onAuthenticationFailure: @escaping () -> Void,
         onChanged: @escaping () -> Void = {}) {
        self.baseURL = baseURL
        self.isCurrent = isCurrent
        self.bindCookie = bindCookie
        self.onExternal = onExternal
        self.onAuthenticationFailure = onAuthenticationFailure
        self.onChanged = onChanged
        url = initialURL

        // The store must be attached at configuration time; assigning it after
        // WKWebView creation has no effect (macOS probe evidence, 2026-10-19).
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = WKWebsiteDataStore.nonPersistent()
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        webView.navigationDelegate = self
        webView.uiDelegate = self
        titleObservation = webView.observe(\.title, options: [.initial, .new]) { [weak self] _, _ in
            Task { @MainActor [weak self] in
                guard let self, self.valid(), let value = self.webView.title, value != self.title else { return }
                self.title = value
                self.onChanged()
            }
        }
        // The page only loads after the session cookie is bound to this store.
        bindCookie { [weak self] header in
            guard let self, self.valid() else { return }
            guard let header,
                  let cookie = RuntimeBrowserPolicy.sessionCookie(header: header, url: initialURL) else {
                self.fail(String(localized: "browser.cookieBindFailed"))
                return
            }
            self.webView.configuration.websiteDataStore.httpCookieStore.setCookie(cookie) { [weak self] in
                guard let self, self.valid() else { return }
                self.loadTarget(initialURL)
            }
        }
    }

    private func valid() -> Bool { !closed && isCurrent() }

    private func loadTarget(_ target: String) {
        guard valid(), let requestURL = RuntimeBrowserPolicy.safeBrowserHTTPURL(target) else { return }
        error = nil
        loading = true
        url = target
        onChanged()
        webView.load(URLRequest(url: requestURL, cachePolicy: .reloadIgnoringLocalCacheData))
    }

    private func fail(_ message: String) {
        error = message
        loading = false
        onChanged()
    }

    private func route(_ target: String) -> WKNavigationActionPolicy {
        switch RuntimeBrowserPolicy.navigation(target, base: baseURL) {
        case .internalWeb: return .allow
        case .externalWeb:
            onExternal(target)
            return .cancel
        case .blocked:
            fail(String(localized: "browser.navigationBlocked"))
            return .cancel
        }
    }

    @discardableResult
    func back() -> Bool {
        guard valid(), webView.canGoBack else { return false }
        webView.goBack()
        return true
    }

    /// Never replay a POST/history entry: retry is an explicit fresh GET of the current URL.
    func reload() {
        loadTarget(url)
    }

    func close() {
        guard !closed else { return }
        closed = true
        titleObservation?.invalidate()
        titleObservation = nil
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
        // The non-persistent data store disappears with the last strong reference.
    }
}

extension RuntimeWebPage: WKNavigationDelegate {
    func webView(_ webView: WKWebView,
                 decidePolicyFor navigationAction: WKNavigationAction,
                 preferences: WKWebpagePreferences) async -> (WKNavigationActionPolicy, WKWebpagePreferences) {
        guard valid() else { return (.cancel, preferences) }
        let target = navigationAction.request.url?.absoluteString ?? ""
        if navigationAction.shouldPerformDownload {
            fail(String(localized: "browser.downloadUnsupported"))
            return (.cancel, preferences)
        }
        guard navigationAction.targetFrame?.isMainFrame == true else {
            // Subframes may only follow the runtime origin (defense in depth, not a
            // replacement for the runtime's CSP or browser origin policy).
            return (RuntimeBrowserPolicy.sameRuntimeOrigin(target, base: baseURL) ? .allow : .cancel, preferences)
        }
        if navigationAction.navigationType == .formResubmitted {
            fail(String(localized: "browser.formResubmissionBlocked"))
            return (.cancel, preferences)
        }
        return (route(target), preferences)
    }

    func webView(_ webView: WKWebView,
                 decidePolicyFor navigationResponse: WKNavigationResponse) async -> WKNavigationResponsePolicy {
        guard valid(), navigationResponse.isForMainFrame,
              let http = navigationResponse.response as? HTTPURLResponse else { return .allow }
        if !navigationResponse.canShowMIMEType {
            fail(String(localized: "browser.downloadUnsupported"))
            return .cancel
        }
        switch http.statusCode {
        case 200..<400:
            return .allow
        case 401:
            fail(String(localized: "browser.sessionExpired"))
            onAuthenticationFailure()
        case 403:
            fail(String(localized: "browser.forbidden"))
        case 404:
            fail(String(localized: "browser.notFound"))
        default:
            fail(String(format: String(localized: "browser.httpError"), http.statusCode))
        }
        // Like Android, the status becomes an error message while the response body still renders.
        return .allow
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation?) {
        guard valid(), let target = webView.url?.absoluteString,
              RuntimeBrowserPolicy.sameRuntimeOrigin(target, base: baseURL) else { return }
        url = target
        error = nil
        loading = true
        onChanged()
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation?) {
        guard valid() else { return }
        loading = false
        onChanged()
    }

    private func navigationFailureMessage(_ failure: Error) -> String? {
        guard let urlError = failure as? URLError else { return String(localized: "browser.loadFailed") }
        if urlError.code == .cancelled { return nil }
        if urlError.code == .secureConnectionFailed || urlError.code == .serverCertificateUntrusted {
            return String(localized: "browser.tlsFailed")
        }
        // NSURLErrorServerCertificate* (-1205...-1200) are not all exposed as URLError.Code cases.
        if (-1205...(-1200)).contains((failure as NSError).code) {
            return String(localized: "browser.tlsFailed")
        }
        return String(localized: "browser.loadFailed")
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation?, withError failure: Error) {
        guard valid(), let message = navigationFailureMessage(failure) else { return }
        fail(message)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation?, withError failure: Error) {
        guard valid(), let message = navigationFailureMessage(failure) else { return }
        fail(message)
    }
}

extension RuntimeWebPage: WKUIDelegate {
    /// iOS hands the would-be window's request directly to the delegate, so the
    /// destination is routed immediately and no secondary web view is created.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard valid(), navigationAction.navigationType == .linkActivated else { return nil }
        route(navigationAction.request.url?.absoluteString ?? "")
        return nil
    }
}
