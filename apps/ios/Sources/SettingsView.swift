import SwiftUI

struct SettingsView: View {
    @Bindable var connection: ConnectionCoordinator
    @AppStorage("ui.language") private var language = "system"
    @State private var confirmingConsent = false
    @State private var consentRevision = 0
    @State private var reviewedURL: URL?

    private var policyURL: URL {
        let effective = language == "system" ? Locale.current.language.languageCode?.identifier ?? "en" : language
        return URL(string: effective.hasPrefix("zh") ? "https://holon.run/zh-CN/privacy" : "https://holon.run/privacy")!
    }

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
                Section("privacy.title") {
                    Link("privacy.policy", destination: policyURL)
                        .accessibilityIdentifier("privacy.policy")
                    Link("privacy.support", destination: URL(string: "mailto:hello@holon.run")!)
                    Text("privacy.disclosure").font(.footnote)
                    if let url = connection.selectedProfile?.apiBaseURL {
                        Text(verbatim: url.absoluteString).font(.footnote)
                        if SharingConsent.shared.approved(url) {
                            Button("privacy.revoke", role: .destructive) {
                                SharingConsent.shared.revoke(url)
                                consentRevision += 1
                            }
                            .accessibilityIdentifier("privacy.revoke")
                        } else {
                            Button("privacy.review") {
                                reviewedURL = url
                                confirmingConsent = true
                            }
                                .accessibilityIdentifier("privacy.review")
                        }
                    }
                    Text("privacy.withdrawal").font(.footnote)
                }
                .id(consentRevision)
            }
            .navigationTitle("settings.title")
            .alert("privacy.title", isPresented: $confirmingConsent) {
                Button("privacy.agree") {
                    if let url = reviewedURL, url == connection.selectedProfile?.apiBaseURL {
                        SharingConsent.shared.approve(url)
                        consentRevision += 1
                    }
                }
                .accessibilityIdentifier("privacy.agree")
                Button("privacy.cancel", role: .cancel) {}
            } message: {
                Text("privacy.disclosure")
            }
    }
}
