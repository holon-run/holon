import SwiftUI

/// Selection and management are separate actions, including on the connection welcome screen.
struct SavedNetworkRow: View {
    @Bindable var coordinator: ConnectionCoordinator
    let profile: ConnectionProfile
    let connect: () -> Void
    @State private var confirmingDeletion = false

    var body: some View {
        HStack(spacing: 12) {
            Button(action: connect) {
                HStack(spacing: 12) {
                    Image(systemName: "server.rack").foregroundStyle(.secondary).accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(verbatim: profile.name).font(.headline).foregroundStyle(.primary)
                        Text(verbatim: profile.apiBaseURL.absoluteString)
                            .font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.leading)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if coordinator.selectedProfile?.id == profile.id {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(Color.accentColor).accessibilityLabel(Text("profiles.selected"))
                    }
                }
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("profiles.select.\(profile.id)")
            Menu {
                Button("profiles.delete", systemImage: "trash", role: .destructive) { confirmingDeletion = true }
            } label: {
                Image(systemName: "ellipsis").frame(width: 44, height: 44)
            }
            .accessibilityLabel(Text("profiles.manage") + Text(verbatim: ": " + profile.name))
            .accessibilityIdentifier("profiles.actions.\(profile.id)")
        }
        .disabled(coordinator.isBusy)
        .swipeActions {
            Button("profiles.delete", role: .destructive) { confirmingDeletion = true }
                .disabled(coordinator.isBusy)
        }
        .alert("profiles.deleteTitle", isPresented: $confirmingDeletion) {
            Button("profiles.delete", role: .destructive) { Task { await coordinator.removeProfile(profile) } }
                .disabled(coordinator.isBusy)
                .accessibilityIdentifier("profiles.confirmDelete")
            Button("action.cancel", role: .cancel) {}
        } message: {
            Text(verbatim: profile.name + "\n\n") + Text("profiles.deleteHelp")
        }
    }
}
