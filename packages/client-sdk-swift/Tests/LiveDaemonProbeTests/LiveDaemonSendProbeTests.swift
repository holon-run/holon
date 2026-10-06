import Foundation
import HolonClient
import XCTest

final class LiveDaemonSendProbeTests: XCTestCase {
    func testReadyDaemonWritesAndLostAcknowledgement() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let base = environment["HOLON_SEND_BASE"],
              let ticket = environment["HOLON_SEND_PAIRING_TICKET"],
              let heldRunStarted = environment["HOLON_SEND_HELD_RUN_STARTED"],
              let proxyBase = environment["HOLON_SEND_PROXY_BASE"] else {
            throw XCTSkip("Requires isolated ready daemon and HOLON_SEND_BASE, HOLON_SEND_PAIRING_TICKET, HOLON_SEND_PROXY_BASE, HOLON_SEND_HELD_RUN_STARTED")
        }
        let endpoint = try HolonEndpoint(apiBaseURL: XCTUnwrap(URL(string: base)))
        let client = try HolonClient(endpoint: endpoint, networkID: "ready-send")
        let login = try await client.redeemPairingTicket(ticket: ticket)
        XCTAssertTrue(login.value.ok)
        let credential = try XCTUnwrap(login.value.credential)
        try await client.bindIdentity(runtimeID: nil, userID: login.value.userId,
                                      visibilityScopeID: nil, credential: credential)
        let user = try await client.currentUser()
        XCTAssertTrue(user.value.ok)
        let catalog = try await client.modelCatalog()
        guard case .array(let models) = catalog.value.availableModels else {
            return XCTFail("Model catalog must contain an array")
        }
        XCTAssertFalse(models.isEmpty)
        let selection = try HolonAgentModelRequest(model: "ios-fixture/fixture-model")
        _ = try await client.setAgentModel(agentID: "main", request: selection)
        let model = try await client.agentModel(agentID: "main")
        XCTAssertEqual(model.value.effectiveModel, .string("ios-fixture@default/fixture-model"))

        let attachment = try HolonPromptAttachment(kind: .file, name: "contract.txt",
            mediaType: "text/plain", data: Data("inline attachment contract".utf8))
        let prompt = try HolonPromptRequest(clientRequestID: UUID().uuidString,
            text: "Read the attached contract and acknowledge it.", attachments: [attachment])
        let accepted = try await client.sendOperatorPrompt(agentID: "main", request: prompt)
        XCTAssertTrue(accepted.value.isAccepted)
        let duplicate = try await client.sendOperatorPrompt(agentID: "main", request: prompt)
        XCTAssertEqual(duplicate.value.messageID, accepted.value.messageID)
        XCTAssertEqual(duplicate.value.disposition, "duplicate")

        let anonymous = try HolonClient(endpoint: endpoint, networkID: "ready-denied")
        do {
            _ = try await anonymous.sendOperatorPrompt(agentID: "main", request: prompt)
            XCTFail("Unauthenticated writes must be rejected")
        } catch {
            XCTAssertEqual((error as? HolonHTTPFailure)?.statusCode, 401)
        }

        let proxy = try HolonClient(
            endpoint: HolonEndpoint(apiBaseURL: XCTUnwrap(URL(string: proxyBase))),
            networkID: "ready-proxy", credential: credential)
        let uncertain = try HolonPromptRequest(clientRequestID: UUID().uuidString,
            text: "Acknowledge this lost-response contract.", attachments: [attachment])
        do {
            _ = try await proxy.sendOperatorPrompt(agentID: "main", request: uncertain)
            XCTFail("The prefix proxy must discard the first successful prompt response")
        } catch {
            XCTAssertNil(error as? HolonHTTPFailure, "Expected lost transport response, not HTTP rejection")
        }
        let recovered = try await proxy.sendOperatorPrompt(agentID: "main", request: uncertain)
        XCTAssertEqual(recovered.value.disposition, "duplicate")
        let direct = try await client.sendOperatorPrompt(agentID: "main", request: uncertain)
        XCTAssertEqual(direct.value.messageID, recovered.value.messageID)

        let held = try HolonPromptRequest(clientRequestID: UUID().uuidString,
            text: "Explicit stop contract: hold this request.")
        let heldReceipt = try await client.sendOperatorPrompt(agentID: "main", request: held)
        XCTAssertTrue(heldReceipt.value.isAccepted)
        for _ in 0..<100 {
            if FileManager.default.fileExists(atPath: heldRunStarted) { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: heldRunStarted),
                      "The provider must hold the explicit-stop request before inspection")
        let active = try await client.listAgents()
        let main = try XCTUnwrap(active.value.agents.first(where: { $0.id == "main" }))
        let runID = try XCTUnwrap(main.currentRunId, "Must observe a real active run")
        let stopped = try await client.stopCurrentRun(agentID: "main", runID: runID)
        XCTAssertEqual(stopped.value.runID, runID)
        var finished = false
        for _ in 0..<100 {
            let state = try await client.listAgents()
            let agent = try XCTUnwrap(state.value.agents.first(where: { $0.id == "main" }))
            if agent.currentRunId == nil {
                finished = true
                break
            }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertTrue(finished, "The explicitly stopped run must leave the active roster")
    }
}
