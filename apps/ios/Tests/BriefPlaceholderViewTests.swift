import SwiftUI
import UIKit
import XCTest
@testable import Holon

@MainActor
final class BriefPlaceholderViewTests: XCTestCase {
    func testLoadingHidesActionEvenWhenPreviousAttemptFailed() {
        let state = BriefPlaceholderState(isLoading: true, hasFailed: true)
        XCTAssertEqual(state, .loading)
        XCTAssertEqual(state.titleKey, "reading.brief.loading")
        XCTAssertNil(state.actionKey)
    }

    func testUnloadedOffersLoadAndFailureOffersExplicitRetry() {
        let unloaded = BriefPlaceholderState(isLoading: false, hasFailed: false)
        XCTAssertEqual(unloaded, .unloaded)
        XCTAssertEqual(unloaded.titleKey, "reading.brief.unloaded")
        XCTAssertEqual(unloaded.actionKey, "reading.brief.load")
        let failed = BriefPlaceholderState(isLoading: false, hasFailed: true)
        XCTAssertEqual(failed, .failed)
        XCTAssertEqual(failed.titleKey, "reading.brief.failed")
        XCTAssertEqual(failed.actionKey, "reading.retry")
    }

    func testPlaceholderRendersCompactlyAndExpandsForMaximumTextSize() throws {
        for locale in ["en", "zh-Hans"] {
            for state in [BriefPlaceholderState.unloaded, .failed] {
                for scheme in [ColorScheme.light, .dark] {
                    let regular = try render(state, locale: locale, scheme: scheme, size: .large)
                    let maximum = try render(state, locale: locale, scheme: scheme, size: .accessibility5)
                    XCTAssertEqual(regular.size.width, 320, accuracy: 0.5)
                    XCTAssertEqual(maximum.size.width, 320, accuracy: 0.5)
                    XCTAssertGreaterThanOrEqual(regular.size.height, 44)
                    XCTAssertLessThanOrEqual(regular.size.height, 64, "Normal placeholders should remain one compact row")
                    XCTAssertGreaterThan(maximum.size.height, regular.size.height, "Accessibility text must grow without a fixed row height")
                    if state == .failed {
                        for (name, image) in [("regular", regular), ("maximum", maximum)] {
                            let attachment = XCTAttachment(image: image)
                            attachment.name = "brief-placeholder-\(locale)-\(scheme)-\(name)"
                            attachment.lifetime = .keepAlways
                            add(attachment)
                        }
                    }
                }
            }
        }
    }

    private func render(_ state: BriefPlaceholderState, locale: String,
                        scheme: ColorScheme, size: DynamicTypeSize) throws -> UIImage {
        let renderer = ImageRenderer(content:
            BriefPlaceholderView(briefID: "fixture", state: state, load: {})
                .frame(width: 320)
                .environment(\.locale, Locale(identifier: locale))
                .environment(\.colorScheme, scheme)
                .dynamicTypeSize(size)
        )
        renderer.scale = 1
        return try XCTUnwrap(renderer.uiImage)
    }
}
