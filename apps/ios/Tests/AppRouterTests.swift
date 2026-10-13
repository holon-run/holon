import Foundation
import HolonClient
import XCTest
@testable import Holon

@MainActor
final class AppRouterTests: XCTestCase {
    private let bookmarkKey = "navigation.confirmedAgent.v1"

    private func partition(_ userID: String = "user") -> ReadingPartition {
        ReadingPartition(
            apiBaseURL: URL(string: "https://host.example/api")!,
            identity: HolonConnectionIdentity(networkID: "network", runtimeID: "runtime",
                                              userID: userID, visibilityScopeID: "private"))!
    }

    private func agent(_ id: String = "saved") -> ReadingAgent {
        ReadingAgent(id: id, name: id, preview: "")
    }

    private func withSavedRoute(_ check: (AppRouter, UserDefaults) -> Void) {
        let suite = "HolonAppRouterTests." + UUID().uuidString
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let original = AppRouter(preferences: preferences)
        original.activate(partition())
        original.path = [.conversation("saved")]
        original.remember()
        XCTAssertNotNil(preferences.data(forKey: bookmarkKey))
        let restored = AppRouter(preferences: preferences)
        restored.activate(partition())
        check(restored, preferences)
    }

    func testCachedRosterHitRestoresSavedAgent() {
        withSavedRoute { router, _ in
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertEqual(router.path, [.conversation("saved")])
            XCTAssertEqual(router.agentID, "saved")
        }
    }

    func testCachedRosterMissPreservesPendingRestorationForCurrentRoster() {
        withSavedRoute { router, preferences in
            router.restore(agents: [agent("other")], authoritative: false)
            XCTAssertTrue(router.path.isEmpty)
            XCTAssertNotNil(preferences.data(forKey: bookmarkKey))
            router.restore(agents: [agent()], authoritative: true)
            XCTAssertEqual(router.path, [.conversation("saved")])
        }
    }

    func testRememberAfterCacheMissPreservesRestorationAcrossColdStart() {
        withSavedRoute { router, preferences in
            let bookmark = preferences.data(forKey: bookmarkKey)
            router.restore(agents: [], authoritative: false)
            router.remember()
            XCTAssertEqual(preferences.data(forKey: bookmarkKey), bookmark)
            let relaunched = AppRouter(preferences: preferences)
            relaunched.activate(partition())
            relaunched.restore(agents: [agent()], authoritative: false)
            XCTAssertEqual(relaunched.path, [.conversation("saved")])
        }
    }

    func testCurrentRosterMissClearsPendingAndPersistedBookmark() {
        withSavedRoute { router, preferences in
            router.restore(agents: [agent("other")], authoritative: false)
            router.restore(agents: [agent("other")], authoritative: true)
            XCTAssertTrue(router.path.isEmpty)
            XCTAssertNil(preferences.data(forKey: bookmarkKey))
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertTrue(router.path.isEmpty)
            let relaunched = AppRouter(preferences: preferences)
            relaunched.activate(partition())
            relaunched.restore(agents: [agent()], authoritative: false)
            XCTAssertTrue(relaunched.path.isEmpty)
        }
    }

    func testCurrentEmptyRosterRejectsPendingBookmark() {
        withSavedRoute { router, preferences in
            router.restore(agents: [], authoritative: true)
            router.remember()
            XCTAssertNil(preferences.data(forKey: bookmarkKey))
            router.restore(agents: [agent()], authoritative: true)
            XCTAssertTrue(router.path.isEmpty)
        }
    }

    func testCachedRosterHitRequiresExactPartition() {
        withSavedRoute { router, preferences in
            router.activate(partition("other-user"))
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertTrue(router.path.isEmpty)
            XCTAssertNotNil(preferences.data(forKey: bookmarkKey))
            router.activate(partition())
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertEqual(router.path, [.conversation("saved")])
        }
    }

    func testCachedRestorationDoesNotReplaceUserNavigation() {
        withSavedRoute { router, preferences in
            router.path = [.settings]
            router.restore(agents: [agent()], authoritative: false)
            router.remember()
            XCTAssertEqual(router.path, [.settings])
            XCTAssertNotNil(preferences.data(forKey: bookmarkKey))
            router.path = []
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertEqual(router.path, [.conversation("saved")])
        }
    }

    func testWithdrawalPreventsLateCachedRestoration() {
        withSavedRoute { router, preferences in
            router.withdrawAgent()
            router.restore(agents: [agent()], authoritative: false)
            XCTAssertTrue(router.path.isEmpty)
            XCTAssertNil(preferences.data(forKey: bookmarkKey))
        }
    }
}
