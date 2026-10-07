import SwiftUI

enum SettingsDestination: Hashable {
    case connections, files, tools, diagnostics
}

struct SettingsView: View {
    @Bindable var connection: ConnectionCoordinator
    let reader: ReadingCoordinator
    let sender: SendingCoordinator?
    let files: FilesCoordinator
    let imports: SharedImportCoordinator
    @Binding var path: [SettingsDestination]
    let addConnection: () -> Void

    var body: some View {
        NavigationStack(path: $path) {
            Form {
                Section("profiles.title") {
                    NavigationLink(value: SettingsDestination.connections) {
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
                    NavigationLink(value: SettingsDestination.files) {
                        Label("files.title", systemImage: "folder")
                    }
                    .accessibilityIdentifier("settings.files")
                    NavigationLink(value: SettingsDestination.tools) {
                        Label("settings.sharing", systemImage: "square.and.arrow.up")
                    }
                    .accessibilityIdentifier("settings.tools")
                    NavigationLink(value: SettingsDestination.diagnostics) {
                        Label("diagnostics.title", systemImage: "stethoscope")
                    }
                    .accessibilityIdentifier("diagnostics.open")
                }
                ConnectionSettings()
            }
            .navigationTitle("settings.title")
            .navigationDestination(for: SettingsDestination.self) { destination in
                switch destination {
                case .connections:
                    ContentView(coordinator: connection, addConnection: addConnection)
                case .files:
                    FilesView(coordinator: files)
                case .tools:
                    SystemExperienceView(connection: connection, reader: reader,
                                         sender: sender, imports: imports)
                case .diagnostics:
                    DiagnosticView(connection: connection, reader: reader, sender: sender)
                }
            }
        }
    }
}
