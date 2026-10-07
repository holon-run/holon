import SwiftUI
import AuthenticationServices

struct ContentView: View {
    @Bindable var coordinator: ConnectionCoordinator
    let addConnection: () -> Void

    var body: some View {
        Form {
            Section("profiles.title") {
                ForEach(coordinator.profiles) { profile in
                    Button {
                        Task { await coordinator.connect(profile) }
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(verbatim: profile.name)
                                if coordinator.selectedProfile?.id == profile.id {
                                    Image(systemName: "checkmark").accessibilityLabel(Text("profiles.selected"))
                                }
                            }
                            Text(verbatim: profile.apiBaseURL.absoluteString)
                                .font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                    .disabled(coordinator.isBusy)
                    .swipeActions {
                        Button("profiles.delete", role: .destructive) {
                            Task { await coordinator.removeProfile(profile) }
                        }.disabled(coordinator.isBusy)
                    }
                }
                Button("connection.add", systemImage: "plus", action: addConnection)
                    .accessibilityIdentifier("connection.add")
                    .disabled(coordinator.isBusy)
            }
            Section("connection.title") {
                ConnectionStatusView(coordinator: coordinator)
                Button("connection.refresh") { Task { await coordinator.refresh() } }
                    .disabled(coordinator.isBusy || coordinator.selectedProfile == nil)
                Button("login.logout", role: .destructive) {
                    Task { await coordinator.logout() }
                }.disabled(coordinator.isBusy || coordinator.selectedProfile == nil)
            }
            if let identity = coordinator.identity {
                Section {
                    DisclosureGroup("connection.advanced") {
                        identityRow("identity.network", identity.networkID)
                        identityRow("identity.runtime", identity.runtimeID)
                        identityRow("identity.user", identity.userID)
                        identityRow("identity.scope", identity.visibilityScopeID)
                    }
                }
            }
        }
        .navigationTitle("settings.connections")
    }

    private func identityRow(_ key: LocalizedStringKey, _ value: String?) -> some View {
        LabeledContent(key) {
            if let value { Text(verbatim: value).textSelection(.enabled).privacySensitive() }
            else { Text("identity.unavailable") }
        }
    }
}

struct WindowAnchorReader: UIViewRepresentable {
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
