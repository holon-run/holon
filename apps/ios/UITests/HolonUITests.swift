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
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        XCTAssertTrue(app.buttons["diagnostics.prepare"].waitForExistence(timeout: 10))
    }

    func testDisconnectedEnglishLight() throws {
        try disconnected(language: "en", dark: false, large: false)
    }

    func testDisconnectedChineseDarkAccessibilitySize() throws {
        try disconnected(language: "zh-Hans", dark: true, large: true)
    }

    func testPreparedDiagnosticsViewportCoverage() throws {
        let app = launch(language: "en", dark: false, large: true)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Tools"].tap()
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

    func testPreparedDiagnosticsRespondToRuntimeTextSize() throws {
        let app = launch(language: "en", dark: false, large: false)
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Tools"].tap()
        openDiagnostics(app)
        app.buttons["diagnostics.prepare"].tap()
        let report = app.staticTexts["diagnostics.report"]
        let export = app.buttons["diagnostics.export"]
        let chooseAgent = app.staticTexts["diagnostics.chooseAgent"]
        XCTAssertTrue(report.waitForExistence(timeout: 10))
        let elements = [report, export, chooseAgent]
        let originalHeights = elements.map { $0.frame.height }
        let originalReport = report.label

        // Change the real system preference while the prepared view is alive.
        let settings = openLargerTextSettings()
        let largerSizes = settings.switches.firstMatch
        XCTAssertTrue(largerSizes.waitForExistence(timeout: 10))
        let wasEnabled = largerSizes.value as? String == "1"
        if !wasEnabled { largerSizes.tap() }
        let slider = settings.sliders.firstMatch
        XCTAssertTrue(slider.waitForExistence(timeout: 10))
        slider.adjust(toNormalizedSliderPosition: 1)
        defer {
            settings.activate()
            slider.adjust(toNormalizedSliderPosition: 0.25)
            if !wasEnabled { largerSizes.tap() }
            settings.terminate()
            app.terminate()
        }
        app.activate()
        XCTAssertEqual(report.label, originalReport)
        for (element, originalHeight) in zip(elements, originalHeights) {
            reveal(element, in: app)
            XCTAssertGreaterThan(element.frame.height, originalHeight,
                                 "\(element.identifier) must respond without regenerating the report")
        }
        capture(app, "prepared-diagnostics-runtime-accessibility-size")
    }

    func testDiagnosticsControlsRespondToRuntimeTextSize() throws {
        let app = launch(language: "en", dark: false, large: true)
        defer { app.terminate() }
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Tools"].tap()
        openDiagnostics(app)
        let allowlist = app.staticTexts[
            "Only connection states and counts are included. Credentials, identities, addresses, message content and raw errors are excluded."]
        let prepare = app.buttons["diagnostics.prepare"]
        XCTAssertTrue(allowlist.waitForExistence(timeout: 10))
        let elements = [allowlist, prepare]
        let maximumHeights = elements.map { $0.frame.height }
        capture(app, "diagnostics-controls-maximum-size")

        let settings = openLargerTextSettings()
        let largerSizes = settings.switches.firstMatch
        let slider = settings.sliders.firstMatch
        XCTAssertTrue(largerSizes.waitForExistence(timeout: 10))
        XCTAssertTrue(slider.waitForExistence(timeout: 10))
        let wasEnabled = largerSizes.value as? String == "1"
        let originalValue = try XCTUnwrap(slider.value as? String)
        let originalPercentage = try XCTUnwrap(Double(originalValue.replacingOccurrences(of: "%", with: "")))
        let originalPosition = CGFloat(originalPercentage / 100)
        defer {
            settings.activate()
            if (largerSizes.value as? String == "1") != wasEnabled { largerSizes.tap() }
            slider.adjust(toNormalizedSliderPosition: originalPosition)
            settings.terminate()
        }
        if wasEnabled { largerSizes.tap() }
        slider.adjust(toNormalizedSliderPosition: 0.5)
        app.activate()
        for (element, maximumHeight) in zip(elements, maximumHeights) {
            XCTAssertLessThan(element.frame.height, maximumHeight,
                              "\(element.label) must respond to the real system text size")
        }
        capture(app, "diagnostics-controls-ordinary-size")
    }

    private func openLargerTextSettings() -> XCUIApplication {
        let settings = XCUIApplication(bundleIdentifier: "com.apple.Preferences")
        settings.launch()
        capture(settings, "runtime-size-settings")
        let accessibility = settings.staticTexts["Accessibility"].firstMatch
        reveal(accessibility, in: settings, fullyVisible: true)
        accessibility.tap()
        let display = settings.cells["DISPLAY_AND_TEXT"].firstMatch
        XCTAssertTrue(display.waitForExistence(timeout: 10))
        reveal(display, in: settings, fullyVisible: true)
        display.tap()
        let largerText = settings.staticTexts["Larger Text"].firstMatch
        XCTAssertTrue(largerText.waitForExistence(timeout: 10))
        reveal(largerText, in: settings, fullyVisible: true)
        largerText.tap()
        return settings
    }

    private func disconnected(language: String, dark: Bool, large: Bool) throws {
        let app = launch(language: language, dark: dark, large: large)
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        let chinese = language == "zh-Hans"
        app.tabBars.buttons[chinese ? "连接" : "Connect"].tap()
        let name = app.textFields[chinese ? "网络名称" : "Network name"]
        XCTAssertTrue(name.waitForExistence(timeout: 10))
        XCTAssertEqual(name.placeholderValue, chinese ? "网络名称" : "Network name")
        capture(app, "\(language)-connection")
        app.tabBars.buttons[chinese ? "工具" : "Tools"].tap()
        openDiagnostics(app)
        let prepare = app.buttons["diagnostics.prepare"]
        XCTAssertTrue(prepare.waitForExistence(timeout: 10))
        prepare.tap()
        XCTAssertTrue(app.staticTexts["diagnostics.report"].waitForExistence(timeout: 10))
        if !large {
            try settlePreparedDiagnostics(app, prepare: prepare)
        }
        capture(app, "\(language)-diagnostics")
        try audit(app, name: language)
        app.terminate()
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
        XCTAssertTrue(app.tabBars.firstMatch.waitForExistence(timeout: 15))
        app.tabBars.buttons["Connect"].tap()
        // Native profile creation proves the separate per-target HTTP permission UI.
        let name = app.textFields["Network name"]
        name.tap()
        name.typeText("UI isolated daemon")
        let address = app.textFields["Complete API base URL"]
        address.tap()
        address.typeText(endpoint)
        let permission = app.switches["profiles.allowHTTP"]
        reveal(permission, in: app)
        XCTAssertEqual(permission.value as? String, "0")
        // SwiftUI exposes both the labelled row and its native switch.
        // Tapping the row's centre can hit only the multiline label.
        permission.switches.firstMatch.tap()
        XCTAssertEqual(permission.value as? String, "1")
        let add = app.buttons["Add and connect"]
        reveal(add, in: app)
        add.tap()
        let payload = app.textFields["Paste QR payload"]
        reveal(payload, in: app)
        payload.tap()
        payload.typeText(components.string!)
        let preview = app.buttons["Preview target"]
        reveal(preview, in: app)
        preview.tap()
        let keyboardDismissed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"), object: app.keyboards.firstMatch)
        XCTAssertEqual(XCTWaiter.wait(for: [keyboardDismissed], timeout: 5), .completed)
        // Preview has not authenticated; redemption requires a second explicit consent.
        let pairPermission = app.switches["pairing.allowHTTP"]
        reveal(pairPermission, in: app)
        XCTAssertEqual(pairPermission.value as? String, "0")
        pairPermission.switches.firstMatch.tap()
        XCTAssertEqual(pairPermission.value as? String, "1")
        let confirm = app.buttons["Confirm target and pair"]
        reveal(confirm, in: app)
        confirm.tap()
        XCTAssertTrue(app.tabBars.buttons["Agents"].isHittable)
        app.tabBars.buttons["Agents"].tap()
        XCTAssertTrue(app.tabBars.buttons["Agents"].isSelected)
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
        app.tabBars.buttons["Work"].tap()
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let taskRow = app.buttons["work.task." + task]
        reveal(taskRow, in: app)
        taskRow.tap()
        let output = app.staticTexts["work.output"]
        XCTAssertTrue(output.waitForExistence(timeout: 30))
        XCTAssertTrue(output.label.contains(taskMarker))
        capture(app, "task-output")
        app.tabBars.buttons["Files"].tap()
        // Close the plan preview before opening the independently supplied artifact.
        if app.buttons["Close preview"].exists { app.buttons["Close preview"].tap() }
        let reference = app.textFields["files.reference"]
        XCTAssertTrue(reference.waitForExistence(timeout: 10))
        reference.tap()
        reference.typeText(fileReference)
        app.buttons["files.openReference"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", fileMarker))
            .firstMatch.waitForExistence(timeout: 30))
        capture(app, "file-preview")
        app.tabBars.buttons["Tools"].tap()
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
