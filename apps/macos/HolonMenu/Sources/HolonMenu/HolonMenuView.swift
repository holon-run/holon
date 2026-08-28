import AppKit
import SwiftUI

struct HolonMenuView: View {
    @ObservedObject var viewModel: HolonMenuViewModel
    let updater: HolonUpdater

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Holon")
                    .font(.headline)
                Text(viewModel.stateTitle)
                    .font(.title3.weight(.semibold))
                Text(viewModel.statusMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let activeOperation = viewModel.activeOperation {
                    ProgressView(activeOperation)
                        .controlSize(.small)
                }
            }

            GroupBox {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Web: \(viewModel.webAddressText)")
                    Text("Polling: \(viewModel.isPolling ? "On" : "Off")")
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .font(.caption)
            }

            HStack(spacing: 8) {
                Button("Start") {
                    Task { await viewModel.start() }
                }
                .disabled(viewModel.isOperating || viewModel.isRunning)
                Button("Stop") {
                    Task { await viewModel.stop() }
                }
                .disabled(viewModel.isOperating || !viewModel.isRunning)
                Button("Restart") {
                    Task { await viewModel.restart() }
                }
                .disabled(viewModel.isOperating || !viewModel.isRunning)
            }

            HStack(spacing: 8) {
                Button("Open Web") {
                    Task { await viewModel.openWeb() }
                }
                Button("Open Logs") {
                    Task { await viewModel.openLogs() }
                }
            }

            Toggle(
                "Launch Holon Menu App at Login",
                isOn: Binding(
                    get: { viewModel.launchAtLoginEnabled },
                    set: { newValue in
                        viewModel.launchAtLoginEnabled = newValue
                        Task { await viewModel.setLaunchAtLogin(newValue) }
                    }
                )
            )

            Button("Check for Updates…") {
                updater.checkForUpdates()
            }

            Button("Install Command Line Tool…") {
                Task { await viewModel.installCommandLineTool() }
            }

            if let message = viewModel.commandLineToolMessage {
                Text(message)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }

            if let error = viewModel.lastError {
                Text(error)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Divider()

            Button("Quit Holon Menu App") {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(12)
        .frame(width: 320)
    }
}
