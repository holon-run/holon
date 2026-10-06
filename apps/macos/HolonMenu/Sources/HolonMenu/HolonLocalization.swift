import Foundation

enum L10n {
    static func text(_ key: String, locale: Locale? = nil) -> String {
        let bundle: Bundle
        if let locale {
            let language = locale.identifier.hasPrefix("zh") ? "zh-Hans" : "en"
            bundle = Bundle.module.path(forResource: language, ofType: "lproj")
                .flatMap(Bundle.init(path:)) ?? Bundle.module
        } else {
            bundle = Bundle.module
        }
        return bundle.localizedString(forKey: key, value: key, table: nil)
    }

    static func format(_ key: String, _ arguments: String..., locale: Locale? = nil) -> String {
        String(format: text(key, locale: locale), arguments: arguments)
    }
}
