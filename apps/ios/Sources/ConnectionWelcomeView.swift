import SwiftUI
import HolonClient

struct ConnectionWelcomeView: View {
    @Bindable var coordinator: ConnectionCoordinator
    var cancel: (() -> Void)? = nil
    @State private var step = Step.welcome
    @State private var scanning = false
    @State private var address = ""
    @State private var name = ""
    @State private var allowHTTP = false
    @State private var token = ""
    @State private var payload = ""
    @State private var inputError = false
    @State private var addressOnly = false
    @State private var pairingAttempt = false
    @State private var anchor: UIWindow?
    @FocusState private var payloadFocused: Bool

    private enum Step { case welcome, address, authentication, pairing }

    var body: some View {
        NavigationStack {
            Group {
                if step == .welcome {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 24) {
                            welcome
                            ConnectionSettings()
                        }
                        .padding()
                    }
                } else {
                    Form {
                        switch step {
                        case .welcome: EmptyView()
                        case .address: manualAddress
                        case .authentication: authentication
                        case .pairing: pairing
                        }
                        ConnectionSettings()
                    }
                }
            }
            .navigationTitle("onboarding.title")
            .background(WindowAnchorReader { anchor = $0 }.frame(width: 0, height: 0))
            .toolbar {
                if let cancel {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("onboarding.returnConnection", action: cancel)
                            .accessibilityIdentifier("onboarding.cancel")
                    }
                }
                if step != .welcome {
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
                        receive(payload)
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
            .onChange(of: address) { allowHTTP = false }
            .onDisappear {
                token = ""
                payload = ""
                coordinator.cancelPairing()
            }
        }
    }

    private var welcome: some View {
        Group {
            Section {
                VStack(alignment: .leading, spacing: 12) {
                    Image(systemName: "network").font(.largeTitle).accessibilityHidden(true)
                    Text("onboarding.welcome").font(.title2.bold())
                    Text("onboarding.purpose").foregroundStyle(.secondary)
                    Button("onboarding.scan", systemImage: "qrcode.viewfinder") {
                        pairingAttempt = false
                        scanning = true
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("onboarding.scan")
                    Button("onboarding.manual", systemImage: "keyboard") {
                        token = ""
                        addressOnly = false
                        pairingAttempt = false
                        step = .address
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("onboarding.manual")
                    PasteButton(payloadType: String.self) { values in
                        if let value = values.first { receive(value) }
                    }
                    .accessibilityLabel(Text("onboarding.paste"))
                }
            }
            Section {
                DisclosureGroup {
                    SecureField("pairing.payload", text: $payload)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .privacySensitive().focused($payloadFocused)
                        .submitLabel(.done)
                        .onSubmit { payloadFocused = false }
                        .accessibilityIdentifier("onboarding.payload")
                    Button("pairing.preview") {
                        let submitted = payload
                        payload = ""
                        payloadFocused = false
                        receive(submitted)
                    }
                    .disabled(coordinator.isBusy || payload.isEmpty)
                    .accessibilityIdentifier("onboarding.preview")
                } label: {
                    Text("onboarding.paste")
                        .accessibilityIdentifier("onboarding.pasteEntry")
                }
                DisclosureGroup("onboarding.getCode") {
                    Text("onboarding.getCodeHelp")
                    Text("connection.localNetworkHelp")
                }
                .font(.body)
            }
            if !coordinator.profiles.isEmpty {
                Section("profiles.title") {
                    ForEach(coordinator.profiles) { profile in
                        Button {
                            token = ""
                            pairingAttempt = false
                            step = .authentication
                            Task { await coordinator.connect(profile) }
                        } label: {
                            VStack(alignment: .leading) {
                                Text(verbatim: profile.name)
                                Text(verbatim: profile.apiBaseURL.absoluteString)
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .disabled(coordinator.isBusy)
                    }
                    if coordinator.selectedProfile != nil {
                        ConnectionStatusView(coordinator: coordinator)
                        Button("onboarding.continueLogin") { step = .authentication }
                            .disabled(coordinator.isBusy)
                    }
                }
            }
        }
    }

    private var manualAddress: some View {
        Section("onboarding.addressStep") {
            if addressOnly { Text("onboarding.addressOnly") }
            TextField("profiles.address", text: $address)
                .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("onboarding.address")
            Text("profiles.addressHelp").font(.caption).foregroundStyle(.secondary)
            Text("onboarding.noLocalhost").font(.caption).foregroundStyle(.secondary)
            DisclosureGroup("onboarding.advanced") {
                TextField("profiles.name", text: $name)
            }
            if URL(string: address)?.scheme?.lowercased() == "http" {
                Toggle("connection.allowHTTP", isOn: $allowHTTP)
                    .accessibilityIdentifier("onboarding.allowHTTP")
                Text("connection.httpWarning").font(.caption)
            }
            Button("onboarding.checkAddress") { connectAddress() }
                .disabled(coordinator.isBusy || address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
                          (URL(string: address)?.scheme?.lowercased() == "http" && !allowHTTP))
                .accessibilityIdentifier("onboarding.checkAddress")
        }
    }

    private var authentication: some View {
        Group {
            Section("onboarding.loginStep") {
                ConnectionStatusView(coordinator: coordinator)
                if pairingAttempt && !coordinator.isBusy {
                    Text("onboarding.newCode")
                    Button("onboarding.scan") { back(); scanning = true }
                } else if coordinator.authenticationMode == "oidc" {
                    if coordinator.supportsOIDC {
                        Button("login.organization") {
                            guard let anchor else { return }
                            token = ""
                            coordinator.startOrganizationLogin(anchor: anchor)
                        }
                        .disabled(coordinator.isBusy || anchor == nil)
                    } else {
                        Text("onboarding.oidcHTTPS")
                    }
                } else if coordinator.authenticationMode == "local" {
                    SecureField("login.token", text: $token)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().privacySensitive()
                        .accessibilityIdentifier("onboarding.token")
                    Text("onboarding.tokenHelp").font(.caption).foregroundStyle(.secondary)
                    Button("login.tokenSubmit") {
                        let submitted = token
                        token = ""
                        Task { await coordinator.login(token: submitted) }
                    }
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
    }

    private var pairing: some View {
        Section("pairing.title") {
            if let invitation = coordinator.pendingPairing {
                Text("pairing.confirmHelp")
                Text(verbatim: invitation.apiBaseURL.absoluteString)
                    .textSelection(.enabled).accessibilityIdentifier("onboarding.pairingTarget")
                if invitation.apiBaseURL.scheme == "http" {
                    Toggle("connection.allowHTTP", isOn: $allowHTTP)
                        .accessibilityIdentifier("onboarding.pairingHTTP")
                    Text("connection.httpWarning").font(.caption)
                }
                Button("pairing.confirm") {
                    pairingAttempt = true
                    step = .authentication
                    Task { await coordinator.confirmPairing(allowInsecureHTTP: allowHTTP) }
                }
                .disabled(coordinator.isBusy || (invitation.apiBaseURL.scheme == "http" && !allowHTTP))
                .accessibilityIdentifier("onboarding.confirmPairing")
                Button("pairing.cancel", role: .cancel) { back() }
            }
        }
    }

    private func receive(_ payload: String) {
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
                addressOnly = true
                pairingAttempt = false
                step = .address
            }
        } catch { inputError = true }
    }

    private func connectAddress() {
        do {
            guard case .address(let url) = try ConnectionInput(address) else {
                throw HolonClientError.invalidRequest
            }
            let chosenName = name.trimmingCharacters(in: .whitespacesAndNewlines)
            let profile = try coordinator.addProfile(name: chosenName.isEmpty ? url.host ?? "Holon" : chosenName,
                                                     apiBaseURL: url, allowInsecureHTTP: allowHTTP)
            token = ""
            step = .authentication
            Task { await coordinator.connect(profile) }
        } catch { inputError = true }
    }

    private func back() {
        token = ""
        payload = ""
        if coordinator.isBusy { coordinator.cancelLogin() }
        coordinator.cancelPairing()
        allowHTTP = false
        pairingAttempt = false
        step = .welcome
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
