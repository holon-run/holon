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

            GroupBox("Local network") {
                VStack(alignment: .leading, spacing: 6) {
                    Text(viewModel.lanURL == nil ? "LAN access is off" : "LAN access is on")
                        .font(.headline)
                    if let lanURL = viewModel.lanURL {
                        Text(lanURL.absoluteString)
                            .font(.caption2)
                            .textSelection(.enabled)
                        Button("Disable LAN access") {
                            Task { await viewModel.disableLAN() }
                        }
                    } else {
                        Text("Explicitly expose Holon on this Mac's local network.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if viewModel.showLANConfirmation {
                            Text("Holon will listen on the local network and require a control token. If none is configured, the app creates a private control.token file in the Holon home directory. Devices on the same network may be able to reach this service.")
                                .font(.caption)
                            HStack {
                                Button("Enable") {
                                    Task { await viewModel.enableLAN() }
                                }
                                .disabled(viewModel.isOperating)
                                Button("Cancel") {
                                    viewModel.showLANConfirmation = false
                                }
                            }
                        } else {
                            Button("Enable LAN access…") {
                                viewModel.requestLANAccess()
                            }
                            .disabled(viewModel.isOperating)
                        }
                    }
                    if let error = viewModel.lanError {
                        Text(error)
                            .font(.caption2)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                        Button("Copy LAN error") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(error, forType: .string)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            GroupBox("Tailscale") {
                VStack(alignment: .leading, spacing: 6) {
                    Text(viewModel.tailscaleStatus?.desiredTitle ?? "Checking…")
                        .font(.headline)
                    Text(viewModel.tailscaleStatus?.actualTitle ?? "Checking…")
                        .font(.headline)
                    Text(viewModel.tailscaleStatus?.message ?? "Checking Tailscale status.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let serveURL = viewModel.tailscaleStatus?.serveURL {
                        Text(serveURL.absoluteString)
                            .font(.caption2)
                            .textSelection(.enabled)
                    }
                    if viewModel.tailscaleStatus?.hasDrift == true {
                        Text("Desired and actual Serve state differ. Use the control below to retry manually.")
                            .font(.caption)
                        if viewModel.tailscaleStatus?.conflict == true {
                            Text("Another Serve rule conflicts with Holon; resolve it before retrying.")
                                .font(.caption)
                        }
                    }
                    if viewModel.tailscaleStatus?.statusKnown == true,
                       viewModel.tailscaleStatus?.serving == true {
                        Button("Disable Serve") {
                            Task { await viewModel.disableTailscaleServe() }
                        }
                        .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                    } else if viewModel.tailscaleStatus?.statusKnown == true {
                        if viewModel.showTailscaleServeConfirmation {
                            Text("Holon will ask Tailscale to expose its local web service over your tailnet. This changes network reachability and can be disabled from this menu.")
                                .font(.caption)
                            HStack {
                                Button("Enable") {
                                    Task { await viewModel.enableTailscaleServe() }
                                }
                                .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                                Button("Cancel") {
                                    viewModel.showTailscaleServeConfirmation = false
                                }
                            }
                        } else {
                            Button(viewModel.tailscaleStatus?.desiredEnabled == true ? "Restore Serve…" : "Enable Serve…") {
                                viewModel.requestTailscaleServe()
                            }
                            .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                        }
                        if viewModel.tailscaleStatus?.desiredEnabled == true {
                            Button("Turn off desired Serve") {
                                Task { await viewModel.disableTailscaleServe() }
                            }
                            .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                        }
                    }
                    if let error = viewModel.tailscaleError {
                        Text(error)
                            .font(.caption2)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                        Button("Copy Tailscale error") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(error, forType: .string)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            if let connectionURL = viewModel.connectionURL {
                GroupBox("Connect from phone") {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(connectionURL.scheme == "https"
                             ? "Pair over Tailscale HTTPS when possible."
                             : "Plain HTTP LAN pairing can be observed or redeemed first by someone on the same network. Prefer Tailscale HTTPS.")
                            .font(.caption)
                        if let pairingURL = viewModel.pairingURL {
                            HolonQRCodeView(payload: pairingURL.absoluteString)
                                .frame(maxWidth: .infinity)
                            Text("One-time pairing code; expires after 2 minutes. Keep this QR private.")
                                .font(.caption2)
                            Button("Hide pairing code") { viewModel.hidePairingCode() }
                        } else {
                            Button("Show one-time pairing QR…") {
                                Task { await viewModel.showPairingCode() }
                            }
                        }
                        Text("Opens an authorized Web session. Android app automatic pairing requires an app update.")
                            .font(.caption2)
                        if let error = viewModel.pairingError {
                            Text(error)
                                .font(.caption2)
                                .foregroundStyle(.red)
                                .textSelection(.enabled)
                        }
                    }
                }
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
