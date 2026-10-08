import Foundation
import HolonClient
import XCTest
@testable import Holon

private final class CodeFileProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let url = request.url!, name = url.lastPathComponent
        let mediaTypes = ["ts": "text/typescript", "sh": "application/x-sh", "toml": "application/toml", "py": "text/x-python", "c": "text/x-c"]
        let media = mediaTypes[url.pathExtension] ?? "application/octet-stream"
        let metadata = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.contains { $0.name == "meta" } == true
        let body: Data
        if metadata {
            body = try! JSONSerialization.data(withJSONObject: ["type": "file", "kind": "file", "workspace_id": "w",
                "execution_root_id": "r", "root_kind": "canonical_root", "path": name,
                "absolute_path": "/root/" + name, "size": 12, "mime_type": media])
        } else { body = Data("literal code".utf8) }
        let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil,
            headerFields: ["Content-Type": metadata ? "application/json" : media])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class FileCodeTransportTests: XCTestCase {
    func testSupportedSourceMIMEsReachTheLiteralCodeReader() async throws {
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [CodeFileProtocol.self]
        let client = try HolonClient(endpoint: HolonEndpoint(apiBaseURL: URL(string: "https://code.invalid/api")!),
                                     networkID: "n", configuration: configuration)
        try await client.bindIdentity(runtimeID: "runtime", userID: "user", visibilityScopeID: "scope", credential: nil)
        let transport = FilesClientTransport(client: client, authority: .init(networkID: "n", runtimeID: "runtime", userID: "user", visibilityScopeID: "scope"))
        for ext in ["ts", "sh", "toml", "py", "c"] {
            let file = try await transport.download(source: .workspace(.init(workspaceID: "w", executionRootID: "r", name: "W"), path: "code." + ext), maximumBytes: 100)
            XCTAssertEqual(String(data: file.data, encoding: .utf8), "literal code")
            XCTAssertEqual(FilesPreviewKind.classify(mediaType: file.mediaType, name: file.name), .text)
            XCTAssertEqual(file.location?.executionRootID, "r")
        }
        await transport.close()
    }
}
