import Foundation
import SwiftUI

/// Host-aware data-sharing disclosure: App Review 5.1.1 requires consent copy
/// to identify who receives user content, not only the connected runtime.
enum PrivacyDisclosure {
    /// Official demo runtime, operated by holon.run with DeepSeek as its only
    /// configured AI provider. Every other host is disclosed by address.
    static let officialDemoHost = "demo.holon.run"

    static func isOfficialDemo(_ url: URL) -> Bool {
        url.host()?.lowercased() == officialDemoHost
    }

    /// Disclosure text naming the recipient for the given runtime base URL.
    static func text(for url: URL?) -> String {
        guard let url, let host = url.host() else {
            return NSLocalizedString("privacy.disclosure", comment: "Generic data-sharing disclosure")
        }
        if isOfficialDemo(url) {
            return NSLocalizedString("privacy.disclosure.officialDemo", comment: "Official demo disclosure")
        }
        return String(
            format: NSLocalizedString("privacy.disclosure.thirdPartyHost", comment: "Third-party host disclosure"),
            host
        )
    }

    /// Language-aware privacy policy URL shared by every disclosure surface.
    static func policyURL(language: String) -> URL {
        let effective = language == "system" ? Locale.current.language.languageCode?.identifier ?? "en" : language
        return URL(string: effective.hasPrefix("zh") ? "https://holon.run/zh-CN/privacy" : "https://holon.run/privacy")!
    }
}

/// In-conversation AI data sharing entry point showing the same host-aware
/// disclosure as the consent gate.
struct PrivacyInfoSheet: View {
    let apiBaseURL: URL?
    @AppStorage("ui.language") private var language = "system"
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section("privacy.title") {
                    Text(verbatim: PrivacyDisclosure.text(for: apiBaseURL)).font(.footnote)
                    if let apiBaseURL {
                        Text(verbatim: apiBaseURL.absoluteString).font(.footnote)
                            .accessibilityIdentifier("privacy.target")
                    }
                    Link("privacy.policy", destination: PrivacyDisclosure.policyURL(language: language))
                        .accessibilityIdentifier("privacy.policy")
                }
            }
            .navigationTitle("privacy.aiInfo")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("common.cancel") { dismiss() } }
        }
    }
}
