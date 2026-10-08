import SwiftUI

struct SettingsView: View {
    @Bindable var connection: ConnectionCoordinator

    var body: some View {
            Form {
                Section("profiles.title") {
                    NavigationLink(value: AppRoute.connections) {
                        VStack(alignment: .leading, spacing: 4) {
                            Label("settings.connections", systemImage: "network")
                            if let profile = connection.selectedProfile {
                                Text(verbatim: profile.name).font(.subheadline).foregroundStyle(.secondary)
                            }
                            Text(LocalizedStringKey("status." + connection.status.rawValue))
                                .font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityIdentifier("settings.connection")
                }
                Section("settings.utilities") {
                    NavigationLink(value: AppRoute.tools) {
                        Label("settings.sharing", systemImage: "square.and.arrow.up")
                    }
                    .accessibilityIdentifier("settings.tools")
                    NavigationLink(value: AppRoute.diagnostics) {
                        Label("diagnostics.title", systemImage: "stethoscope")
                    }
                    .accessibilityIdentifier("diagnostics.open")
                }
                ConnectionSettings()
            }
            .navigationTitle("settings.title")
    }
}
