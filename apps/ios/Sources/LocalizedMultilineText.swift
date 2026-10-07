import SwiftUI
import UIKit

// Keep mixed-script multiline sizing in the same renderer as its font.
struct LocalizedMultilineText: UIViewRepresentable {
    let key: String
    var isHeading = false
    @Environment(\.locale) private var locale
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    func makeUIView(context: Context) -> UILabel {
        let label = UILabel()
        label.numberOfLines = 0
        label.adjustsFontForContentSizeCategory = true
        label.setContentCompressionResistancePriority(.required, for: .vertical)
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return label
    }

    func updateUIView(_ label: UILabel, context: Context) {
        configure(label)
    }

    private func configure(_ label: UILabel) {
        let resource = LocalizedStringResource(String.LocalizationValue(key),
                                               locale: locale)
        label.text = String(localized: resource)
        let traits = UITraitCollection(preferredContentSizeCategory:
            UIContentSizeCategory(dynamicTypeSize))
        let font = UIFont.preferredFont(forTextStyle: isHeading ? .title2 : .body,
                                        compatibleWith: traits)
        if isHeading {
            let descriptor = font.fontDescriptor.withSymbolicTraits(.traitBold) ?? font.fontDescriptor
            label.font = UIFont(descriptor: descriptor, size: 0)
        } else {
            label.font = font
        }
        label.textColor = isHeading ? .label : .secondaryLabel
        label.accessibilityIdentifier = key
        label.accessibilityTraits = isHeading ? [.staticText, .header] : .staticText
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView label: UILabel,
                      context: Context) -> CGSize? {
        guard let width = proposal.width, width > 0 else { return nil }
        // SwiftUI can measure the new environment before updating the UIKit view.
        configure(label)
        let size = label.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: size.height)
    }
}
