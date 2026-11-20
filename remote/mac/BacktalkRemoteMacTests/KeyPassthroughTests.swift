import CoreGraphics
import XCTest
@testable import Backtalk_Remote

/// Keyboard passthrough, as in NVRC: every key to the phone until
/// Control-Command-Escape.
final class KeyPassthroughTests: XCTestCase {
    private var sent: [String] = []
    private var stopped = 0
    private var keys: KeyPassthrough!

    override func setUp() {
        sent = []
        stopped = 0
        keys = KeyPassthrough()
        keys.onEvent = { [unowned self] key in
            let action = key.action == .down ? "down" : "up"
            // Num Lock is always on; leave it out of what the tests compare.
            let meta = key.metaState & ~AndroidKeys.numLockOn
            self.sent.append("\(action) \(key.keyCode) meta \(String(meta, radix: 16)) repeat \(key.repeat)")
        }
        keys.onStopShortcutTriggered = { [unowned self] in self.stopped += 1 }
    }

    // Mac key codes.
    let rightArrow = 124, a = 0, g = 5, escape = 53, f19 = 0x50
    let leftCommand = 55, leftOption = 58, leftControl = 59, leftShift = 56

    @discardableResult
    private func down(_ code: Int, _ flags: CGEventFlags = [], repeat r: Bool = false) -> Bool {
        keys.handle(.keyDown, keyCode: code, flags: flags, autorepeat: r)
    }

    @discardableResult
    private func up(_ code: Int, _ flags: CGEventFlags = []) -> Bool {
        keys.handle(.keyUp, keyCode: code, flags: flags, autorepeat: false)
    }

    @discardableResult
    private func flags(_ code: Int, _ flags: CGEventFlags) -> Bool {
        keys.handle(.flagsChanged, keyCode: code, flags: flags, autorepeat: false)
    }

    func testEveryKeyGoesToThePhone() {
        XCTAssertTrue(down(rightArrow))
        XCTAssertTrue(up(rightArrow))
        XCTAssertEqual(sent, ["down 22 meta 0 repeat 0", "up 22 meta 0 repeat 0"])
    }

    func testModifiersGoAsTheyArePressedCommandAsMetaOptionAsAlt() {
        XCTAssertTrue(flags(leftCommand, .maskCommand))
        XCTAssertTrue(flags(leftOption, [.maskCommand, .maskAlternate]))
        down(rightArrow, [.maskCommand, .maskAlternate])
        up(rightArrow, [.maskCommand, .maskAlternate])
        flags(leftOption, .maskCommand)
        flags(leftCommand, [])
        XCTAssertEqual(sent, [
            "down 117 meta 30000 repeat 0",
            "down 57 meta 30012 repeat 0",
            "down 22 meta 30012 repeat 0",
            "up 22 meta 30012 repeat 0",
            "up 57 meta 30000 repeat 0",
            "up 117 meta 0 repeat 0",
        ])
    }

    func testVoiceOverKeysGoToThePhoneToo() {
        flags(leftControl, .maskControl)
        flags(leftOption, [.maskControl, .maskAlternate])
        down(rightArrow, [.maskControl, .maskAlternate])
        XCTAssertEqual(sent.first, "down 113 meta 3000 repeat 0")
        XCTAssertEqual(sent.last, "down 22 meta 3012 repeat 0")
    }

    func testControlCommandEscapeReturnsTheKeyboardAndLetsGoOfEverything() {
        flags(leftControl, .maskControl)
        flags(leftCommand, [.maskControl, .maskCommand])
        sent = []
        XCTAssertTrue(down(escape, [.maskControl, .maskCommand]))
        // The phone never sees the Escape; it sees the modifiers let go, keys first.
        XCTAssertEqual(sent, ["up 113 meta 30000 repeat 0", "up 117 meta 0 repeat 0"])
        XCTAssertTrue(keys.didTriggerStopShortcut)
        let done = expectation(description: "stop shortcut")
        DispatchQueue.main.async { done.fulfill() }
        wait(for: [done], timeout: 1)
        XCTAssertEqual(stopped, 1)
    }

    func testEscapeAloneOrWithOneOfThemIsThePhones() {
        down(escape)
        down(escape, .maskCommand)
        XCTAssertFalse(keys.didTriggerStopShortcut)
        XCTAssertEqual(sent.count, 2)
    }

    func testTheReleaseOfAKeyThePhoneNeverSawIsTheMacs() {
        // The G and Command of Command-G, which turned passthrough on.
        XCTAssertFalse(up(g, .maskCommand))
        XCTAssertFalse(flags(leftCommand, []))
        XCTAssertTrue(sent.isEmpty)
    }

    func testAHeldKeyRepeats() {
        down(a)
        down(a, repeat: true)
        down(a, repeat: true)
        up(a)
        XCTAssertEqual(sent, [
            "down 29 meta 0 repeat 0", "down 29 meta 0 repeat 1", "down 29 meta 0 repeat 2", "up 29 meta 0 repeat 0",
        ])
    }

    func testCapsLockArrivesAsF19AndBecomesAndroidsCapsLock() {
        down(f19)
        down(f19, repeat: true)
        up(f19)
        down(a)
        XCTAssertEqual(sent, [
            "down 115 meta 100000 repeat 0", "up 115 meta 100000 repeat 0", "down 29 meta 100000 repeat 0",
        ])
    }

    func testCapsLockWithoutTheRemapIsATapForEachChange() {
        flags(57, .maskAlphaShift)
        flags(57, [])
        XCTAssertEqual(sent, [
            "down 115 meta 100000 repeat 0", "up 115 meta 100000 repeat 0",
            "down 115 meta 0 repeat 0", "up 115 meta 0 repeat 0",
        ])
    }

    func testEverythingHeldIsLetGoModifiersLast() {
        flags(leftShift, .maskShift)
        down(a, .maskShift)
        sent = []
        keys.releaseAllPressedKeys()
        XCTAssertEqual(sent, ["up 29 meta 41 repeat 0", "up 59 meta 0 repeat 0"])
    }

    func testTheLettersAreInOrder() {
        let letters: [UInt16] = [0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6]
        XCTAssertEqual(letters.map { AndroidKeys.keyCode(forMac: $0) }, (29...54).map { UInt32($0) })
    }
}
