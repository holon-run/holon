import Foundation
import HolonClient
import XCTest
@testable import Holon

final class ConnectionInputTests: XCTestCase {
    func testPairingPreservesProxyPrefixAndRedactsTicket() throws {
        let ticket = String(repeating: "a", count: 64)
        guard case .pairing(let invitation) = try ConnectionInput(" https://host.test/holon/login#pair=\(ticket) ") else {
            return XCTFail("Expected a pairing invitation")
        }
        XCTAssertEqual(invitation.apiBaseURL.absoluteString, "https://host.test/holon/api/")
        XCTAssertFalse(String(describing: invitation).contains(ticket))
        XCTAssertThrowsError(try ConnectionInput("https://host.test/login#pair=expired"))
    }

    func testAddressCodeOnlyPrefillsAnAPIEndpoint() throws {
        for input in ["https://host.test", "https://host.test/api", "https://host.test/api/"] {
            XCTAssertEqual(try ConnectionInput(input), .address(URL(string: "https://host.test/api/")!))
        }
        XCTAssertEqual(try ConnectionInput("https://host.test/proxy/"),
                       .address(URL(string: "https://host.test/proxy/api/")!))
        // Previewing HTTP does not itself authorize insecure requests.
        XCTAssertEqual(try ConnectionInput("http://host.test:8787"),
                       .address(URL(string: "http://host.test:8787/api/")!))
    }

    func testUnsafeOrOversizedInputIsRejectedWithoutEchoingIt() {
        for input in [
            "https://user:secret@host.test/api", "https://host.test/api?token=secret",
            "https://host.test/api#token=secret", "run.holon.ios://oidc/callback",
            "file:///etc/passwd", "https://host.test/%2e%2e/api", "https://host.test/a%2fb",
            "https://host.test:0/api", "https://host.test:65536/api", "",
            String(repeating: "a", count: 2049)
        ] {
            XCTAssertThrowsError(try ConnectionInput(input)) { error in
                XCTAssertTrue(error is HolonClientError)
            }
        }
    }
}
