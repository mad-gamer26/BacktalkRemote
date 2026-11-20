// Keyboard passthrough, as in NVRC (and Light Touch+): while it is on, every key, Mac shortcuts and the Mac
// VoiceOver's keys included, goes to the phone as a key of a keyboard plugged into it, until Control-Command-Escape
// gives the keyboard back to the Mac.

import AppKit
import AVFoundation
import CoreGraphics
import Foundation

/// The keys passing through, as Android key events: each key down (again for each repeat of a held key) and
/// up, with the modifiers held. Kept apart from the event tap so that it can be tested. Used only on the main
/// thread, where the event tap's run loop source is.
final class KeyPassthrough: @unchecked Sendable {
    typealias Modifier = AndroidKeys.Modifier

    enum Kind {
        case keyDown, keyUp, flagsChanged
    }

    var onEvent: ((KeyInput) -> Void)?
    var onStopShortcutTriggered: (() -> Void)?

    private var activeModifierStates: Set<Modifier> = []
    /// Android key codes down on the phone, with how many times each has repeated.
    private var pressedRemoteKeys: [UInt32: UInt32] = [:]
    private(set) var didTriggerStopShortcut = false
    private var capsLockEngaged = false
    /// Caps Lock's lock, as the phone has it, for the meta state.
    private var capsLockOn = false

    /// Android's key code for Caps Lock; the Mac's Caps Lock arrives as F19 (see CapsLockRemapper).
    static let androidCapsLock: UInt32 = 115
    static let capsLockCourier = 0x50
    static let macCapsLock = 57

    func reset() {
        activeModifierStates.removeAll()
        pressedRemoteKeys.removeAll()
        capsLockEngaged = false
        didTriggerStopShortcut = false
    }

    var metaState: UInt32 {
        activeModifierStates.reduce(AndroidKeys.numLockOn | (capsLockOn ? 0x100000 : 0)) { $0 | $1.metaBits }
    }

    /// Handles one key event; returns true if the phone took it, false to leave it to the Mac.
    func handle(_ kind: Kind, keyCode: Int, flags: CGEventFlags, autorepeat: Bool) -> Bool {
        switch kind {
        case .keyDown, .keyUp:
            if kind == .keyDown, keyCode == 53, flags.contains(.maskControl), flags.contains(.maskCommand) {
                triggerStopShortcut()
                return true
            }
            // Caps Lock is remapped to F19 at the HID layer. F19 is only an internal courier and must become
            // Android's Caps Lock; its repeats are dropped, as a held Caps Lock does not repeat.
            if kind == .keyDown, keyCode == Self.capsLockCourier, autorepeat {
                return true
            }
            let code: UInt32? = keyCode == Self.capsLockCourier
                ? Self.androidCapsLock
                : AndroidKeys.keyCode(forMac: UInt16(keyCode))
            guard let code else {
                // A key Android has no code for: still the phone's, so the Mac does nothing with it either.
                return true
            }
            if kind == .keyDown {
                var repeats: UInt32 = 0
                if let held = pressedRemoteKeys[code] { repeats = held + 1 }
                pressedRemoteKeys[code] = repeats
                if code == Self.androidCapsLock && repeats == 0 { capsLockOn.toggle() }
                onEvent?(KeyInput(action: .down, keyCode: code, metaState: metaState, repeat: repeats))
            } else {
                // A release for a key the phone never saw down (the G of Command-G) is the Mac's.
                guard pressedRemoteKeys.removeValue(forKey: code) != nil else { return false }
                onEvent?(KeyInput(action: .up, keyCode: code, metaState: metaState, repeat: 0))
            }
            return true
        case .flagsChanged:
            return handleFlagsChanged(keyCode: keyCode, flags: flags)
        }
    }

