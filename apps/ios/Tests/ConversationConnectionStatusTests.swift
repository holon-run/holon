import SwiftUI
import UIKit
import XCTest
@testable import Holon

@MainActor
final class ConversationConnectionStatusTests: XCTestCase {
    func testNarrowMaximumTextSizeReservesUntruncatedRecoveryInstructions() throws {
        for locale in ["en", "zh-Hans"] {
            for key in ["reading.status.sessionExpired", "reading.status.incompatible", "reading.cache.offline"] {
                // At accessibility sizes instructions use the full width,
                // below the fixed-height icon/action row.
                let reference = ImageRenderer(content:
                    VStack(alignment: .leading, spacing: 4) {
                        Color.clear.frame(height: 44)
                        Text(LocalizedStringKey(key)).font(.caption)
                            .fixedSize(horizontal: false, vertical: true)
                        Text("reading.cache.readOnly").font(.caption2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.horizontal, 16).padding(.vertical, 8)
                    .frame(width: 320)
                    .environment(\.locale, Locale(identifier: locale))
                    .dynamicTypeSize(.accessibility5)
                )
                let renderer = ImageRenderer(content:
                    ConversationConnectionStatus(status: .offline, hasSnapshot: true,
                                                 isCached: true, canPersistDraft: true, retry: {})
                        .frame(width: 320)
                        .environment(\.locale, Locale(identifier: locale))
                        .dynamicTypeSize(.accessibility5)
                )
                let referenceImage = try XCTUnwrap(reference.uiImage)
                let image = try XCTUnwrap(renderer.uiImage)
                XCTAssertGreaterThanOrEqual(image.size.height + 0.5, referenceImage.size.height,
                                           "\(locale), \(key): full recovery instructions must fit")
            }
        }
    }

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
