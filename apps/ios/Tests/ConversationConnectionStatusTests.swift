import SwiftUI
import UIKit
import XCTest
@testable import Holon

@MainActor
final class ConversationConnectionStatusTests: XCTestCase {
    func testNarrowMaximumTextSizeReservesUntruncatedRecoveryInstructions() throws {
        let recoveryStates: [(ReadingStatus, String)] = [
            (.sessionExpired, "reading.status.sessionExpired"),
            (.incompatible, "reading.status.incompatible"),
            (.offline, "reading.cache.offline"),
        ]
        for locale in ["en", "zh-Hans"] {
            for (status, key) in recoveryStates {
                let reference = ImageRenderer(content:
                    VStack(alignment: .leading, spacing: 12) {
                        Text(LocalizedStringKey(key)).font(.headline)
                            .fixedSize(horizontal: false, vertical: true)
                        Text("reading.cache.readOnly")
                            .fixedSize(horizontal: false, vertical: true)
                        if status != .sessionExpired { Color.clear.frame(height: 44) }
                    }
                    .frame(width: 320)
                    .environment(\.locale, Locale(identifier: locale))
                    .dynamicTypeSize(.accessibility5)
                )
                let renderer = ImageRenderer(content:
                    ConversationConnectionDetails(presentation: .init(status: status, hasSnapshot: true,
                                                                       isCached: true, canPersistDraft: true),
                                                  retry: {})
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

    func testConnectionActionKeepsItsHitTargetAcrossStatusAndDynamicType() throws {
        for locale in ["en", "zh-Hans"] {
            for size in [DynamicTypeSize.large, .accessibility5] {
                for status in [ReadingStatus.live, .syncing, .offline, .sessionExpired] {
                    let renderer = ImageRenderer(content:
                        ConversationConnectionButton(presentation: .init(status: status, hasSnapshot: true), open: {})
                            .environment(\.locale, Locale(identifier: locale))
                            .dynamicTypeSize(size)
                    )
                    let image = try XCTUnwrap(renderer.uiImage)
                    XCTAssertEqual(image.size.width, 44, accuracy: 0.5)
                    XCTAssertEqual(image.size.height, 44, accuracy: 0.5)
                }
            }
        }
    }

    func testRefreshDoesNotMaskAccessErrorsOrPretendCachedContentIsLive() {
        let expired = ConversationConnectionPresentation(status: .sessionExpired, hasSnapshot: true,
                                                         isCached: true, isRefreshing: true, canPersistDraft: true)
        XCTAssertEqual(expired.key, "reading.status.sessionExpired")
        XCTAssertTrue(expired.isError)
        XCTAssertFalse(expired.isLoading)
        XCTAssertTrue(expired.showsDraftNotice)
        XCTAssertFalse(expired.showsRetry)

        let offline = ConversationConnectionPresentation(status: .offline, hasSnapshot: true, isCached: true)
        XCTAssertEqual(offline.key, "reading.cache.offline")
        XCTAssertTrue(offline.showsRetry)
        XCTAssertFalse(offline.showsDraftNotice)
    }
}