    /// True if the phone took the event; false leaves it to the Mac (a modifier let go that the phone never saw
    /// pressed, such as the Command of Command-G).
    private func handleFlagsChanged(keyCode: Int, flags: CGEventFlags) -> Bool {
        // Fallback only: without the HID remap, macOS reports Caps Lock as a state change rather than a held key
        // event, so each change of state is a tap of Caps Lock.
        if keyCode == Self.macCapsLock {
            let shouldBeDown = flags.contains(.maskAlphaShift)
            guard shouldBeDown != capsLockEngaged else { return true }
            capsLockEngaged = shouldBeDown
            capsLockOn.toggle()
            onEvent?(KeyInput(action: .down, keyCode: Self.androidCapsLock, metaState: metaState, repeat: 0))
            onEvent?(KeyInput(action: .up, keyCode: Self.androidCapsLock, metaState: metaState, repeat: 0))
            return true
        }
        guard let modifier = Modifier(macCode: UInt16(keyCode)) else { return true }
        let shouldBeDown = Self.isDown(modifier, after: flags)
        let wasDown = activeModifierStates.contains(modifier)
        guard shouldBeDown != wasDown else { return shouldBeDown }
        if shouldBeDown {
            activeModifierStates.insert(modifier)
            pressedRemoteKeys[modifier.androidCode] = 0
            onEvent?(KeyInput(action: .down, keyCode: modifier.androidCode, metaState: metaState, repeat: 0))
        } else {
            activeModifierStates.remove(modifier)
            pressedRemoteKeys.removeValue(forKey: modifier.androidCode)
            onEvent?(KeyInput(action: .up, keyCode: modifier.androidCode, metaState: metaState, repeat: 0))
        }
        return true
    }

    private static func isDown(_ modifier: Modifier, after flags: CGEventFlags) -> Bool {
        switch modifier.flag {
        case .command: flags.contains(.maskCommand)
        case .option: flags.contains(.maskAlternate)
        case .control: flags.contains(.maskControl)
        default: flags.contains(.maskShift)
        }
    }

    /// Lets go of every key still down on the phone, modifiers last.
    func releaseAllPressedKeys() {
        guard !pressedRemoteKeys.isEmpty else { return }
        let modifierCodes = Set(Modifier.allCases.map(\.androidCode))
        let keys = pressedRemoteKeys.keys.sorted {
            let (a, b) = (modifierCodes.contains($0), modifierCodes.contains($1))
            return a == b ? $0 < $1 : !a
        }
        for code in keys {
            if let m = Modifier.allCases.first(where: { $0.androidCode == code }) {
                activeModifierStates.remove(m)
            }
            onEvent?(KeyInput(action: .up, keyCode: code, metaState: metaState, repeat: 0))
        }
        pressedRemoteKeys.removeAll()
    }

    private func triggerStopShortcut() {
        guard !didTriggerStopShortcut else { return }
        didTriggerStopShortcut = true
        releaseAllPressedKeys()
        activeModifierStates.removeAll()
        DispatchQueue.main.async { [weak self] in
            self?.onStopShortcutTriggered?()
        }
    }
}

/// The event tap that takes the keys while passthrough is on, as NVRC's KeyboardCaptureController.
final class KeyboardCaptureController {
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private let passthrough = KeyPassthrough()
    var onStop: (() -> Void)?

    var isRunning: Bool { eventTap != nil }

    func start(
        onStop: @escaping () -> Void,
        onStopShortcutTriggered: @escaping () -> Void,
        onEvent: @escaping (KeyInput) -> Void
    ) {
        self.onStop = onStop
        passthrough.onStopShortcutTriggered = onStopShortcutTriggered
        passthrough.onEvent = onEvent
        passthrough.reset()
        CapsLockRemapper.shared.apply()
        guard eventTap == nil else { return }
        let mask = (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.keyUp.rawValue)
            | (1 << CGEventType.flagsChanged.rawValue)
        let callback: CGEventTapCallBack = { _, type, event, refcon in
            guard let refcon else { return Unmanaged.passUnretained(event) }
            let controller = Unmanaged<KeyboardCaptureController>.fromOpaque(refcon).takeUnretainedValue()
            return controller.handleTap(type: type, event: event)
        }
        let pointer = Unmanaged.passUnretained(self).toOpaque()
        guard let tap = CGEvent.tapCreate(
            // HID level, at the head: each key arrives here before macOS's own shortcuts (Command-Tab,
            // Command-Space) and the Mac's VoiceOver (Control-Option keys) see it, as in NVRC.
            tap: .cghidEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: CGEventMask(mask),
            callback: callback,
            userInfo: pointer
        ) else {
            CapsLockRemapper.shared.restore()
            self.onStop?()
            self.onStop = nil
            passthrough.onEvent = nil
            return
        }
        eventTap = tap
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        runLoopSource = source
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
    }

