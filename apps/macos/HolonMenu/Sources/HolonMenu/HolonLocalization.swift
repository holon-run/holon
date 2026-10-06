import Foundation

enum L10n {
    static func text(_ key: String, locale: Locale? = nil) -> String {
        let bundle: Bundle
        if let locale {
            bundle = localizedBundle(for: locale, in: Bundle.module)
        } else {
            bundle = Bundle.module
        }
        return bundle.localizedString(forKey: key, value: key, table: nil)
    }

    static func localizedBundle(for locale: Locale, in resources: Bundle) -> Bundle {
        let language = locale.identifier.hasPrefix("zh") ? "zh-Hans" : "en"
        let localization = Bundle.preferredLocalizations(
            from: resources.localizations, forPreferences: [language]
        ).first
        return localization.flatMap { resources.path(forResource: $0, ofType: "lproj") }
            .flatMap(Bundle.init(path:)) ?? resources
    }

    static func format(_ key: String, _ arguments: String..., locale: Locale? = nil) -> String {
        String(format: text(key, locale: locale), arguments: arguments)
    }
}
