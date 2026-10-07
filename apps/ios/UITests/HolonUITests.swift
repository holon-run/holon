import XCTest

/// Drives only the shipped UI. Fixture credentials never enter app launch arguments.
@MainActor
final class HolonUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func launch(language: String, dark: Bool, large: Bool) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-AppleLanguages", "(\(language))", "-AppleLocale", language,
                               "-AppleInterfaceStyle", dark ? "Dark" : "Light"]
        // The runner verifies the real simulator preference, not an app-only override.
        XCTAssertEqual(ProcessInfo.processInfo.environment["HOLON_UI_CONTENT_SIZE"],
                       large ? "accessibility-extra-extra-extra-large" : "large",
                       "The UI runner must configure and verify the system text size")
        app.launch()
        return app
    }

    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func reveal(_ element: XCUIElement, in app: XCUIApplication,
                        fullyVisible: Bool = false,
                        file: StaticString = #filePath, line: UInt = #line) {
        for _ in 0..<40 {
            let window = app.windows.firstMatch.frame
            let navigation = app.navigationBars.firstMatch
            let tabs = app.tabBars.firstMatch
            let keyboard = app.keyboards.firstMatch
            let top = navigation.exists ? max(window.minY, navigation.frame.maxY) : window.minY
            var bottom = tabs.exists ? min(window.maxY, tabs.frame.minY) : window.maxY
            if keyboard.exists {
                bottom = min(bottom, keyboard.frame.minY)
                // The keyboard's AX frame excludes its input assistant overlay.
                let assistant = app.otherElements["SystemInputAssistantView"].firstMatch
                if assistant.exists && !assistant.frame.isEmpty {
                    bottom = min(bottom, assistant.frame.minY)
                }
            }
            guard bottom > top else { break }
            let viewport = CGRect(x: window.minX, y: top, width: window.width, height: bottom - top)
            if element.exists && element.isHittable
                && (!fullyVisible || (!element.frame.isEmpty && viewport.contains(element.frame))) {
                return
            }
            // Form rows are lazy; small drags load them without skipping the target.
            var distance = viewport.height * 0.3
            if element.exists {
                let frame = element.frame
                // Lazy rows may exist before their frame is laid out.
                if frame.isEmpty {
                    distance = viewport.height * 0.3
                } else if frame.maxY > viewport.maxY {
                    distance = frame.maxY - viewport.maxY + 8
                } else if frame.minY < viewport.minY {
                    distance = frame.minY - viewport.minY - 8
                }
            }
            let magnitude = min(viewport.height * 0.3, max(32, abs(distance)))
            let delta = distance < 0 ? -magnitude : magnitude
            let start = app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: viewport.midX, dy: viewport.midY))
            let end = start.withOffset(CGVector(dx: 0, dy: -delta))
            start.press(forDuration: 0.05, thenDragTo: end, withVelocity: .slow,
                        thenHoldForDuration: 0.1)
        }
        if fullyVisible {
            capture(app, "unreachable-control")
            XCTFail("Control must be fully visible before navigation", file: file, line: line)
            return
        }
        if !element.exists || !element.isHittable { capture(app, "unreachable-control") }
        XCTAssertTrue(element.waitForExistence(timeout: 10), file: file, line: line)
        XCTAssertTrue(element.isHittable, file: file, line: line)
    }

    private func required(_ key: String) throws -> String {
        let value = ProcessInfo.processInfo.environment["HOLON_UI_" + key] ?? ""
        guard !value.isEmpty, !value.hasPrefix("$(") else {
            XCTFail("Missing fixture input HOLON_UI_\(key); authenticated coverage cannot skip.")
            throw FixtureError.missingInput
        }
        return value
    }

    private enum FixtureError: Error { case missingInput, invalidEndpoint }

    private func openDiagnostics(_ app: XCUIApplication) {
        let open = app.buttons["diagnostics.open"]
        reveal(open, in: app, fullyVisible: true)
        open.tap()
        XCTAssertTrue(app.buttons["diagnostics.prepare"].waitForExistence(timeout: 10))
    }

    func testDisconnectedEnglishLight() throws {
        try disconnected(language: "en", dark: false, large: false)
    }

    func testDisconnectedChineseDarkAccessibilitySize() throws {
        try disconnected(language: "zh-Hans", dark: true, large: true)
    }

    func testChineseDiagnosticsDarkAccessibilitySize() throws {
        let app = launch(language: "zh-Hans", dark: true, large: true)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["设置"].tap()
        openDiagnostics(app)
        let allowlist = app.staticTexts[
            "仅包含连接状态与数量，不包含凭据、身份、地址、消息内容或原始错误。"]
        let prepare = app.buttons["diagnostics.prepare"]
        for element in [allowlist, prepare] {
            reveal(element, in: app, fullyVisible: true)
            XCTAssertTrue(element.exists)
            XCTAssertGreaterThan(element.frame.height, 0)
        }
        XCTAssertEqual(prepare.label, "生成诊断")
        capture(app, "zh-Hans-maximum-diagnostics-controls")
        try audit(app, name: "zh-Hans-maximum-diagnostics-controls")
    }

    func testPreparedDiagnosticsViewportCoverage() throws {
        let app = launch(language: "en", dark: false, large: true)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Settings"].tap()
        openDiagnostics(app)
        app.buttons["diagnostics.prepare"].tap()
        XCTAssertTrue(app.staticTexts["diagnostics.report"].waitForExistence(timeout: 10))
        continueAfterFailure = true
        defer { continueAfterFailure = false }

        recordDiagnosticsViewport(app, name: "large-diagnostics-top")
        try audit(app, name: "large-diagnostics-top")
        for element in [app.staticTexts["diagnostics.report"],
                        app.buttons["diagnostics.export"],
                        app.staticTexts["diagnostics.chooseAgent"]] {
            XCTAssertTrue(element.waitForExistence(timeout: 10))
            for _ in 0..<20 {
                let viewport = diagnosticsViewport(app)
                let frame = element.frame
                let fullRowFits = frame.height <= viewport.height
                let topIsVisible = !fullRowFits || frame.minY >= viewport.minY
                if element.isHittable && topIsVisible && frame.maxY <= viewport.maxY
                    && frame.maxY >= viewport.minY { break }
                // Bound each drag by the remaining distance; full swipes overshoot short rows.
                let distance = frame.maxY > viewport.maxY
                    ? frame.maxY - viewport.maxY + 8
                    : frame.minY - viewport.minY - 8
                // Near-edge 10pt drags can leave the scroll position unchanged.
                let magnitude = min(viewport.height * 0.4, max(32, abs(distance)))
                let delta = distance < 0 ? -magnitude : magnitude
                let start = app.coordinate(withNormalizedOffset: .zero)
                    .withOffset(CGVector(dx: viewport.midX, dy: viewport.midY))
                let end = start.withOffset(CGVector(dx: 0, dy: -delta))
                start.press(forDuration: 0.05, thenDragTo: end, withVelocity: .slow,
                            thenHoldForDuration: 0.1)
            }
            guard element.exists else {
                XCTFail("Diagnostic row must remain reachable")
                continue
            }
            let viewport = diagnosticsViewport(app)
            XCTAssertTrue(element.isHittable)
            XCTAssertLessThanOrEqual(element.frame.maxY, viewport.maxY)
            XCTAssertGreaterThanOrEqual(element.frame.maxY, viewport.minY)
            // A long report may exceed one screen; its end must remain reachable.
            if element.frame.height <= viewport.height {
                XCTAssertGreaterThanOrEqual(element.frame.minY, viewport.minY)
            }
            recordDiagnosticsViewport(app, name: "large-revealed-\(element.identifier)")
        }
        try audit(app, name: "large-diagnostics-revealed")
    }

    private func diagnosticsViewport(_ app: XCUIApplication) -> CGRect {
        let content = app.scrollViews["diagnostics.content"]
        XCTAssertTrue(content.exists)
        let bounds = content.frame.intersection(app.windows.firstMatch.frame)
        let top = max(bounds.minY, app.navigationBars.firstMatch.frame.maxY)
        let bottom = min(bounds.maxY, app.tabBars.firstMatch.frame.minY)
        XCTAssertGreaterThan(bottom, top)
        return CGRect(x: bounds.minX, y: top, width: bounds.width, height: bottom - top)
    }

    private func recordDiagnosticsViewport(_ app: XCUIApplication, name: String) {
        let elements = [("diagnostics.report", app.staticTexts["diagnostics.report"]),
                        ("diagnostics.export", app.buttons["diagnostics.export"]),
                        ("diagnostics.chooseAgent", app.staticTexts["diagnostics.chooseAgent"])]
        let detail = "content: \(app.scrollViews["diagnostics.content"].frame)\n"
            + "navigation: \(app.navigationBars.firstMatch.frame)\n"
            + "tab: \(app.tabBars.firstMatch.frame)\nviewport: \(diagnosticsViewport(app))\n"
            + elements.map { identifier, element in
                element.exists
                    ? "\(identifier): \(element.frame), hittable: \(element.isHittable)"
                    : "\(identifier): offscreen row not loaded"
            }
                .joined(separator: "\n")
        let attachment = XCTAttachment(string: detail)
        attachment.name = name + "-viewport"
        attachment.lifetime = .keepAlways
        add(attachment)
        print("Diagnostics viewport \(name):\n\(detail)")
        capture(app, name)
    }

    func testPreparedDiagnosticsRespondToRuntimeTextSize() async throws {
        let app = launch(language: "en", dark: false, large: false)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Settings"].tap()
        openDiagnostics(app)
        app.buttons["diagnostics.prepare"].tap()
        let report = app.staticTexts["diagnostics.report"]
        let export = app.buttons["diagnostics.export"]
        let chooseAgent = app.staticTexts["diagnostics.chooseAgent"]
        XCTAssertTrue(report.waitForExistence(timeout: 10))
        let elements = [report, export, chooseAgent]
        let originalHeights = elements.map { element in
            reveal(element, in: app)
            XCTAssertTrue(element.exists)
            XCTAssertGreaterThan(element.frame.height, 0, "Runtime size requires a real baseline")
            return element.frame.height
        }
        let originalReport = report.label
        capture(app, "prepared-diagnostics-runtime-ordinary-size")

        // The fixture changes and reads back the system preference, not an app override.
        XCTAssertEqual(app.state, .runningForeground)
        try await changeSystemTextSize("accessibility-extra-extra-extra-large")
        XCTAssertEqual(app.state, .runningForeground)
        XCTAssertEqual(report.label, originalReport)
        for (element, originalHeight) in zip(elements, originalHeights) {
            reveal(element, in: app)
            let changed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                element.exists && element.frame.height > originalHeight
            }, object: nil)
            let result = await XCTWaiter.fulfillment(of: [changed], timeout: 10)
            XCTAssertEqual(result, .completed)
            XCTAssertGreaterThan(element.frame.height, originalHeight,
                                 "\(element.identifier) must respond without regenerating the report")
        }
        XCTAssertEqual(report.label, originalReport)
        XCTAssertEqual(app.state, .runningForeground)
        capture(app, "prepared-diagnostics-runtime-accessibility-size")
    }

    func testDiagnosticsControlsRespondToRuntimeTextSize() async throws {
        let app = launch(language: "en", dark: false, large: true)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Settings"].tap()
        openDiagnostics(app)
        let allowlist = app.staticTexts[
            "Only connection states and counts are included. Credentials, identities, addresses, message content and raw errors are excluded."]
        let prepare = app.buttons["diagnostics.prepare"]
        XCTAssertTrue(allowlist.waitForExistence(timeout: 10))
        let elements = [allowlist, prepare]
        let maximumHeights = elements.map { element in
            reveal(element, in: app)
            XCTAssertTrue(element.exists)
            XCTAssertGreaterThan(element.frame.height, 0, "Runtime size requires a real baseline")
            return element.frame.height
        }
        capture(app, "diagnostics-controls-maximum-size")

        XCTAssertEqual(app.state, .runningForeground)
        try await changeSystemTextSize("large")
        XCTAssertEqual(app.state, .runningForeground)
        for (element, maximumHeight) in zip(elements, maximumHeights) {
            XCTAssertTrue(element.exists)
            let changed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
                element.exists && element.frame.height > 0 && element.frame.height < maximumHeight
            }, object: nil)
            let result = await XCTWaiter.fulfillment(of: [changed], timeout: 10)
            XCTAssertEqual(result, .completed)
            XCTAssertLessThan(element.frame.height, maximumHeight,
                              "\(element.label) must respond to the real system text size")
        }
        XCTAssertEqual(app.state, .runningForeground)
        capture(app, "diagnostics-controls-ordinary-size")
    }

    private func changeSystemTextSize(_ category: String) async throws {
        let url = try XCTUnwrap(URL(string: required("TEXT_SIZE_URL")))
        XCTAssertEqual(url.scheme, "http")
        XCTAssertEqual(url.host, "127.0.0.1")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 40
        request.setValue("Bearer " + (try required("TEXT_SIZE_TOKEN")), forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["category": category])
        let (data, response) = try await URLSession.shared.data(for: request)
        let http = try XCTUnwrap(response as? HTTPURLResponse)
        XCTAssertEqual(http.statusCode, 200, "Real system text-size setter and readback must succeed")
        struct VerifiedSize: Decodable { let category: String }
        let verified = try JSONDecoder().decode(VerifiedSize.self, from: data)
        XCTAssertEqual(verified.category, category, "Acknowledgement must contain the actual system category")
        let attachment = XCTAttachment(string: "Verified system content size: \(verified.category)")
        attachment.name = "system-content-size-readback"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func disconnected(language: String, dark: Bool, large: Bool) throws {
        let app = launch(language: language, dark: dark, large: large)
        defer { app.terminate() }
        let chinese = language == "zh-Hans"
        let scan = app.buttons["onboarding.scan"]
        XCTAssertTrue(scan.waitForExistence(timeout: 15))
        XCTAssertEqual(scan.label, chinese ? "扫码连接" : "Scan to connect")
        XCTAssertFalse(app.tabBars.firstMatch.exists, "No empty content tabs before authentication")
        capture(app, "\(language)-welcome")
        try audit(app, name: language)
        let manual = app.buttons["onboarding.manual"]
        reveal(manual, in: app)
        manual.tap()
        let address = app.textFields["onboarding.address"]
        XCTAssertTrue(address.waitForExistence(timeout: 10))
        address.tap()
        address.typeText("http://host.example.test:8787")
        let permission = app.switches["onboarding.allowHTTP"]
        reveal(permission, in: app)
        XCTAssertEqual(permission.value as? String, "0")
        let checkAddress = app.buttons["onboarding.checkAddress"]
        reveal(checkAddress, in: app)
        XCTAssertFalse(checkAddress.isEnabled)
        app.buttons["onboarding.back"].tap()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.tabBars.firstMatch.exists)
    }

    func testPairingPreviewStaysOfflineAndCanCancel() throws {
        let app = launch(language: "en", dark: false, large: false)
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 15))
        let pasteEntry = app.staticTexts["onboarding.pasteEntry"]
        reveal(pasteEntry, in: app)
        pasteEntry.tap()
        let payload = app.secureTextFields["onboarding.payload"]
        reveal(payload, in: app)
        payload.tap()
        payload.typeText("http://pair.example.test/login#pair=" + String(repeating: "a", count: 64))
        app.buttons["onboarding.preview"].tap()
        let target = app.staticTexts["onboarding.pairingTarget"]
        XCTAssertTrue(target.waitForExistence(timeout: 10))
        XCTAssertEqual(target.label, "http://pair.example.test/api/")
        XCTAssertFalse(app.buttons["onboarding.confirmPairing"].isEnabled)
        XCTAssertFalse(app.tabBars.firstMatch.exists)
        app.buttons["onboarding.back"].tap()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 10))
        XCTAssertFalse(target.exists)
    }

    private func audit(_ app: XCUIApplication, name: String) throws {
        // Collect every finding without suppressing or downgrading any audit failure.
        let previousContinueAfterFailure = continueAfterFailure
        continueAfterFailure = true
        defer { continueAfterFailure = previousContinueAfterFailure }
        try app.performAccessibilityAudit(for: [.elementDetection, .sufficientElementDescription,
                                                 .hitRegion, .dynamicType, .textClipped]) { issue in
            let detail = "\(issue.compactDescription)\n\(issue.detailedDescription)\n"
                + (issue.element?.debugDescription ?? "No associated element")
            let attachment = XCTAttachment(string: detail)
            attachment.name = "\(name)-accessibility-issue"
            attachment.lifetime = .keepAlways
            self.add(attachment)
            print("Accessibility audit finding: \(detail)")
            return false
        }
    }

    private func settlePreparedDiagnostics(_ app: XCUIApplication, prepare: XCUIElement) throws {
        let report = app.staticTexts["diagnostics.report"]
        let export = app.buttons["diagnostics.export"]
        let chooseAgent = app.staticTexts["diagnostics.chooseAgent"]
        let elements = [prepare, report, export, chooseAgent]
        for element in elements {
            XCTAssertTrue(element.waitForExistence(timeout: 10))
        }
        let startingFrames = elements.map(\.frame)
        let viewportBottom = app.tabBars.firstMatch.frame.minY
        for element in [export, chooseAgent] {
            for _ in 0..<12 {
                if element.isHittable && element.frame.maxY <= viewportBottom { break }
                app.swipeUp()
            }
            XCTAssertTrue(element.isHittable)
            XCTAssertLessThanOrEqual(element.frame.maxY, viewportBottom)
        }
        let revealedFrames = elements.map(\.frame)
        for _ in 0..<12 {
            if abs(prepare.frame.minY - startingFrames[0].minY) < 1 { break }
            app.swipeDown()
        }
        var previousFrames: [CGRect] = []
        var previousLabels: [String] = []
        var stableSamples = 0
        let stable = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let frames = elements.map(\.frame)
            let labels = elements.map(\.label)
            if frames == previousFrames && labels == previousLabels {
                stableSamples += 1
            } else {
                stableSamples = 0
            }
            previousFrames = frames
            previousLabels = labels
            return stableSamples >= 2 && elements.allSatisfy { $0.exists && !$0.frame.isEmpty }
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [stable], timeout: 15), .completed)
        let restoredFrames = elements.map(\.frame)
        for (starting, restored) in zip(startingFrames, restoredFrames) {
            XCTAssertEqual(starting.minY, restored.minY, accuracy: 1)
            XCTAssertEqual(starting.height, restored.height, accuracy: 1)
        }
        let attachment = XCTAttachment(string: "start: \(startingFrames)\n"
            + "revealed: \(revealedFrames)\nrestored: \(restoredFrames)\n"
            + "labels: \(previousLabels)")
        attachment.name = "prepared-diagnostics-layout-control"
        attachment.lifetime = .keepAlways
        add(attachment)
        print("Prepared diagnostics layout control: \(attachment.name)")
    }

    func testAuthenticatedNativeWorkflow() throws {
        let endpoint = try required("ENDPOINT")
        let code = try required("PAIRING_CODE")
        let agent = try required("AGENT_ID")
        let work = try required("WORK_ID")
        let task = try required("TASK_ID")
        let readMarker = try required("READ_MARKER")
        let planMarker = try required("PLAN_MARKER")
        let taskMarker = try required("TASK_MARKER")
        let fileReference = try required("FILE_REFERENCE")
        let fileMarker = try required("FILE_MARKER")
        guard endpoint.hasSuffix("/api"),
              var components = URLComponents(string: endpoint),
              code.count == 64, code.allSatisfy(\.isHexDigit) else {
            XCTFail("Fixture endpoint must end /api and pairing code must be 64 hex characters.")
            throw FixtureError.invalidEndpoint
        }
        components.path = String(components.path.dropLast(4)) + "/login"
        components.fragment = "pair=" + code
        let app = launch(language: "en", dark: false, large: false)
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.tabBars.firstMatch.exists, "Identity must be confirmed before showing tabs")
        let pasteEntry = app.staticTexts["onboarding.pasteEntry"]
        reveal(pasteEntry, in: app)
        pasteEntry.tap()
        let payload = app.secureTextFields["onboarding.payload"]
        reveal(payload, in: app)
        payload.tap()
        payload.typeText(try XCTUnwrap(components.string))
        payload.typeText("\n")
        let keyboardDismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"), object: app.keyboards.firstMatch)
        XCTAssertEqual(XCTWaiter.wait(for: [keyboardDismissed], timeout: 5), .completed)
        app.buttons["onboarding.preview"].tap()
        let target = app.staticTexts["onboarding.pairingTarget"]
        XCTAssertTrue(target.waitForExistence(timeout: 10))
        XCTAssertEqual(target.label, endpoint + "/")
        XCTAssertFalse(app.tabBars.firstMatch.exists)
        let confirm = app.buttons["onboarding.confirmPairing"]
        XCTAssertFalse(confirm.isEnabled, "HTTP pairing requires explicit per-target consent")
        let permission = app.switches["onboarding.pairingHTTP"]
        reveal(permission, in: app)
        XCTAssertEqual(permission.value as? String, "0")
        permission.switches.firstMatch.tap()
        XCTAssertEqual(permission.value as? String, "1")
        reveal(confirm, in: app)
        XCTAssertTrue(confirm.isEnabled)
        confirm.tap()
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 30))
        XCTAssertEqual(app.tabBars.buttons.count, 3)
        for tab in ["Agents", "Work", "Settings"] {
            XCTAssertTrue(app.tabBars.buttons[tab].exists)
        }
        XCTAssertTrue(app.tabBars.buttons["Agents"].isSelected, "Successful pairing defaults to Agents")
        let manage = app.buttons["connection.manage"]
        XCTAssertTrue(manage.waitForExistence(timeout: 10))
        let originalHost = manage.label
        manage.tap()
        let add = app.buttons["connection.add"]
        reveal(add, in: app)
        add.tap()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 10))
        let cancel = app.buttons["onboarding.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        cancel.tap()
        XCTAssertTrue(add.waitForExistence(timeout: 10), "Cancel returns to existing connection management")
        XCTAssertFalse(app.buttons["onboarding.scan"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["settings.connection"].waitForExistence(timeout: 10),
                      "Back returns from connection management to the Settings home")
        app.tabBars.buttons["Agents"].tap()
        XCTAssertTrue(manage.waitForExistence(timeout: 10))
        XCTAssertEqual(manage.label, originalHost, "Cancelled onboarding preserves the original host")
        let connected = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label == %@", "Live"),
            object: app.staticTexts["reading.status"])
        XCTAssertEqual(XCTWaiter.wait(for: [connected], timeout: 30), .completed)
        let agentButton = app.buttons["agent." + agent]
        XCTAssertTrue(agentButton.waitForExistence(timeout: 30))
        agentButton.tap()
        let read = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", readMarker)).firstMatch
        XCTAssertTrue(read.waitForExistence(timeout: 30))
        capture(app, "authenticated-reading")
        let draft = app.buttons["sending.draft"]
        draft.tap()
        let editor = app.textViews["sending.text"]
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        editor.tap()
        editor.typeText("P6 native UI explicit operator message")
        let enqueue = app.buttons["sending.enqueue"]
        reveal(enqueue, in: app)
        XCTAssertTrue(enqueue.isEnabled)
        enqueue.tap()
        app.buttons["Close"].tap()
        // Returning from the native editor is not evidence of server acceptance.
        // Outbox must expose a concrete accepted state.
        let queue = app.buttons["sending.queue"]
        XCTAssertTrue(queue.waitForExistence(timeout: 10))
        queue.tap()
        XCTAssertTrue(app.descendants(matching: .any)["sending.state.received"]
            .waitForExistence(timeout: 30))
        capture(app, "authenticated-outbox")
        app.buttons["Close"].tap()
        app.tabBars.buttons["Work"].tap()
        let item = app.buttons["work.item." + work]
        XCTAssertTrue(item.waitForExistence(timeout: 30))
        item.tap()
        let openPlan = app.buttons["work.openPlan"]
        reveal(openPlan, in: app)
        openPlan.tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", planMarker))
            .firstMatch.waitForExistence(timeout: 30))
        capture(app, "full-plan")
        let dismissFiles = app.buttons["files.dismiss"]
        XCTAssertTrue(dismissFiles.waitForExistence(timeout: 10))
        dismissFiles.tap()
        XCTAssertTrue(app.tabBars.buttons["Work"].isSelected)
        XCTAssertTrue(openPlan.waitForExistence(timeout: 10))
        XCTAssertFalse(dismissFiles.exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let taskRow = app.buttons["work.task." + task]
        reveal(taskRow, in: app)
        taskRow.tap()
        let output = app.staticTexts["work.output"]
        XCTAssertTrue(output.waitForExistence(timeout: 30))
        XCTAssertTrue(output.label.contains(taskMarker))
        capture(app, "task-output")
        app.tabBars.buttons["Settings"].tap()
        for entry in ["settings.connection", "settings.files", "settings.tools", "diagnostics.open"] {
            XCTAssertTrue(app.buttons[entry].exists)
        }
        let files = app.buttons["settings.files"]
        reveal(files, in: app)
        files.tap()
        let reference = app.textFields["files.reference"]
        XCTAssertTrue(reference.waitForExistence(timeout: 10))
        reference.tap()
        reference.typeText(fileReference)
        app.buttons["files.openReference"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", fileMarker))
            .firstMatch.waitForExistence(timeout: 30))
        capture(app, "file-preview")
        let closePreview = app.buttons["Close preview"]
        XCTAssertTrue(closePreview.waitForExistence(timeout: 10))
        closePreview.tap()
        XCTAssertTrue(reference.waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.tabBars.buttons["Settings"].tap()
        openDiagnostics(app)
        app.buttons["diagnostics.prepare"].tap()
        XCTAssertTrue(app.staticTexts["diagnostics.report"].waitForExistence(timeout: 10))
        let send = app.buttons["diagnostics.send"]
        reveal(send, in: app)
        send.tap()
        let confirmButtons = app.sheets.buttons.matching(identifier: "diagnostics.confirmSend")
        XCTAssertTrue(confirmButtons.firstMatch.waitForExistence(timeout: 10))
        // A native sheet can expose the action as a button wrapper and child.
        // Require one leaf action rather than accepting arbitrary duplicate controls.
        let leafButtons = confirmButtons.allElementsBoundByIndex.filter { $0.buttons.count == 0 }
        XCTAssertEqual(leafButtons.count, 1)
        let confirmSend = try XCTUnwrap(leafButtons.first)
        XCTAssertTrue(confirmSend.isHittable)
        capture(app, "diagnostics-explicit-confirmation")
        confirmSend.tap()
        XCTAssertTrue(app.staticTexts[
            "Queued independently; your editor draft is unchanged. Check the Agent's sending queue for receipt."
        ].waitForExistence(timeout: 10))
        app.terminate()
    }
}