    func stop(notifyStop: Bool = true) {
        passthrough.releaseAllPressedKeys()
        CapsLockRemapper.shared.restore()
        if let runLoopSource {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), runLoopSource, .commonModes)
        }
        runLoopSource = nil
        if let eventTap {
            CFMachPortInvalidate(eventTap)
        }
        eventTap = nil
        passthrough.reset()
        passthrough.onEvent = nil
        passthrough.onStopShortcutTriggered = nil
        if notifyStop {
            self.onStop?()
        }
        onStop = nil
    }

    private func handleTap(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let eventTap { CGEvent.tapEnable(tap: eventTap, enable: true) }
            return Unmanaged.passUnretained(event)
        }
        // Keys pass through only while Backtalk Remote is the active app: a dialog or another app gets the Mac's
        // keyboard. After Control-Command-Escape, the keys still being let go are the Mac's too.
        guard NSApp.isActive, !passthrough.didTriggerStopShortcut else { return Unmanaged.passUnretained(event) }
        let kind: KeyPassthrough.Kind
        switch type {
        case .keyDown: kind = .keyDown
        case .keyUp: kind = .keyUp
        case .flagsChanged: kind = .flagsChanged
        default: return Unmanaged.passUnretained(event)
        }
        let taken = passthrough.handle(
            kind,
            keyCode: Int(event.getIntegerValueField(.keyboardEventKeycode)),
            flags: event.flags,
            autorepeat: event.getIntegerValueField(.keyboardEventAutorepeat) != 0)
        return taken ? nil : Unmanaged.passUnretained(event)
    }
}

/// Temporarily turns the physical Mac Caps Lock key into an ordinary key event, as in NVRC.
/// macOS otherwise exposes Caps Lock only as a flagsChanged lock-state toggle,
/// which cannot represent a held key.
final class CapsLockRemapper {
    nonisolated(unsafe) static let shared = CapsLockRemapper()

    private let capsUsage: UInt64 = 30_064_771_129 // 0x700000039
    private let f19Usage: UInt64 = 30_064_771_182  // 0x70000006E
    private var originalMapping: String?
    private var originalEntries: [(src: UInt64, dst: UInt64)] = []
    private var applied = false

    private static let activeKey = "BacktalkRemote.CapsLockRemapActive"
    private static let originalKey = "BacktalkRemote.CapsLockOriginalMapping"

    private init() {}

    func apply() {
        guard !applied else { return }
        guard let current = runHidutil(["property", "--get", "UserKeyMapping"]),
              let entries = Self.parseEntries(current) else {
            NSLog("Backtalk Remote: could not read hidutil mapping; Caps Lock remap not applied")
            return
        }
        originalEntries = entries
        originalMapping = Self.json(for: entries)

        // Preserve the user's other hidutil mappings. `--set` replaces the
        // complete array, so append only this app's one entry.
        let preserved = entries.filter { $0.src != capsUsage }
        let mapping = Self.json(for: preserved + [(capsUsage, f19Usage)])
        let defaults = UserDefaults.standard
        defaults.set(originalMapping, forKey: Self.originalKey)
        defaults.set(true, forKey: Self.activeKey)
        guard runHidutil(["property", "--set", mapping]) != nil else {
            originalMapping = nil
            originalEntries.removeAll()
            defaults.removeObject(forKey: Self.activeKey)
            defaults.removeObject(forKey: Self.originalKey)
            NSLog("Backtalk Remote: Caps Lock remap failed")
            return
        }
        applied = true
    }

