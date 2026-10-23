import SwiftUI
import HolonClient

/// Android-parity single-screen connection flow: enter or paste an address,
/// press 下一步, and the host's sign-in method is probed automatically.
/// Organization sign-in presents itself; token hosts show a token field.
/// Data-sharing consent gates the login flow itself: the host-aware prompt
/// (`LoginConsentSheet`) must be approved before any connection activity,
/// and restored sessions re-review consent at the full-screen gate.
struct ConnectionWelcomeView: View {
    @Bindable var coordinator: ConnectionCoordinator
    var cancel: (() -> Void)? = nil
    @State private var step = Step.address
    @State private var scanning = false
    @State private var address = ""
    @State private var name = ""
    @State private var allowHTTP = false
    @State private var token = ""
    @State private var inputError = false
    @State private var pairingAttempt = false
    @State private var organizationLoginShown = false
    @State private var anchor: UIWindow?
    @State private var consentRequest: LoginConsentRequest?
    @FocusState private var addressFocused: Bool

    private enum Step { case address, authentication, pairing }

    var body: some View {
        NavigationStack {
            Group {
                if step == .address {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 24) {
                            addressEntry
                        }
                        .padding()
                    }
                    .background(Color(uiColor: .systemGroupedBackground))
                } else {
                    Form {
                        switch step {
                        case .address: EmptyView()
                        case .authentication: authentication
                        case .pairing: pairing
                        }
                    }
                }
            }
            .navigationTitle("onboarding.title")
            .navigationBarTitleDisplayMode(.inline)
            .background(WindowAnchorReader { anchor = $0 }.frame(width: 0, height: 0))
            .toolbar {
                if let cancel {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("onboarding.returnConnection", action: cancel)
                            .accessibilityIdentifier("onboarding.cancel")
                    }
                }
                if step != .address {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("action.back") { back() }
                            .accessibilityIdentifier("onboarding.back")
                    }
                }
            }
            .sheet(isPresented: $scanning) {
                NavigationStack {
                    QRCodeScannerView { payload in
                        scanning = false
                        applyScan(payload)
                    }
                    .navigationTitle("onboarding.scan")
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("action.cancel") { scanning = false }
                        }
                    }
                }
            }
            .alert("input.error", isPresented: $inputError) {
                Button("action.ok", role: .cancel) {}
            } message: { Text("onboarding.invalidInput") }
            .sheet(item: $consentRequest) { request in
                LoginConsentSheet(apiBaseURL: request.apiBaseURL) {
                    let action = request.action
                    consentRequest = nil
                    perform(action)
                }
            }
            .onChange(of: address) { allowHTTP = false }
            .onChange(of: step) { attemptAutoOrganizationLogin() }
            .onChange(of: coordinator.status) { attemptAutoOrganizationLogin() }
            .onChange(of: coordinator.authenticationMode) { attemptAutoOrganizationLogin() }
            .onAppear { organizationLoginShown = false }
            .onDisappear {
                token = ""
                coordinator.cancelPairing()
            }
        }
    }

    private var addressEntry: some View {
        Group {
            // Mirrors the audited welcome layout: ScrollView cards, no Form rows.
            VStack(alignment: .leading, spacing: 16) {
                Image(systemName: "server.rack").font(.largeTitle)
                    .foregroundStyle(Color.accentColor).accessibilityHidden(true)
                LocalizedMultilineText(key: "onboarding.welcome", isHeading: true)
                LocalizedMultilineText(key: "onboarding.purpose")
                TextField("profiles.address", text: $address)
                    .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    .submitLabel(.next).onSubmit { connectAddress() }
                    .focused($addressFocused)
                    .accessibilityIdentifier("onboarding.address")
                Text("onboarding.addressExample").font(.subheadline).foregroundStyle(.secondary)
                Button {
                    addressFocused = false
                    pairingAttempt = false
                    scanning = true
                } label: {
                    actionLabel("onboarding.scan", image: "qrcode.viewfinder")
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("onboarding.scan")
                DisclosureGroup("onboarding.advanced") {
                    TextField("profiles.name", text: $name)
                    Text("profiles.addressHelp").font(.subheadline).foregroundStyle(.secondary)
                    Text("onboarding.noLocalhost").font(.subheadline).foregroundStyle(.secondary)
                }
                if insecureAddressNeedsConfirmation {
                    Toggle("connection.allowHTTP", isOn: $allowHTTP)
                        .accessibilityIdentifier("onboarding.allowHTTP")
                    Text("connection.httpWarning").font(.subheadline).foregroundStyle(.secondary)
                }
                Button { connectAddress() } label: { actionLabel("onboarding.checkAddress") }
                    .buttonStyle(.borderedProminent)
                    .disabled(coordinator.isBusy || !canAdvanceAddress)
                    .accessibilityIdentifier("onboarding.checkAddress")
                Text("onboarding.methodNote").font(.subheadline).foregroundStyle(.secondary)
            }
            .disabled(coordinator.isBusy)
            if !coordinator.profiles.isEmpty {
                VStack(alignment: .leading, spacing: 12) {
                    Text("profiles.title").font(.headline)
                    ForEach(coordinator.profiles) { profile in
                        SavedNetworkRow(coordinator: coordinator, profile: profile) {
                            connectSaved(profile)
                        }
                    }
                    if coordinator.selectedProfile != nil {
                        ConnectionStatusView(coordinator: coordinator)
                    }
                }
                .padding().background(Color(uiColor: .secondarySystemGroupedBackground),
                                      in: RoundedRectangle(cornerRadius: 16))
            }
            VStack(alignment: .leading, spacing: 20) {
                DisclosureGroup("onboarding.getCode") {
                    Text("onboarding.getCodeHelp").padding(.top, 8)
                    Text("connection.localNetworkHelp").padding(.top, 8)
                }
                DisclosureGroup("settings.title") { ConnectionSettings(showsHeading: false) }
            }
            .font(.subheadline).foregroundStyle(.secondary)
        }
    }

    private func actionLabel(_ key: LocalizedStringKey, image: String? = nil) -> some View {
        Group {
            if let image { Label(key, systemImage: image) }
            else { Text(key) }
        }
        .frame(maxWidth: .infinity, minHeight: 32)
        .multilineTextAlignment(.center)
    }

    private var authentication: some View {
        Section("onboarding.loginStep") {
            ConnectionStatusView(coordinator: coordinator)
            if pairingAttempt && !coordinator.isBusy {
                Text("onboarding.newCode")
                Button("onboarding.scan") { back(); scanning = true }
            } else if coordinator.authenticationMode == "oidc" {
                if coordinator.supportsOIDC {
                    Text("login.organizationHint")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("login.organization") {
                        guard let anchor else { return }
                        token = ""
                        coordinator.startOrganizationLogin(anchor: anchor)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(coordinator.isBusy || anchor == nil)
                    .accessibilityIdentifier("login.organization")
                } else {
                    Text("onboarding.oidcHTTPS")
                }
            } else if coordinator.authenticationMode == "local" {
                SecureField("login.token", text: $token)
                    .textInputAutocapitalization(.never).autocorrectionDisabled().privacySensitive()
                    .accessibilityIdentifier("onboarding.token")
                Text("onboarding.tokenHelp").font(.caption).foregroundStyle(.secondary)
                Button {
                    let submitted = token
                    token = ""
                    Task { await coordinator.login(token: submitted) }
                } label: { actionLabel("login.tokenSubmit") }
                    .buttonStyle(.borderedProminent)
                    .disabled(coordinator.isBusy || token.isEmpty)
                    .accessibilityIdentifier("onboarding.login")
            }
            if coordinator.isBusy {
                Button("login.cancel") { coordinator.cancelLogin() }
            } else {
                Button("connection.refresh") {
                    token = ""
                    pairingAttempt = false
                    Task { await coordinator.refresh() }
                }.disabled(coordinator.selectedProfile == nil)
                Button("onboarding.otherConnection") { back() }
            }
        }
    }

    private var pairing: some View {
        Section("pairing.title") {
            if let invitation = coordinator.pendingPairing {
                Text("pairing.confirmHelp")
                Text(verbatim: coordinator.pairingTargetURL?.absoluteString ?? invitation.apiBaseURL.absoluteString)
                    .textSelection(.enabled).accessibilityIdentifier("onboarding.pairingTarget")
                if invitation.apiBaseURL.scheme == "http" {
                    Toggle("connection.allowHTTP", isOn: $allowHTTP)
                        .accessibilityIdentifier("onboarding.pairingHTTP")
                    Text("connection.httpWarning").font(.caption)
                }
                Button {
                    let target = coordinator.pairingTargetURL ?? invitation.apiBaseURL
                    beginConnection(.pairing(allowInsecureHTTP: allowHTTP), url: target)
                } label: { actionLabel("pairing.confirm") }
                    .buttonStyle(.borderedProminent)
                    .disabled(coordinator.isBusy ||
                              (invitation.apiBaseURL.scheme == "http" && !allowHTTP))
                    .accessibilityIdentifier("onboarding.confirmPairing")
                Button("pairing.cancel", role: .cancel) { back() }
            }
        }
    }

    /// Organization sign-in presents itself once when the probe lands on OIDC,
    /// mirroring Android's automatic browser open. The button remains as retry.
    private func attemptAutoOrganizationLogin() {
        guard step == .authentication, !organizationLoginShown, !pairingAttempt,
              coordinator.canAutoPresentOrganizationLogin, let anchor else { return }
        organizationLoginShown = true
        coordinator.startOrganizationLogin(anchor: anchor)
    }

    private func connectSaved(_ profile: ConnectionProfile) {
        beginConnection(.saved(profile), url: profile.apiBaseURL)
    }

    /// A pairing payload confirms HTTP later on its own offline confirmation
    /// step (`onboarding.pairingHTTP`); only plain addresses gate 下一步 here.
    private var enteredPairingPayload: Bool {
        guard let input = try? ConnectionInput(address) else { return false }
        if case .pairing = input { return true }
        return false
    }

    private var insecureAddressNeedsConfirmation: Bool {
        URL(string: address)?.scheme?.lowercased() == "http" && !enteredPairingPayload
    }

    private var canAdvanceAddress: Bool {
        guard !address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        return !insecureAddressNeedsConfirmation || allowHTTP
    }

    /// Scanning is offline: an address code only fills the field for review;
    /// a pairing code opens the offline confirmation step.
    private func applyScan(_ payload: String) {
        guard !coordinator.isBusy else { return }
        token = ""
        allowHTTP = false
        coordinator.cancelPairing()
        do {
            switch try ConnectionInput(payload) {
            case .pairing:
                try coordinator.previewPairing(payload)
                step = .pairing
            case .address(let url):
                address = url.absoluteString
                pairingAttempt = false
            }
        } catch { inputError = true }
    }

    /// 下一步 owns the transition: a pairing link opens offline confirmation;
    /// an address asks for data-sharing consent first, and only an approved
    /// host is probed for its sign-in method.
    private func connectAddress() {
        addressFocused = false
        guard !coordinator.isBusy else { return }
        do {
            switch try ConnectionInput(address) {
            case .pairing:
                coordinator.cancelPairing()
                try coordinator.previewPairing(address)
                // A consumed ticket is not an editable address; the field
                // restarts clean when the user backs out of confirmation.
                address = ""
                step = .pairing
            case .address(let url):
                let chosenName = name.trimmingCharacters(in: .whitespacesAndNewlines)
                let resolvedName = chosenName.isEmpty ? url.host ?? "Holon" : chosenName
                beginConnection(.address(name: resolvedName, apiBaseURL: url, allowInsecureHTTP: allowHTTP),
                                url: url)
            }
        } catch { inputError = true }
    }

    /// Every connection start funnels through here: approved hosts connect
    /// straight through, new hosts see the host-aware sharing prompt first.
    private func beginConnection(_ action: LoginConsentRequest.Action, url: URL) {
        if SharingConsent.shared.approved(url) {
            perform(action)
        } else {
            consentRequest = LoginConsentRequest(apiBaseURL: url, action: action)
        }
    }

    private func perform(_ action: LoginConsentRequest.Action) {
        switch action {
        case let .address(name, url, allowInsecureHTTP):
            do {
                let profile = try coordinator.addProfile(name: name, apiBaseURL: url,
                                                         allowInsecureHTTP: allowInsecureHTTP)
                token = ""
                pairingAttempt = false
                organizationLoginShown = false
                step = .authentication
                Task { await coordinator.connect(profile) }
            } catch { inputError = true }
        case let .saved(profile):
            token = ""
            pairingAttempt = false
            organizationLoginShown = false
            step = .authentication
            Task { await coordinator.connect(profile) }
        case let .pairing(allowInsecureHTTP):
            pairingAttempt = true
            step = .authentication
            Task { await coordinator.confirmPairing(allowInsecureHTTP: allowInsecureHTTP) }
        }
    }

    private func back() {
        token = ""
        if coordinator.isBusy { coordinator.cancelLogin() }
        coordinator.cancelPairing()
        allowHTTP = false
        pairingAttempt = false
        organizationLoginShown = false
        step = .address
    }
}

/// A connection start held until data-sharing consent is approved.
private struct LoginConsentRequest: Identifiable {
    let id = UUID()
    let apiBaseURL: URL
    let action: Action

    enum Action {
        case address(name: String, apiBaseURL: URL, allowInsecureHTTP: Bool)
        case saved(ConnectionProfile)
        case pairing(allowInsecureHTTP: Bool)
    }
}

/// Host-aware data-sharing prompt that blocks the login flow itself: nothing
/// is sent to the host until the user explicitly agrees. Declining (cancel
/// or swipe) returns without a profile or a probe. Presented by onboarding
/// and by ContentView before stored-profile reconnects.
struct LoginConsentSheet: View {
    let apiBaseURL: URL
    let onApproved: () -> Void
    @State private var agreed = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                LoginSharingConsentSection(apiBaseURL: apiBaseURL, agreed: $agreed)
                Button("privacy.continueLogin") {
                    SharingConsent.shared.approve(apiBaseURL)
                    guard SharingConsent.shared.approved(apiBaseURL) else { return }
                    onApproved()
                }
                .disabled(!agreed || SharingConsent.shared.defaults == nil)
                .accessibilityIdentifier("privacy.continueLogin")
            }
            .navigationTitle("privacy.title")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("action.cancel") { dismiss() }
                        .accessibilityIdentifier("privacy.cancelLogin")
                }
            }
        }
    }
}

struct ConnectionStatusView: View {
    @Bindable var coordinator: ConnectionCoordinator

    var body: some View {
        if let profile = coordinator.selectedProfile {
            Text(verbatim: profile.name).font(.headline)
            Text(verbatim: profile.apiBaseURL.absoluteString).font(.caption).foregroundStyle(.secondary)
        }
        Text(LocalizedStringKey("status." + coordinator.status.rawValue))
            .accessibilityIdentifier("connection.status")
        if coordinator.isBusy {
            ProgressView().accessibilityLabel(Text("connection.working"))
        } else {
            Text(LocalizedStringKey("onboarding.recovery." + coordinator.status.rawValue))
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}
