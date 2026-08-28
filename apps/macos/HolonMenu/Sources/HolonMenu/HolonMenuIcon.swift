import AppKit

enum HolonMenuIcon {
    static let image: NSImage = {
        guard
            let url = Bundle.module.url(forResource: "holon-mark", withExtension: "png"),
            let image = NSImage(contentsOf: url)
        else {
            return NSImage(
                systemSymbolName: "hexagon",
                accessibilityDescription: "Holon"
            ) ?? NSImage()
        }

        image.isTemplate = true
        image.size = NSSize(width: 18, height: 18)
        image.accessibilityDescription = "Holon"
        return image
    }()
}
