import SwiftUI
import AuthenticationServices

struct ContentView: View {
    @Bindable var coordinator: ConnectionCoordinator
    @State private var name = ""
    @State private var address = ""
    @State private var allowHTTP = false
    @State private var token = ""
    @State private var payload = ""
    @State private var pairingHTTP = false
    @State private var inputError = false
    @State private var anchor: UIWindow?
    @FocusState private var pairingFocused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section("profiles.title") {
                    ForEach(coordinator.profiles) { profile in
                        Button {
                            clearInputs()
                            Task { await coordinator.connect(profile) }
                        } label: {
                            VStack(alignment: .leading) {
                                HStack {
                                    Text(verbatim: profile.name)
                                    if coordinator.selectedProfile?.id == profile.id {
                                        Image(systemName: "checkmark").accessibilityLabel(Text("profiles.selected"))
                                    }
                                }
                                Text(verbatim: profile.apiBaseURL.absoluteString).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .disabled(coordinator.isBusy)
                        .swipeActions {
                            Button("profiles.delete", role: .destructive) {
                                clearInputs()
                                Task { await coordinator.removeProfile(profile) }
                            }.disabled(coordinator.isBusy)
                        }
                    }
                    TextField("profiles.name", text: $name)
                    TextField("profiles.address", text: $address)
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    Text("profiles.addressHelp").font(.caption).foregroundStyle(.secondary)
                    Toggle("connection.allowHTTP", isOn: $allowHTTP)
                        .accessibilityIdentifier("profiles.allowHTTP")
                    Text("connection.httpWarning").font(.caption).foregroundStyle(.secondary)
                    Text("connection.localNetworkHelp").font(.caption).foregroundStyle(.secondary)
                    Button("profiles.add") { addProfile() }
                        .disabled(coordinator.isBusy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || address.isEmpty)
                }
                Section("connection.title") {
                    LabeledContent("connection.status") {
                        Text(LocalizedStringKey("status." + coordinator.status.rawValue))
                    }
                    if coordinator.isBusy { ProgressView().accessibilityLabel(Text("connection.working")) }
                    if let identity = coordinator.identity {
                        identityRow("identity.network", identity.networkID)
                        identityRow("identity.runtime", identity.runtimeID)
                        identityRow("identity.user", identity.userID)
                        identityRow("identity.scope", identity.visibilityScopeID)
                    }
                    SecureField("login.token", text: $token)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .privacySensitive()
                    Button("login.tokenSubmit") {
                        let submitted = token
                        token = ""
                        Task { await coordinator.login(token: submitted) }
                    }.disabled(coordinator.isBusy || coordinator.selectedProfile == nil || token.isEmpty)
                    Button("login.organization") {
                        guard let anchor else { return }
                        token = ""
                        coordinator.startOrganizationLogin(anchor: anchor)
                    }.disabled(coordinator.isBusy || !coordinator.supportsOIDC || anchor == nil)
                    if anchor == nil { Text("login.noWindow").font(.caption) }
                    Button("login.cancel") { coordinator.cancelLogin() }
                    Button("connection.refresh") { Task { await coordinator.refresh() } }
                        .disabled(coordinator.isBusy || coordinator.selectedProfile == nil)
                    Button("login.logout", role: .destructive) {
                        clearInputs()
                        Task { await coordinator.logout() }
                    }.disabled(coordinator.isBusy || coordinator.selectedProfile == nil)
                }
                Section("pairing.title") {
                    TextField("pairing.payload", text: $payload, axis: .vertical)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().privacySensitive()
                        .focused($pairingFocused)
                    Button("pairing.preview") {
                        do {
                            try coordinator.previewPairing(payload)
                            pairingFocused = false
                            payload = ""
                            pairingHTTP = false
                        } catch { inputError = true }
                    }.disabled(coordinator.isBusy || payload.isEmpty)
                    if let invitation = coordinator.pendingPairing {
                        Text("pairing.confirmHelp")
                        Text(verbatim: invitation.apiBaseURL.absoluteString).textSelection(.enabled)
                        Toggle("connection.allowHTTP", isOn: $pairingHTTP)
                            .accessibilityIdentifier("pairing.allowHTTP")
                        Text("connection.httpWarning").font(.caption).foregroundStyle(.secondary)
                        Button("pairing.confirm") {
                            token = ""
                            payload = ""
                            Task { await coordinator.confirmPairing(allowInsecureHTTP: pairingHTTP) }
                        }.disabled(coordinator.isBusy)
                        Button("pairing.cancel", role: .cancel) { coordinator.cancelPairing() }
                    }
                }
                ConnectionSettings()
            }
            .navigationTitle("Holon")
            .background(WindowAnchorReader { anchor = $0 }.frame(width: 0, height: 0))
            .alert("input.error", isPresented: $inputError) {
                Button("action.ok", role: .cancel) {}
            } message: { Text("input.errorHelp") }
            .onChange(of: coordinator.selectedProfile?.id) { clearInputs() }
            .onChange(of: address) { allowHTTP = false }
        }
    }

    private func identityRow(_ key: LocalizedStringKey, _ value: String?) -> some View {
        LabeledContent(key) {
            if let value { Text(verbatim: value).textSelection(.enabled) }
            else { Text("identity.unavailable") }
        }
    }

    private func clearInputs() {
        pairingFocused = false
        token = ""
        payload = ""
        coordinator.cancelPairing()
    }

    private func addProfile() {
        guard let url = URL(string: address) else { inputError = true; return }
        do {
            let profile = try coordinator.addProfile(name: name.trimmingCharacters(in: .whitespacesAndNewlines), apiBaseURL: url, allowInsecureHTTP: allowHTTP)
            name = ""
            address = ""
            allowHTTP = false
            clearInputs()
            Task { await coordinator.connect(profile) }
        } catch { inputError = true }
    }
}

private struct WindowAnchorReader: UIViewRepresentable {
    let onChange: (UIWindow?) -> Void

    func makeUIView(context: Context) -> AnchorView {
        let view = AnchorView()
        view.onChange = onChange
        return view
    }

    func updateUIView(_ uiView: AnchorView, context: Context) { uiView.onChange = onChange }

    final class AnchorView: UIView {
        var onChange: ((UIWindow?) -> Void)?
        override func didMoveToWindow() {
            super.didMoveToWindow()
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.onChange?(self.window)
            }
        }
    }
}
