import SwiftUI
import UIKit
import XCTest
@testable import Holon

@MainActor
final class ConversationConnectionStatusTests: XCTestCase {
    func testConnectionAndDraftChangesKeepTheSameHeightInBothLanguagesAndTextSizes() throws {
        for locale in ["en", "zh-Hans"] {
            var regularHeight: CGFloat = 0
            for size in [DynamicTypeSize.large, .accessibility5] {
                var expectedHeight: CGFloat?
                for status in [ReadingStatus.live, .syncing, .offline, .permissionDenied, .sessionExpired, .incompatible] {
                    for cached in [false, true] {
                        for refreshing in [false, true] {
                            let renderer = ImageRenderer(content:
                                ConversationConnectionStatus(
                                    status: status, hasSnapshot: cached || status == .live,
                                    isCached: cached, isRefreshing: refreshing,
                                    canPersistDraft: status == .offline, retry: {}
                                )
                                .frame(width: 320)
                                .environment(\.locale, Locale(identifier: locale))
                                .dynamicTypeSize(size)
                            )
                            renderer.scale = 1
                            let image = try XCTUnwrap(renderer.uiImage)
                            XCTAssertEqual(image.size.width, 320, accuracy: 0.5)
                            if let expectedHeight {
                                XCTAssertEqual(image.size.height, expectedHeight, accuracy: 0.5,
                                               "\(locale), \(size), \(status), cached=\(cached), refreshing=\(refreshing)")
                            } else {
                                expectedHeight = image.size.height
                            }
                        }
                    }
                }
                let height = try XCTUnwrap(expectedHeight)
                if size == .large { regularHeight = height }
                else { XCTAssertGreaterThan(height, regularHeight, "Stable chrome must still support Dynamic Type") }
            }
        }
    }
}
