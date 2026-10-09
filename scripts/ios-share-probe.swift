import UIKit

/// Independent sender app for testing the actual OS sheet, not a production hook.
@main @MainActor
final class ShareProbeApp: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(_ application: UIApplication, didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = ShareProbeView()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}
@MainActor
final class ShareProbeView: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let stack = UIStackView()
        stack.axis = .vertical; stack.spacing = 20
        stack.translatesAutoresizingMaskIntoConstraints = false
        for kind in ["text", "url", "image", "file"] {
            let button = UIButton(type: .system)
            button.setTitle("Share " + kind, for: .normal)
            button.accessibilityIdentifier = "probe." + kind
            button.addAction(UIAction { [weak self] _ in self?.share(kind, anchor: button) }, for: .touchUpInside)
            stack.addArrangedSubview(button)
        }
        view.addSubview(stack)
        NSLayoutConstraint.activate([stack.centerXAnchor.constraint(equalTo: view.centerXAnchor),
                                     stack.centerYAnchor.constraint(equalTo: view.centerYAnchor)])
    }
    private func share(_ kind: String, anchor: UIButton) {
        let item: Any
        switch kind {
        case "text": item = "IOS_SHARED_TEXT\nLiteral **operator** input"
        case "url": item = URL(string: "https://example.test/holon-share")!
        case "image":
            let format = UIGraphicsImageRendererFormat()
            format.scale = 1
            item = UIGraphicsImageRenderer(size: CGSize(width: 16, height: 16), format: format).image { context in
                UIColor.systemBlue.setFill(); context.fill(CGRect(x: 0, y: 0, width: 16, height: 16))
            }
        default:
            let url = FileManager.default.temporaryDirectory.appendingPathComponent("shared-note.txt")
            do { try Data("IOS_SHARED_FILE_BYTES".utf8).write(to: url, options: .atomic) }
            catch { return }
            item = url
        }
        let sheet = UIActivityViewController(activityItems: [item], applicationActivities: nil)
        sheet.popoverPresentationController?.sourceView = anchor
        sheet.popoverPresentationController?.sourceRect = anchor.bounds
        present(sheet, animated: true)
    }
}
