import SwiftUI

struct HolonMenuSettingsView: View {
    @ObservedObject var viewModel: HolonMenuViewModel

    var body: some View {
        GroupBox("Advanced") {
            VStack(alignment: .leading, spacing: 8) {
                TextField("Custom remote origin (optional)", text: $viewModel.customPairingOrigin)
                    .textFieldStyle(.roundedBorder)
                Text("Leave empty to use Tailscale or LAN automatically. Custom origins must not contain a path, query, or fragment. Changing the destination hides the current pairing QR.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding()
        .frame(width: 440)
    }
}
