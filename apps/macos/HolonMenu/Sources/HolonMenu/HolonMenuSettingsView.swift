import SwiftUI

struct HolonMenuSettingsView: View {
    @ObservedObject var viewModel: HolonMenuViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            GroupBox(L10n.text("Advanced")) {
                VStack(alignment: .leading, spacing: 8) {
                    TextField(L10n.text("Custom remote origin (optional)"), text: $viewModel.customPairingOrigin)
                        .textFieldStyle(.roundedBorder)
                    Text(L10n.text("Leave empty to use Tailscale or LAN automatically. Custom origins must not contain a path, query, or fragment. Changing the destination hides the current pairing QR."))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            GroupBox(L10n.text("Daemon credentials")) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L10n.text("Control token file (optional)"))
                        .font(.caption)
                    TextField(
                        L10n.text("Path to an owner-only control token file"),
                        text: $viewModel.controlTokenFilePath
                    )
                    .textFieldStyle(.roundedBorder)
                    HStack {
                        Button(L10n.text("Save token file")) {
                            Task { await viewModel.saveControlTokenFilePath() }
                        }
                        .disabled(viewModel.isOperating)
                        if let message = viewModel.controlTokenFileMessage {
                            Text(message)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if let error = viewModel.controlTokenFileError {
                        Text(error)
                            .font(.caption2)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Text(L10n.text("Use an absolute path. Saving an empty field clears the saved approval."))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(L10n.text("Used only when the daemon requires externally managed authentication. Only the file path is saved; the token content is never stored."))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding()
        .frame(width: 440)
        .task { await viewModel.loadControlTokenFilePath() }
    }
}
