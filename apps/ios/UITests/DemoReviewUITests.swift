import XCTest

/// Opt-in, live review-environment acceptance. Never run against a production runtime.
/// The runner supplies a single-use pairing ticket, not the administrator token.
@MainActor
final class DemoReviewUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func required(_ suffix: String) throws -> String {
        let value = ProcessInfo.processInfo.environment["HOLON_DEMO_" + suffix] ?? ""
        guard !value.isEmpty else {
            XCTFail("Missing opt-in demo fixture: " + suffix)
            throw DemoFixtureError.missingInput
        }
        return value
    }

    private func reveal(_ element: XCUIElement, in app: XCUIApplication) {
        for _ in 0..<12 {
            if element.exists && element.isHittable { return }
            app.swipeUp()
        }
        XCTAssertTrue(element.exists && element.isHittable)
    }

    private func connectToDemo(_ app: XCUIApplication) throws {
        let endpoint = try required("ENDPOINT")
        let ticket = try required("PAIRING_TICKET")
        guard var components = URLComponents(string: endpoint),
              components.scheme == "https", components.host == "demo.holon.run",
              components.path == "/api", ticket.count == 64,
              ticket.allSatisfy(\.isHexDigit) else {
            XCTFail("This opt-in test requires the HTTPS review demo and a single-use pairing ticket.")
            throw DemoFixtureError.invalidInput
        }
        components.path = "/login"
        components.fragment = "pair=" + ticket
        let payload = try XCTUnwrap(components.string)
        app.launchArguments = ["-AppleLanguages", "(en)", "-AppleLocale", "en",
                               "-AppleInterfaceStyle", "Light"]
        app.launch()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: UIWaitScale.scaled(20)))
        // Single-screen flow: paste the pairing link into the address field.
        let address = app.textFields["onboarding.address"]
        reveal(address, in: app)
        address.tap()
        address.typeText(payload)
        XCTAssertEqual(address.value as? String, payload,
                       "Address field must hold the exact pairing link")
        let next = app.buttons["onboarding.checkAddress"]
        reveal(next, in: app)
        next.tap()
        let target = app.staticTexts["onboarding.pairingTarget"]
        XCTAssertTrue(target.waitForExistence(timeout: UIWaitScale.scaled(10)))
        XCTAssertTrue(target.label.hasPrefix(endpoint),
                      "Pairing preview must name the review demo host")
        let confirm = app.buttons["onboarding.confirmPairing"]
        reveal(confirm, in: app)
        XCTAssertTrue(confirm.isEnabled, "HTTPS pairing must not require insecure HTTP permission")
        confirm.tap()
        // Confirm stops at the sharing prompt before any connection activity.
        let consent = app.switches["privacy.accept"]
        XCTAssertTrue(consent.waitForExistence(timeout: UIWaitScale.scaled(30)),
                      "Confirm must stop at the sharing prompt before data")
        let providerCopy = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "DeepSeek")).firstMatch
        XCTAssertTrue(providerCopy.waitForExistence(timeout: UIWaitScale.scaled(10)),
                      "Demo consent copy must name the AI provider")
        reveal(consent, in: app)
        XCTAssertEqual(consent.value as? String, "0", "Consent must never be preselected")
        consent.switches.firstMatch.tap()
        XCTAssertEqual(consent.value as? String, "1")
        let proceed = app.buttons["privacy.continueLogin"]
        reveal(proceed, in: app)
        proceed.tap()
        XCTAssertTrue(app.buttons["settings.open"].waitForExistence(timeout: UIWaitScale.scaled(30)))
    }

    func testDemoReviewWorkflow() throws {
        let agentID = try required("AGENT_ID")
        let marker = try required("REPLY_MARKER")
        let app = XCUIApplication()
        defer { app.terminate() }
        try connectToDemo(app)

        app.buttons["settings.open"].tap()
        let review = app.buttons["privacy.review"]
        reveal(review, in: app)
        review.tap()
        let agree = app.alerts.buttons["privacy.agree"].firstMatch
        XCTAssertTrue(agree.waitForExistence(timeout: UIWaitScale.scaled(10)))
        agree.tap()
        XCTAssertTrue(app.buttons["privacy.revoke"].waitForExistence(timeout: UIWaitScale.scaled(10)))
        app.navigationBars.buttons.element(boundBy: 0).tap()

        let agent = app.buttons["agent." + agentID]
        XCTAssertTrue(agent.waitForExistence(timeout: UIWaitScale.scaled(30)))
        reveal(agent, in: app)
        agent.tap()
        let editor = app.descendants(matching: .any)["sending.text"].firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: UIWaitScale.scaled(15)))
        editor.tap()
        editor.typeText("This is a non-sensitive iOS Beta review connectivity test. "
            + "Do not use tools, execute commands, or read files. Reply with exactly: " + marker)
        let send = app.buttons["sending.enqueue"]
        reveal(send, in: app)
        XCTAssertTrue(send.isEnabled)
        send.tap()
        app.buttons["sending.options"].tap()
        app.buttons["sending.queue"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["sending.state.received"]
            .waitForExistence(timeout: UIWaitScale.scaled(30)), "Require an authoritative prompt receipt")
        app.buttons["Close"].tap()
        XCTAssertTrue(app.staticTexts[marker].waitForExistence(timeout: UIWaitScale.scaled(120)),
                      "Require the demo Agent's actual reply, not the outgoing prompt")

        app.navigationBars.buttons.element(boundBy: 0).tap()
        try deleteOnlyNetwork(in: app)
    }

    /// Complements an already verified reply without enqueueing another prompt.
    func testDemoReviewLoginAndDeleteOnly() throws {
        let agentID = try required("AGENT_ID")
        let app = XCUIApplication()
        defer { app.terminate() }
        try connectToDemo(app)
        XCTAssertTrue(app.buttons["agent." + agentID].waitForExistence(timeout: UIWaitScale.scaled(30)))
        try deleteOnlyNetwork(in: app)
    }

    private func deleteOnlyNetwork(in app: XCUIApplication) throws {
        let manage = app.buttons["connection.manage"]
        XCTAssertTrue(manage.waitForExistence(timeout: UIWaitScale.scaled(15)))
        manage.tap()
        let actions = app.buttons.matching(NSPredicate(format:
            "identifier BEGINSWITH %@", "profiles.actions.")).firstMatch
        reveal(actions, in: app)
        actions.tap()
        app.buttons["Delete network"].tap()
        let buttons = app.alerts.buttons.matching(identifier: "profiles.confirmDelete")
        XCTAssertTrue(buttons.firstMatch.waitForExistence(timeout: UIWaitScale.scaled(10)))
        // UIKit can expose a wrapper and child for one SwiftUI alert action.
        let deletion = buttons.allElementsBoundByIndex.filter { $0.buttons.count == 0 }
        XCTAssertEqual(deletion.count, 1, "There must be exactly one destructive action")
        try XCTUnwrap(deletion.first).tap()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: UIWaitScale.scaled(30)),
                      "Deleting the only network must remove the local sign-in session")
    }

    private enum DemoFixtureError: Error { case missingInput, invalidInput }
}
