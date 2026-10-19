import SwiftUI
import WebKit

private struct RuntimeBrowserTarget: Identifiable, Equatable {
    let url: String
    let lease: RuntimeBrowserLease
    var id: String { url }
}

/// App-wide provider of the runtime web opener (#3603): classified web links
/// from #3598 rendering enter here and only here. The href is re-validated at
/// open time; a stale lease never resurrects a page after A→B→A switching.
struct RuntimeBrowserHost<Content: View>: View {
    let connection: ConnectionCoordinator
    @ViewBuilder let content: () -> Content

    @State private var target: RuntimeBrowserTarget?
    @State private var externalHref: String?
    @State private var notice: String?
    @Environment(\.openURL) private var openURL

    var body: some View {
        content()
            .environment(\.holonOpenWebLink) { href in open(href) }
            .fullScreenCover(item: $target) { open in
                // The external confirm must be owned by the cover itself: an
                // alert dismissed from the presenting view tears the cover down.
                RuntimeBrowserScreen(target: open, connection: connection,
                                     onClose: { target = nil })
            }
            .onChange(of: connection.identity) { _, _ in retireStaleTarget() }
            .onChange(of: connection.status) { _, _ in retireStaleTarget() }
            .alert("browser.openExternalTitle", isPresented: externalPresented, presenting: externalHref) { href in
                Button("browser.openExternalConfirm") {
                    externalHref = nil
                    if let url = RuntimeBrowserPolicy.safeBrowserHTTPURL(href) { openURL(url) }
                }
                .accessibilityIdentifier("browser.openExternal.confirm")
                Button("action.cancel", role: .cancel) { externalHref = nil }
                .accessibilityIdentifier("browser.openExternal.cancel")
            } message: { href in
                Text("browser.openExternalBody")
                    + Text(verbatim: "\n\(href)")
            }
            .alert("browser.noticeTitle", isPresented: noticePresented, presenting: notice) { _ in
                Button("action.ok", role: .cancel) { notice = nil }
            } message: { text in
                Text(text)
            }
    }

    private var externalPresented: Binding<Bool> {
        Binding(get: { externalHref != nil }, set: { if !$0 { externalHref = nil } })
    }

    private var noticePresented: Binding<Bool> {
        Binding(get: { notice != nil }, set: { if !$0 { notice = nil } })
    }

    /// An identity or status transition retires the open page: the old lease
    /// must not keep a page alive across connection switches.
    private func retireStaleTarget() {
        guard let open = target, !connection.isRuntimeBrowserLeaseCurrent(open.lease) else { return }
        target = nil
    }

    private func open(_ href: String) {
        guard let lease = connection.runtimeBrowserLease(),
              connection.isRuntimeBrowserLeaseCurrent(lease) else {
            notice = String(localized: "browser.leaseChanged")
            return
        }
        switch RuntimeBrowserPolicy.linkTarget(href: href, runtimeBaseURL: lease.baseURL) {
        case .load(let url): target = RuntimeBrowserTarget(url: url, lease: lease)
        case .external(let url): externalHref = url
        case .unsafe: notice = String(localized: "browser.unsafeLink")
        }
    }
}

private struct RuntimeBrowserScreen: View {
    let target: RuntimeBrowserTarget
    let connection: ConnectionCoordinator
    let onClose: () -> Void

    @State private var page: RuntimeWebPage?
    @State private var externalHref: String?
    @State private var retry = 0
    @State private var revision = 0
    @State private var revalidating = false
    @Environment(\.openURL) private var openURL

    private var screenID: String { "\(target.url)#\(retry)" }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button("browser.back") { if page?.back() != true { onClose() } }
                    .accessibilityIdentifier("browser.back")
                Spacer()
                Button("browser.close") { onClose() }
                    .accessibilityIdentifier("browser.close")
                Spacer()
                Button("browser.reload") { page?.reload() }
                    .accessibilityIdentifier("browser.reload")
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            Text((page?.title ?? "").prefix(120))
                .lineLimit(1)
                .padding(.horizontal, 12)
                .accessibilityIdentifier("browser.title")
            Text(identityLine)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .padding(.horizontal, 12)
            HStack {
                Button("browser.copyLink") {
                    UIPasteboard.general.string = page?.url ?? target.url
                }
                .accessibilityIdentifier("browser.copyLink")
                Spacer()
                Button("browser.systemOpen") { externalHref = page?.url ?? target.url }
                .accessibilityIdentifier("browser.systemOpen")
                Spacer()
                Button("browser.revalidate") {
                    revalidating = true
                    Task {
                        defer { revalidating = false }
                        guard await connection.revalidateRuntimeBrowserSession(target.lease),
                              connection.isRuntimeBrowserLeaseCurrent(target.lease) else { return }
                        retry += 1
                    }
                }
                .disabled(revalidating)
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            if let error = page?.error {
                Text(error)
                    .foregroundStyle(.red)
                    .font(.footnote)
                    .padding(.horizontal, 12)
                    .accessibilityIdentifier("browser.error")
            }
            if page?.loading == true {
                ProgressView().frame(maxWidth: .infinity)
                    .accessibilityIdentifier("browser.loading")
            }
            if let page {
                RuntimeWebView(page: page, revision: revision)
                    // Mirror Android's key(page): a new RuntimeWebPage must mount a new
                    // WKWebView, otherwise updateUIView (empty) leaves the slot blank.
                    .id(ObjectIdentifier(page))
                    .ignoresSafeArea(edges: .bottom)
            } else {
                Text("browser.placeholder")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .alert("browser.openExternalTitle", isPresented: externalPresented, presenting: externalHref) { href in
            Button("browser.openExternalConfirm") {
                externalHref = nil
                if let url = RuntimeBrowserPolicy.safeBrowserHTTPURL(href) { openURL(url) }
            }
            .accessibilityIdentifier("browser.openExternal.confirm")
            Button("action.cancel", role: .cancel) { externalHref = nil }
            .accessibilityIdentifier("browser.openExternal.cancel")
        } message: { href in
            Text("browser.openExternalBody")
                + Text(verbatim: "\n\(href)")
        }
        .task(id: screenID) { open() }
        .onDisappear { page?.close(); page = nil }
    }

    private var externalPresented: Binding<Bool> {
        Binding(get: { externalHref != nil }, set: { if !$0 { externalHref = nil } })
    }

    private var identityLine: String {
        [target.lease.profile.name,
         target.lease.baseURL,
         target.lease.identity.userID ?? ""]
            .filter { !$0.isEmpty }
            .joined(separator: "\n")
    }

    private func open() {
        page?.close()
        page = RuntimeWebPage(
            initialURL: target.url,
            baseURL: target.lease.baseURL,
            isCurrent: { connection.isRuntimeBrowserLeaseCurrent(target.lease) },
            bindCookie: { write in
                switch connection.bindRuntimeBrowserCookie(target.lease, url: target.url) {
                case .success(let header): write(header)
                case .failure: write(nil)
                }
            },
            onExternal: { externalHref = $0 },
            onAuthenticationFailure: { connection.runtimeBrowserAuthenticationFailed(target.lease) },
            onChanged: { revision += 1 })
    }
}

private struct RuntimeWebView: UIViewRepresentable {
    let page: RuntimeWebPage
    // Unused on purpose: reading `revision` in body lets page-event bumps via
    // `onChanged` re-render title/error/loading without remounting the web view.
    let revision: Int

    func makeUIView(context: Context) -> WKWebView { page.webView }

    func updateUIView(_ uiView: WKWebView, context: Context) {}
}
