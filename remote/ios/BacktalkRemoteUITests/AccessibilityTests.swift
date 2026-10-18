import XCTest

/// Xcode's accessibility audit of each screen (labels, contrast, Dynamic
/// Type, hit areas), and checks that what VoiceOver needs is there. Not a
/// substitute for trying the app with VoiceOver.
final class AccessibilityTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    /// A text field by its label: a field that grows to more lines is a text view.
    private func field(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label == %@ AND (elementType == %d OR elementType == %d)", label, XCUIElement.ElementType.textField.rawValue, XCUIElement.ElementType.textView.rawValue)).firstMatch
    }

    /// The audit, saying which element each issue is on.
    private func audit(_ app: XCUIApplication) throws {
        try app.performAccessibilityAudit { issue in
            let element = issue.element.map { "\($0.elementType.rawValue) '\($0.label)'" } ?? "none"
            print("Audit issue: \(issue.compactDescription); element: \(element)")
            return false
        }
    }

    func testConnectScreen() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-resetSettings"]
        app.launch()
        XCTAssertTrue(field(app, "Access key").waitForExistence(timeout: 5))
        XCTAssertTrue(field(app, "Relay server").exists)
        XCTAssertTrue(app.buttons["Connect"].exists)
        try audit(app)
    }

    func testShortKeyIsExplained() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-resetSettings"]
        app.launch()
        let key = field(app, "Access key")
        XCTAssertTrue(key.waitForExistence(timeout: 5))
        key.tap()
        key.typeText("short")
        app.buttons["Connect"].tap()
        XCTAssertTrue(app.staticTexts["The access key needs at least 8 characters."].waitForExistence(timeout: 5))
    }

    /// With a key no phone shares, the app waits for the phone: the control
    /// screen, with its status and a way out.
    func testWaitingScreen() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-resetSettings"]
        app.launch()
        let key = field(app, "Access key")
        XCTAssertTrue(key.waitForExistence(timeout: 5))
        key.tap()
        key.typeText("no phone shares this \(UUID().uuidString)")
        app.buttons["Connect"].tap()
        XCTAssertTrue(app.buttons["Disconnect"].waitForExistence(timeout: 10))
        let waiting = NSPredicate(format: "label BEGINSWITH 'Waiting for the phone'")
        XCTAssertTrue(app.staticTexts.matching(waiting).firstMatch.waitForExistence(timeout: 20))
        try audit(app)
        app.buttons["Disconnect"].tap()
        XCTAssertTrue(app.buttons["Connect"].waitForExistence(timeout: 5))
    }
}
