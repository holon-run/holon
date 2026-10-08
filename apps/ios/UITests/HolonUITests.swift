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
        XCTAssertEqual(ProcessInfo.processInfo.environment["HOLON_UI_APPEARANCE"], dark ? "dark" : "light",
                       "The UI runner must configure and verify the system appearance")
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
            let magnitude = min(viewport.height * 0.3, max(96, abs(distance)))
            let delta = distance < 0 ? -magnitude : magnitude
            let start = app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: viewport.midX, dy: viewport.midY))
            // XCUI coordinates replace their offset rather than adding to it.
            // Keep x fixed so revealing a row is a genuinely vertical gesture.
            let end = app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: viewport.midX, dy: viewport.midY - delta))
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

    private func openSettings(_ app: XCUIApplication) {
        let settings = app.buttons["settings.open"]
        for _ in 0..<6 {
            if settings.waitForExistence(timeout: 3) { break }
            let back = app.navigationBars.buttons.element(boundBy: 0)
            XCTAssertTrue(back.waitForExistence(timeout: 10))
            back.tap()
        }
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settings.tap()
    }

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
        openSettings(app)
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
        openSettings(app)
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
        let bottom = app.tabBars.firstMatch.exists ? min(bounds.maxY, app.tabBars.firstMatch.frame.minY) : bounds.maxY
        XCTAssertGreaterThan(bottom, top)
        return CGRect(x: bounds.minX, y: top, width: bounds.width, height: bottom - top)
    }

    private func recordDiagnosticsViewport(_ app: XCUIApplication, name: String) {
        let elements = [("diagnostics.report", app.staticTexts["diagnostics.report"]),
                        ("diagnostics.export", app.buttons["diagnostics.export"]),
                        ("diagnostics.chooseAgent", app.staticTexts["diagnostics.chooseAgent"])]
        let detail = "content: \(app.scrollViews["diagnostics.content"].frame)\n"
            + "navigation: \(app.navigationBars.firstMatch.frame)\n"
            + "tab: \(app.tabBars.firstMatch.exists ? app.tabBars.firstMatch.frame : .zero)\nviewport: \(diagnosticsViewport(app))\n"
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
        openSettings(app)
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
        openSettings(app)
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
        XCTAssertTrue(app.buttons["settings.open"].waitForExistence(timeout: 30))
        XCTAssertFalse(app.tabBars.firstMatch.exists, "Agent home has no global bottom tabs")
        let manage = app.buttons["connection.manage"]
        XCTAssertTrue(manage.waitForExistence(timeout: 10))
        let originalHost = manage.label.components(separatedBy: ",").first ?? manage.label
        manage.tap()
        let add = app.buttons["connection.add"]
        reveal(add, in: app)
        add.tap()
        XCTAssertTrue(app.buttons["onboarding.scan"].waitForExistence(timeout: 10))
        let cancel = app.buttons["onboarding.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        cancel.tap()
        reveal(add, in: app)
        XCTAssertTrue(add.waitForExistence(timeout: 10), "Cancel returns to existing connection management")
        XCTAssertFalse(app.buttons["onboarding.scan"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(manage.waitForExistence(timeout: 10), "One back returns to Agent home")
        XCTAssertTrue(manage.waitForExistence(timeout: 10))
        XCTAssertEqual(manage.label.components(separatedBy: ",").first, originalHost,
                       "Cancelled onboarding preserves the original host independently of live/offline status")
        let agentButton = app.buttons["agent." + agent]
        XCTAssertTrue(agentButton.waitForExistence(timeout: 30))
        agentButton.tap()
        let read = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", readMarker)).firstMatch
        XCTAssertTrue(read.waitForExistence(timeout: 30))
        reveal(read, in: app)
        capture(app, "authenticated-reading")
        XCTAssertFalse(read.frame.isEmpty, "Assert margins only on a laid-out, visible element")
        XCTAssertTrue(read.isHittable)
        XCTAssertGreaterThanOrEqual(read.frame.minX, 12, "Conversation text must retain native horizontal margins")
        XCTAssertLessThanOrEqual(read.frame.maxX, app.windows.firstMatch.frame.maxX - 12)
        let editor = app.descendants(matching: .any)["sending.text"].firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        editor.tap()
        editor.typeText("P6 native UI explicit operator message")
        let enqueue = app.buttons["sending.enqueue"]
        reveal(enqueue, in: app)
        XCTAssertTrue(enqueue.isEnabled)
        enqueue.tap()
        // Returning from the native editor is not evidence of server acceptance.
        // Outbox must expose a concrete accepted state.
        let queue = app.buttons["sending.queue"]
        app.buttons["sending.options"].tap()
        XCTAssertTrue(queue.waitForExistence(timeout: 10))
        queue.tap()
        XCTAssertTrue(app.descendants(matching: .any)["sending.state.received"]
            .waitForExistence(timeout: 30))
        capture(app, "authenticated-outbox")
        app.buttons["Close"].tap()
        app.buttons["conversation.more"].tap()
        app.buttons["conversation.work"].tap()
        let item = app.buttons["work.item." + work]
        if ProcessInfo.processInfo.environment["HOLON_UI_RICH_TURN_ID"]?.isEmpty == false {
            let more = app.buttons["work.moreItems"]
            reveal(more, in: app); more.tap()
        }
        reveal(item, in: app)
        XCTAssertTrue(item.waitForExistence(timeout: 10))
        item.tap()
        let openPlan = app.buttons["work.openPlan"]
        reveal(openPlan, in: app)
        openPlan.tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", planMarker))
            .firstMatch.waitForExistence(timeout: 30))
        capture(app, "full-plan")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(openPlan.waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let taskRow = app.buttons["work.task." + task]
        reveal(taskRow, in: app)
        taskRow.tap()
        let output = app.staticTexts["work.output"]
        XCTAssertTrue(output.waitForExistence(timeout: 30))
        XCTAssertTrue(output.label.contains(taskMarker))
        capture(app, "task-output")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["conversation.more"].tap()
        let files = app.buttons["conversation.files"]
        reveal(files, in: app)
        files.tap()
        app.buttons["files.options"].tap()
        app.buttons["Server reference"].tap()
        let reference = app.textFields["files.reference"]
        XCTAssertTrue(reference.waitForExistence(timeout: 10))
        reference.tap()
        reference.typeText(fileReference)
        app.buttons["files.openReference"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", fileMarker))
            .firstMatch.waitForExistence(timeout: 30))
        capture(app, "file-preview")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.navigationBars["Files"].waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["conversation.more"].tap()
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

    func testRichActivityWorkflow() throws {
        let agent = try required("AGENT_ID")
        let turn = try required("RICH_TURN_ID")
        let app = launch(language: "en", dark: false, large: false)
        // Existing credentials came only from the preceding shipped onboarding flow.
        // Wait for confirmed route restoration, not the transient home while
        // roster authority is still loading after launch.
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["settings.open"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.tabBars.firstMatch.exists)
        app.swipeDown()
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10)); search.tap()
        search.typeText("ios-fixture-agent-089\n")
        let lastAgent = app.buttons["agent.ios-fixture-agent-089"]
        XCTAssertTrue(lastAgent.waitForExistence(timeout: 15)); lastAgent.tap()
        XCTAssertTrue(app.descendants(matching: .any)["sending.text"].firstMatch.waitForExistence(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(lastAgent.waitForExistence(timeout: 10), "One native back returns to the filtered Agent list")
        XCTAssertTrue(search.exists)
        XCTAssertFalse(app.buttons["conversation.more"].exists)
        search.tap()
        let clearSearch = search.buttons["Clear text"]
        XCTAssertTrue(clearSearch.waitForExistence(timeout: 10)); clearSearch.tap()
        search.typeText(agent + "\n")
        let agentRow = app.buttons["agent." + agent]
        XCTAssertTrue(agentRow.waitForExistence(timeout: 15)); agentRow.tap()
        app.buttons["sending.options"].tap(); app.buttons["Take photo"].tap()
        XCTAssertTrue(app.staticTexts["The camera is not available on this device. Choose a photo or file instead."].waitForExistence(timeout: 10))
        app.buttons["conversation.more"].tap(); app.buttons["conversation.work"].tap()
        let moreWork = app.buttons["work.moreItems"]
        reveal(moreWork, in: app); moreWork.tap()
        let additionalWork = app.buttons["work.item." + (try required("RICH_ADDITIONAL_WORK_ID"))]
        reveal(additionalWork, in: app)
        XCTAssertTrue(additionalWork.isHittable, "Load more exposes a real item outside the first fifty")
        capture(app, "rich-work-expanded-window")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        // Work returns to the remembered reading position, which can precede
        // this turn. Start at the shipped Latest action before seeking older.
        if app.buttons["conversation.latest"].exists { app.buttons["conversation.latest"].tap() }
        let activity = app.descendants(matching: .any)["activities." + turn].firstMatch
        // This completed turn precedes the baseline's freshly sent messages.
        for _ in 0..<40 {
            if activity.exists && activity.isHittable { break }
            let olderTurns = app.buttons["conversation.older"]
            if olderTurns.exists && olderTurns.isHittable { olderTurns.tap() }
            else { app.swipeDown() }
        }
        XCTAssertTrue(activity.isHittable); activity.tap()
        let fullProcess = app.buttons["activities.full." + turn]
        reveal(fullProcess, in: app); fullProcess.tap()
        let fullReader = app.scrollViews["activities.fullReader"]
        XCTAssertTrue(fullReader.waitForExistence(timeout: 10))
        let older = fullReader.buttons["activities.older"]
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(older.waitForExistence(timeout: 30),
                      "Open activity reader reacquires detail after a quiet foreground bootstrap")
        let firstBatch = fullReader.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "IOS_RICH_ASSISTANT: read-only inspection batch 1.")).firstMatch
        for _ in 0..<4 {
            if firstBatch.exists { break }
            for _ in 0..<20 {
                if older.exists { break }
                fullReader.swipeDown()
            }
            reveal(older, in: app)
            XCTAssertTrue(older.isEnabled); older.tap()
        }
        reveal(firstBatch, in: app)
        capture(app, "rich-activity-paged-to-first-batch")
        let assistant = fullReader.buttons["Full assistant text"].firstMatch
        reveal(assistant, in: app); assistant.tap()
        let rawRecord = fullReader.buttons["Raw record"].firstMatch
        XCTAssertTrue(rawRecord.waitForExistence(timeout: 20)); rawRecord.tap()
        XCTAssertTrue(fullReader.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "assistant_round"))
            .firstMatch.waitForExistence(timeout: 20), "Canonical transcript detail was fetched, not the summary fallback")
        capture(app, "rich-assistant-full-detail")
        rawRecord.tap()
        let tool = fullReader.buttons.containing(NSPredicate(format: "label CONTAINS %@", "GetAgent")).firstMatch
        reveal(tool, in: app); tool.tap()
        let input = fullReader.staticTexts["Input"].firstMatch
        XCTAssertTrue(input.waitForExistence(timeout: 20), "Canonical tool input is readable")
        capture(app, "rich-tool-detail")
        app.buttons["Close"].tap()
        app.terminate()
    }

    func testPopulatedComposerMaximumTextSize() throws {
        let app = launch(language: "zh-Hans", dark: true, large: true)
        defer { app.terminate() }
        // Earlier diagnostics cases intentionally return to home and clear
        // the saved Agent route. Wait for confirmed restoration before treating
        // the transient bootstrap home as a settled navigation destination.
        if !app.buttons["conversation.more"].waitForExistence(timeout: 30) {
            XCTAssertTrue(app.buttons["settings.open"].waitForExistence(timeout: 10))
            let agent = app.buttons["agent." + (try required("AGENT_ID"))]
            XCTAssertTrue(agent.waitForExistence(timeout: 15))
            reveal(agent, in: app); agent.tap()
        }
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        let editor = app.descendants(matching: .any)["sending.text"].firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 15)); XCTAssertTrue(editor.isHittable)
        editor.tap(); editor.typeText("IOS_ACCESSIBILITY_DRAFT")
        let send = app.buttons["sending.enqueue"]
        XCTAssertTrue(send.isHittable); XCTAssertTrue(send.isEnabled)
        XCTAssertGreaterThanOrEqual(send.frame.height, 44)
        let keyboard = app.keyboards.firstMatch
        XCTAssertTrue(keyboard.exists)
        var keyboardTop = keyboard.frame.minY
        let assistant = app.otherElements["SystemInputAssistantView"].firstMatch
        if assistant.exists && !assistant.frame.isEmpty { keyboardTop = min(keyboardTop, assistant.frame.minY) }
        XCTAssertLessThanOrEqual(send.frame.maxY, keyboardTop + 2)
        XCTAssertGreaterThan(editor.frame.minY, app.navigationBars.firstMatch.frame.maxY)
        capture(app, "chinese-dark-maximum-composer-keyboard")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["settings.open"].waitForExistence(timeout: 15), "One native back returns to Agents even with the keyboard open")
        capture(app, "chinese-dark-maximum-agent-home")
    }

    func testRichFilesWorkflow() throws {
        let directory = try required("RICH_DIRECTORY")
        let workspace = try required("WORKSPACE_ID")
        let app = launch(language: "en", dark: false, large: false)
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        app.buttons["conversation.more"].tap(); app.buttons["conversation.files"].tap()
        let home = app.buttons["files.workspace." + workspace].firstMatch
        reveal(home, in: app); home.tap()
        let folder = app.buttons["files.entry." + directory]
        reveal(folder, in: app); folder.tap()
        let lastNote = app.buttons["files.entry." + directory + "/note-079.txt"]
        reveal(lastNote, in: app)
        let savedY = lastNote.frame.minY
        lastNote.tap()
        XCTAssertTrue(app.staticTexts["IOS_RICH_NOTE_079\n"].waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(lastNote.waitForExistence(timeout: 10)); XCTAssertTrue(lastNote.isHittable)
        XCTAssertEqual(lastNote.frame.minY, savedY, accuracy: 44, "Reader return preserves native directory position")
        func open(_ name: String) {
            // Returning preserves the bottom-of-directory bookmark. Use the
            // shipped filter to select an earlier file rather than scrolling away.
            let filter = app.searchFields.firstMatch
            XCTAssertTrue(filter.waitForExistence(timeout: 10)); filter.tap()
            let clear = filter.buttons["Clear text"]
            if clear.exists { clear.tap() }
            filter.typeText(name + "\n")
            let row = app.buttons["files.entry." + directory + "/" + name]
            reveal(row, in: app); row.tap()
            XCTAssertTrue(app.descendants(matching: .any)["files.preview"].firstMatch.waitForExistence(timeout: 20))
        }
        open("large-utf8.txt")
        app.buttons["files.end"].tap()
        let end = app.staticTexts["files.endOfFile"]
        // Do not swipe to rescue a failed End action; the real footer must be on-screen.
        let endVisible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: end)
        XCTAssertEqual(XCTWaiter.wait(for: [endVisible], timeout: 20), .completed)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "IOS_RICH_TEXT_END"))
            .firstMatch.exists, "The final source bytes are loaded, not just a synthetic footer")
        XCTAssertEqual(app.staticTexts["files.textPageCount"].label, "42/42")
        XCTAssertGreaterThanOrEqual(end.frame.minX, 12)
        XCTAssertLessThanOrEqual(end.frame.maxX, app.windows.firstMatch.frame.maxX - 12)
        capture(app, "large-utf8-real-document-end")
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(end.waitForExistence(timeout: 30)); XCTAssertTrue(end.isHittable, "Foreground reauthorization restores the last page")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        open("source.ts"); app.buttons["files.end"].tap()
        let codeEnd = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: end)
        XCTAssertEqual(XCTWaiter.wait(for: [codeEnd], timeout: 30), .completed)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "IOS_RICH_CODE_END"))
            .firstMatch.exists)
        XCTAssertEqual(app.staticTexts["files.textPageCount"].label, "41/41")
        capture(app, "large-typescript-real-document-end")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        open("report.md")
        let relative = app.links["Open sibling"].firstMatch
        XCTAssertTrue(relative.waitForExistence(timeout: 20))
        // Verify the actual text bounds; native file navigation cannot be
        // substituted with a direct resolver call or an auxiliary button.
        XCTAssertGreaterThan(relative.frame.minY, app.navigationBars.firstMatch.frame.maxY)
        XCTAssertLessThan(relative.frame.maxY, app.windows.firstMatch.frame.maxY - 44)
        capture(app, "native-markdown-relative-file-link")
        relative.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(app.staticTexts["IOS_RICH_NOTE_079\n"].waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(relative.waitForExistence(timeout: 15), "Relative links return to their source file")
        let heading = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Native report")).firstMatch
        heading.press(forDuration: 1)
        let selectText = app.buttons["Select source text"]
        XCTAssertTrue(selectText.waitForExistence(timeout: 10)); selectText.tap()
        let selection = app.textViews["reading.selectionText"]
        XCTAssertTrue(selection.waitForExistence(timeout: 10))
        XCTAssertTrue((selection.value as? String)?.contains("[Open sibling](./note-079.txt)") == true)
        selection.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 45, dy: 25)).press(forDuration: 1)
        let copy = app.menuItems["Copy"].firstMatch
        XCTAssertTrue(copy.waitForExistence(timeout: 10), "Native range selection retains copy")
        capture(app, "native-markdown-text-selection")
        copy.tap()
        app.navigationBars.buttons["Close"].tap()
        XCTAssertTrue(relative.waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        open("image.png")
        let raster = app.descendants(matching: .any)["files.raster"].firstMatch
        XCTAssertTrue(raster.waitForExistence(timeout: 20)); XCTAssertTrue(raster.isHittable)
        capture(app, "native-image-preview")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        open("report.pdf")
        XCTAssertTrue(raster.waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["1/2"].exists)
        app.buttons["files.nextPage"].tap()
        XCTAssertTrue(app.staticTexts["2/2"].waitForExistence(timeout: 10))
        capture(app, "native-pdf-second-page")
        app.buttons["files.options"].tap(); app.buttons["Export a copy"].tap()
        let exportName = app.textFields["DOCPicker.filenameTextField"]
        XCTAssertTrue(exportName.waitForExistence(timeout: 15), "Export opens the native destination picker")
        XCTAssertEqual(exportName.value as? String, "report")
        let save = app.buttons["Save"].firstMatch
        XCTAssertTrue(save.isHittable); XCTAssertTrue(save.isEnabled)
        let exportHierarchy = XCTAttachment(string: app.debugDescription)
        exportHierarchy.name = "native-export-picker-hierarchy"; exportHierarchy.lifetime = .keepAlways
        add(exportHierarchy)
        capture(app, "native-file-export-destination")
        save.tap()
        XCTAssertTrue(app.staticTexts["Copy saved."].waitForExistence(timeout: 15),
                      "Native save must finish successfully, not only open a picker")
        XCTAssertTrue(app.buttons["files.options"].waitForExistence(timeout: 10))
        app.buttons["files.options"].tap(); app.buttons["Share a copy"].tap()
        XCTAssertTrue(app.otherElements["ActivityListView"].waitForExistence(timeout: 10), "Share hands the original file to the native system sheet")
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "report.pdf")).firstMatch.exists,
                      "Sharing preserves the original safe filename")
        capture(app, "native-original-file-share")
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30), "Process relaunch restores the confirmed Agent route")
        XCTAssertTrue(app.descendants(matching: .any)["sending.text"].firstMatch.exists)
        capture(app, "restored-confirmed-conversation")
        app.terminate()
    }

    func testConversationHistoryWindowPosition() throws {
        let app = launch(language: "en", dark: false, large: false)
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        if app.buttons["conversation.latest"].exists { app.buttons["conversation.latest"].tap() }
        let older = app.buttons["conversation.older"]
        // The lazy older-window control is above the initial latest position.
        for _ in 0..<40 {
            if older.exists { break }
            app.swipeDown()
        }
        reveal(older, in: app); older.tap()
        func assertTop(_ id: String) {
            let timestamp = app.staticTexts["conversation.time." + id]
            let visible = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: timestamp)
            XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 20), .completed)
            let top = app.navigationBars.firstMatch.frame.maxY
            XCTAssertGreaterThanOrEqual(timestamp.frame.minY, top - 2)
            XCTAssertLessThanOrEqual(timestamp.frame.minY, top + 50,
                "Window navigation must position its first turn, not just change the underlying range")
        }
        assertTop(try required("HISTORY_OLDER_TOP"))
        capture(app, "native-older-turn-window-position")
        let newer = app.buttons["conversation.newer"]
        reveal(newer, in: app); newer.tap()
        assertTop(try required("HISTORY_NEWER_TOP"))
        capture(app, "native-newer-turn-window-position")
    }

    func testLostResponseAndProcessRecovery() async throws {
        let marker = "IOS_LOST_RESPONSE_SEND"
        let app = launch(language: "en", dark: false, large: false)
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        let editor = app.descendants(matching: .any)["sending.text"].firstMatch
        editor.tap(); editor.typeText(marker)
        app.buttons["sending.enqueue"].tap()
        func openQueue() {
            app.buttons["sending.options"].tap(); app.buttons["sending.queue"].tap()
        }
        openQueue()
        let unknown = app.staticTexts["sending.state.unknown"]
        XCTAssertTrue(unknown.waitForExistence(timeout: 30))
        let row = app.cells.containing(.staticText, identifier: marker).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        let uuid = row.staticTexts.matching(NSPredicate(format:
            "label MATCHES %@", "[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}")).firstMatch.label
        XCTAssertNotNil(UUID(uuidString: uuid))
        capture(app, "lost-response-unknown-immutable-request")
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["conversation.more"].waitForExistence(timeout: 30))
        openQueue()
        XCTAssertTrue(unknown.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts[uuid].exists, "Process death retains the original UUID")
        var request = URLRequest(url: try XCTUnwrap(URL(string: required("LOSS_CONTROL_URL"))))
        request.httpMethod = "POST"
        request.setValue("Bearer " + (try required("LOSS_CONTROL_TOKEN")), forHTTPHeaderField: "Authorization")
        let (_, response) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
        let entry = app.cells.containing(.staticText, identifier: marker).firstMatch
        let retry = app.buttons["sending.retry." + uuid]
        XCTAssertTrue(retry.waitForExistence(timeout: 10))
        XCTAssertTrue(retry.isEnabled, "The original request must be explicitly retryable after restoration")
        let queueGeometry = XCTAttachment(string: app.debugDescription)
        queueGeometry.name = "lost-response-restored-queue-controls"
        queueGeometry.lifetime = .keepAlways; add(queueGeometry)
        reveal(retry, in: app); retry.tap()
        XCTAssertTrue(entry.staticTexts["sending.state.received"].waitForExistence(timeout: 30))
        XCTAssertTrue(app.staticTexts[uuid].exists)
        capture(app, "lost-response-explicit-retry-received")
        app.terminate()
    }
}