    func restore() {
        guard applied else { return }
        let mapping = Self.json(for: originalEntries)
        guard runHidutil(["property", "--set", mapping]) != nil else {
            NSLog("Backtalk Remote: Caps Lock could not be restored; restarting macOS will clear the temporary mapping")
            return
        }
        originalMapping = nil
        originalEntries.removeAll()
        applied = false
        UserDefaults.standard.removeObject(forKey: Self.activeKey)
        UserDefaults.standard.removeObject(forKey: Self.originalKey)
    }

    /// Restore a mapping stranded by a crash or force-quit. This intentionally
    /// runs only when the app recorded an active remap in UserDefaults.
    func recoverAtLaunch() {
        let defaults = UserDefaults.standard
        guard defaults.bool(forKey: Self.activeKey) else { return }
        if let saved = defaults.string(forKey: Self.originalKey) {
            _ = runHidutil(["property", "--set", saved])
        }
        defaults.removeObject(forKey: Self.activeKey)
        defaults.removeObject(forKey: Self.originalKey)
    }

    private static func json(for entries: [(src: UInt64, dst: UInt64)]) -> String {
        let values = entries.map {
            "{\"HIDKeyboardModifierMappingSrc\":\($0.src),\"HIDKeyboardModifierMappingDst\":\($0.dst)}"
        }.joined(separator: ",")
        return "{\"UserKeyMapping\":[\(values)]}"
    }

    static func parseEntries(_ text: String) -> [(src: UInt64, dst: UInt64)]? {
        guard let regex = try? NSRegularExpression(pattern: "\\{[^{}]*\\}") else { return nil }
        let range = NSRange(text.startIndex..., in: text)
        var entries: [(src: UInt64, dst: UInt64)] = []
        for match in regex.matches(in: text, range: range) {
            guard let blockRange = Range(match.range, in: text) else { continue }
            let block = String(text[blockRange])
            guard let src = value(named: "HIDKeyboardModifierMappingSrc", in: block),
                  let dst = value(named: "HIDKeyboardModifierMappingDst", in: block) else {
                return nil
            }
            entries.append((src, dst))
        }
        return entries
    }

    private static func value(named name: String, in text: String) -> UInt64? {
        guard let regex = try? NSRegularExpression(pattern: "\(name)\\s*=\\s*(0[xX][0-9a-fA-F]+|[0-9]+)") else { return nil }
        let range = NSRange(text.startIndex..., in: text)
        guard let match = regex.firstMatch(in: text, range: range),
              let valueRange = Range(match.range(at: 1), in: text) else { return nil }
        let raw = String(text[valueRange])
        return raw.lowercased().hasPrefix("0x")
            ? UInt64(raw.dropFirst(2), radix: 16)
            : UInt64(raw)
    }

    private func runHidutil(_ arguments: [String]) -> String? {
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/hidutil")
        process.arguments = arguments
        process.standardOutput = pipe
        process.standardError = pipe
        do {
            try process.run()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else { return nil }
            return String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)
        } catch {
            NSLog("Backtalk Remote: hidutil failed: %@", error.localizedDescription)
            return nil
        }
    }
}

/// Passthrough's announcements, as in NVRC: with VoiceOver running, an announcement at medium priority (queued
/// behind what VoiceOver is saying, never interrupting it); without it, the app's own announcement voice.
@MainActor enum Announcer {
    private static let synthesizer = AVSpeechSynthesizer()

    static func announce(_ text: String) {
        if NSWorkspace.shared.isVoiceOverEnabled {
            NSAccessibility.post(
                element: NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp as Any,
                notification: .announcementRequested,
                userInfo: [.announcement: text, .priority: NSAccessibilityPriorityLevel.medium.rawValue]
            )
        } else {
            synthesizer.speak(AVSpeechUtterance(string: text))
        }
    }
}
