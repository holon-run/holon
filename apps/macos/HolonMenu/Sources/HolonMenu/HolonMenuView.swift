import AppKit
import SwiftUI

struct HolonMenuView: View {
    @ObservedObject var viewModel: HolonMenuViewModel
    let updater: HolonUpdater
    var openSettings: () -> Void = {}

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
                    Text(L10n.format("Web: %@", viewModel.webAddressText))
                    Text(L10n.format("Polling: %@", viewModel.isPolling ? L10n.text("On") : L10n.text("Off")))
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .font(.caption)
            }

            GroupBox(L10n.text("Local network")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(viewModel.lanStateTitle)
                        .font(.headline)
                    if let lanURL = viewModel.lanURL {
                        Text(lanURL.absoluteString)
                            .font(.caption2)
                            .textSelection(.enabled)
                        Button(L10n.text("Disable LAN access")) {
                            Task { await viewModel.disableLAN() }
                        }
                    } else {
                        Text(L10n.text("Explicitly expose Holon on this Mac's local network."))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if viewModel.showLANConfirmation {
                            Text(L10n.text("Holon will restart and listen on ALL IPv4 interfaces (0.0.0.0), including LAN, VPN, and potentially public interfaces. A private control token is required and will be prepared if needed. Use firewall rules to limit access."))
                                .font(.caption)
                            HStack {
                                Button(L10n.text("Enable")) {
                                    Task { await viewModel.enableLAN() }
                                }
                                .disabled(viewModel.isOperating)
                                Button(L10n.text("Cancel")) {
                                    viewModel.showLANConfirmation = false
                                }
                            }
                        } else {
                            Button(L10n.text("Enable LAN access…")) {
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
                        Button(L10n.text("Copy LAN error")) {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(error, forType: .string)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            GroupBox("Tailscale") {
                VStack(alignment: .leading, spacing: 6) {
                    Text(viewModel.tailscaleStatus?.desiredTitle ?? L10n.text("Checking…"))
                        .font(.headline)
                    Text(viewModel.tailscaleStatus?.actualTitle ?? L10n.text("Checking…"))
                        .font(.headline)
                    Text(viewModel.tailscaleStatus.map { L10n.text($0.message) } ?? L10n.text("Checking Tailscale status."))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let serveURL = viewModel.tailscaleStatus?.serveURL {
                        Text(serveURL.absoluteString)
                            .font(.caption2)
                            .textSelection(.enabled)
                    }
                    if viewModel.tailscaleStatus?.hasDrift == true {
                        Text(L10n.text("Desired and actual Serve state differ. Use the control below to retry manually."))
                            .font(.caption)
                        if viewModel.tailscaleStatus?.conflict == true {
                            Text(L10n.text("Another Serve rule conflicts with Holon; resolve it before retrying."))
                                .font(.caption)
                        }
                    }
                    if viewModel.tailscaleStatus?.statusKnown == true,
                       viewModel.tailscaleStatus?.serving == true {
                        Button(L10n.text("Disable Serve")) {
                            Task { await viewModel.disableTailscaleServe() }
                        }
                        .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                    } else if viewModel.tailscaleStatus?.statusKnown == true {
                        if viewModel.showTailscaleServeConfirmation {
                            Text(L10n.text("Holon will expose its local web service over your tailnet. If authentication is missing, the app prepares a private token and may restart its managed daemon, preserving LAN and desktop settings. Externally managed daemons require manual authentication configuration. Your phone needs tailnet membership and ACL access."))
                                .font(.caption)
                            HStack {
                                Button(L10n.text("Enable")) {
                                    Task { await viewModel.enableTailscaleServe() }
                                }
                                .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                                Button(L10n.text("Cancel")) {
                                    viewModel.showTailscaleServeConfirmation = false
                                }
                            }
                        } else {
                            Button(viewModel.tailscaleStatus?.desiredEnabled == true ? L10n.text("Restore Serve…") : L10n.text("Enable Serve…")) {
                                viewModel.requestTailscaleServe()
                            }
                            .disabled(viewModel.isOperating || viewModel.tailscaleStatus?.conflict == true)
                        }
                        if viewModel.tailscaleStatus?.desiredEnabled == true {
                            Button(L10n.text("Turn off desired Serve")) {
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
                        Button(L10n.text("Copy Tailscale error")) {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(error, forType: .string)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            GroupBox(L10n.text("Pairing destination")) {
                VStack(alignment: .leading, spacing: 6) {
                    Toggle(L10n.text("Use LAN instead of Tailscale"), isOn: $viewModel.preferLANPairing)
                    if let destination = viewModel.connectionURL {
                        Text(destination.absoluteString)
                            .font(.caption2)
                            .textSelection(.enabled)
                        if destination == viewModel.tailscaleStatus?.pairingOrigin {
                            Text(L10n.text("Your phone must join this tailnet and have ACL access."))
                                .font(.caption)
                        }
                    } else {
                        Text(L10n.text("No valid remote origin. Enable LAN or Serve, or configure a custom origin in Settings."))
                            .font(.caption)
                    }
                }
            }

            if let connectionURL = viewModel.connectionURL {
                GroupBox(L10n.text("Connect from phone")) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(connectionURL.scheme == "https"
                             ? L10n.text("Pair over Tailscale HTTPS when possible.")
                             : L10n.text("Plain HTTP LAN pairing can be observed or redeemed first by someone on the same network. Prefer Tailscale HTTPS."))
                            .font(.caption)
                        if let pairingURL = viewModel.pairingURL {
                            HolonQRCodeView(payload: pairingURL.absoluteString)
                                .frame(maxWidth: .infinity)
                            Text(pairingURL.fragment == nil
                                 ? L10n.text("Open this URL and sign in normally.")
                                 : L10n.text("One-time pairing code; expires after 2 minutes. Keep this QR private."))
                                .font(.caption2)
                            Button(L10n.text("Hide pairing code")) { viewModel.hidePairingCode() }
                        } else {
                            Button(L10n.text("Show connection QR…")) {
                                Task { await viewModel.showPairingCode() }
                            }
                        }
                        Text(L10n.text("Opens an authorized Web session. The Android app supports automatic QR pairing."))
                            .font(.caption2)
                    }
                }
            }
            if let error = viewModel.pairingError {
                Text(error)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }

            HStack(spacing: 8) {
                Button(L10n.text("Start")) {
                    Task { await viewModel.start() }
                }
                .disabled(viewModel.isOperating || viewModel.isRunning)
                Button(L10n.text("Stop")) {
                    Task { await viewModel.stop() }
                }
                .disabled(viewModel.isOperating || !viewModel.isRunning)
                Button(L10n.text("Restart")) {
                    Task { await viewModel.restart() }
                }
                .disabled(viewModel.isOperating || !viewModel.isRunning)
            }

            HStack(spacing: 8) {
                Button(L10n.text("Open Web")) {
                    Task { await viewModel.openWeb() }
                }
                Button(L10n.text("Open Logs")) {
                    Task { await viewModel.openLogs() }
                }
            }

            Toggle(
                L10n.text("Launch Holon Menu App at Login"),
                isOn: Binding(
                    get: { viewModel.launchAtLoginEnabled },
                    set: { newValue in
                        viewModel.launchAtLoginEnabled = newValue
                        Task { await viewModel.setLaunchAtLogin(newValue) }
                    }
                )
            )

            Button(L10n.text("Check for Updates…")) {
                updater.checkForUpdates()
            }

            Button(L10n.text("Install Command Line Tool…")) {
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

            Button(L10n.text("Settings…"), action: openSettings)

            Button(L10n.text("Quit Holon Menu App")) {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(12)
        .frame(width: 320)
    }
}
