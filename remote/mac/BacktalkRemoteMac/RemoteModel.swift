import AppKit
import Foundation

/// The app's state: the settings, and the session controlling the phone.
///
/// The session (the Rust core) reports its status on its own threads; the
/// model takes it on the main actor, says what changed with VoiceOver, and
/// keeps the Mac awake while it controls the phone.
@MainActor
final class RemoteModel: ObservableObject {
    @Published var server: String {
        didSet { UserDefaults.standard.set(server, forKey: Self.serverKey) }
    }
    @Published var accessKey: String
    /// What is wrong with the settings, in words, after Connect.
    @Published private(set) var problem: String?
    @Published private(set) var status: ControllerStatus?
    @Published private(set) var session: ControllerSession?
    /// Keyboard passthrough, as in NVRC: every key, Mac shortcuts and the
    /// Mac VoiceOver's keys included, goes to the phone until
    /// Control-Command-Escape gives the keyboard back. On by itself once
    /// connected.
    @Published var captureKeyboard = false {
        didSet { updateKeyboardCapture() }
    }
    /// Asks the text field to take keyboard focus.
    @Published var focusTypingRequest = 0

    /// Whether to say each command's name with VoiceOver as it is sent.
    @Published var speakCommands: Bool {
        didSet { UserDefaults.standard.set(speakCommands, forKey: Self.speakCommandsKey) }
    }

    static let defaultServer = "mad-gamer.com"
    private static let serverKey = "relayServer"
    private static let speakCommandsKey = "speakCommands"
    private let keychain = Keychain(account: "accessKey")
    private var awake: NSObjectProtocol?
    private let keyboardCaptureController = KeyboardCaptureController()

    init() {
        if ProcessInfo.processInfo.arguments.contains("-resetSettings") {
            UserDefaults.standard.removeObject(forKey: Self.serverKey)
            Keychain(account: "accessKey").delete()
        }
        server = UserDefaults.standard.string(forKey: Self.serverKey) ?? Self.defaultServer
        // Unit tests run inside the app, signed differently: reading the saved
        // key would stop them at a keychain prompt.
        let testing = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        accessKey = testing ? "" : Keychain(account: "accessKey").read() ?? ""
        speakCommands = UserDefaults.standard.bool(forKey: Self.speakCommandsKey)
    }

    var isActive: Bool { session != nil }
    var isConnected: Bool { status?.state == .connected }

    func connect() {
        let key = accessKey.trimmingCharacters(in: .whitespaces)
        let relay = server.trimmingCharacters(in: .whitespaces)
        if let p = accessKeyProblem(key: key) ?? relayServerProblem(server: relay) {
            problem = p
            announce(p)
            return
        }
        problem = nil
        keychain.write(key)
        status = nil
        session = ControllerSession.start(
            server: relay, accessKey: key, name: Host.current().localizedName ?? "Mac",
            listener: StatusListener(model: self))
        // Keep the Mac from sleeping while it controls the phone; the
        // display may still sleep.
        awake = ProcessInfo.processInfo.beginActivity(
            options: [.idleSystemSleepDisabled, .userInitiated],
            reason: "Controlling a phone")
    }

    func disconnect() {
        guard session != nil else { return }
        captureKeyboard = false
        session?.stop()
        session = nil
        status = nil
        endAwake()
        announce("Disconnected")
    }

    // MARK: Keyboard passthrough, as in NVRC

    /// Connection ▸ Pass Keyboard to Phone (Command-G): every key, Mac
    /// shortcuts and the Mac VoiceOver's keys included, goes to the phone
    /// until Control-Command-Escape gives the keyboard back.
    func toggleKeyboardPassthrough() {
        guard isConnected else { return }
        if captureKeyboard {
            stopRemoteKeyboardControlUsingShortcut()
            return
        }
        startKeyboardPassthrough()
    }

    /// Returns false, and asks for the permission, without Accessibility
    /// permission, which the keyboard tap needs.
    @discardableResult
    private func startKeyboardPassthrough() -> Bool {
        guard AXIsProcessTrusted() else {
            // kAXTrustedCheckOptionPrompt's value: the global itself isn't concurrency-safe to read.
            _ = AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": true] as CFDictionary)
            Announcer.announce("Keyboard passthrough needs Backtalk Remote turned on in System Settings, Privacy and Security, Accessibility. Then press Command-G.")
            return false
        }
        guard phoneTakesKeys else {
            Announcer.announce("This phone's Backtalk Remote is too old to take keys. Update Backtalk Remote on the phone.")
            return false
        }
        setRemoteKeyboardControlActive(true)
        return true
    }

    func setRemoteKeyboardControlActive(_ active: Bool) {
        guard isConnected else {
            captureKeyboard = false
            return
        }
        if captureKeyboard != active {
            captureKeyboard = active
        }
    }

    func stopRemoteKeyboardControlUsingShortcut() {
        guard isConnected, captureKeyboard else { return }
        captureKeyboard = false
        Announcer.announce("Keyboard returned to the Mac.")
    }

    private func updateKeyboardCapture() {
        guard isConnected else {
            keyboardCaptureController.stop(notifyStop: false)
            return
        }
        if captureKeyboard {
            keyboardCaptureController.start(
                onStop: { [weak self] in
                    self?.captureKeyboard = false
                },
                onStopShortcutTriggered: { [weak self] in
                    self?.stopRemoteKeyboardControlUsingShortcut()
                },
                onEvent: { [weak self] key in
                    self?.session?.sendKey(key: key)
                }
            )
        } else {
            keyboardCaptureController.stop(notifyStop: false)
        }
    }

    private func endAwake() {
        if let awake { ProcessInfo.processInfo.endActivity(awake) }
        awake = nil
    }

    func perform(_ command: PhoneCommand) {
        guard let session, isConnected else {
            NSSound.beep()
            return
        }
        switch command.action {
        case .gesture(let gesture):
            if !session.sendGesture(gesture: gesture) {
                // Too many gestures waiting: the key was pressed faster
                // than the phone can be swiped.
                NSSound.beep()
                return
            }
        case .global(let action):
            session.sendGlobalAction(action: action)
        }
        if speakCommands { announce(command.title, interrupt: true) }
    }

    func send(_ event: TouchEvent) { session?.sendTouch(event: event) }

    func send(_ key: KeyInput) { session?.sendKey(key: key) }

    /// Whether the phone takes keys from this Mac's keyboard.
    var phoneTakesKeys: Bool { status?.keyboard ?? false }

    func type(_ text: String) {
        guard !text.isEmpty else { return }
        session?.sendText(text: text)
    }

    func audioDetails() -> String {
        session?.audioDetails() ?? "No audio yet."
    }

    /// The status in words, as the window and VoiceOver say it.
    var statusText: String {
        guard let status else { return "Connecting to the relay server" }
        return Self.describe(status)
    }

    static func describe(_ s: ControllerStatus) -> String {
        var text: String
        switch s.state {
        case .connecting: text = "Connecting to the relay server"
        case .waitingForPhone:
            text = "Waiting for the phone. No phone is sharing with this access key right now."
        case .connected:
            text = "Controlling \(s.phone ?? "the phone")."
            switch s.audio {
            case .playing: text += " Audio playing."
            case .waiting, .off: text += " Waiting for audio."
            case .failed: break
            }
        case .reconnecting: text = "Connection lost. Connecting again."
        case .stopped: text = "Off"
        case .failed: return "Stopped: \(s.detail ?? "")"
        }
        if let detail = s.detail, !detail.isEmpty { text += " " + detail }
        return text
    }

    fileprivate func update(_ new: ControllerStatus) {
        guard session != nil else { return }
        let old = status
        status = new
        if new.state != old?.state {
            switch new.state {
            case .connected:
                // Passthrough starts by itself on connecting.
                if new.keyboard && AXIsProcessTrusted() {
                    Announcer.announce("Controlling \(new.phone ?? "the phone"). Control-Command-Escape returns the keyboard to the Mac.")
                    setRemoteKeyboardControlActive(true)
                } else if new.keyboard {
                    Announcer.announce("Controlling \(new.phone ?? "the phone").")
                    startKeyboardPassthrough()
                } else {
                    Announcer.announce("Controlling \(new.phone ?? "the phone"), but its Backtalk Remote is too old to take keys from this keyboard. Update Backtalk Remote on the phone. Until then, the Phone menu has its gestures.")
                }
            case .waitingForPhone, .reconnecting, .failed:
                // Without the phone, the keyboard is the Mac's.
                captureKeyboard = false
                announce(Self.describe(new))
            default: break
            }
        } else if new.audio != old?.audio, new.audio == .failed {
            announce(Self.describe(new))
        }
        if new.state == .failed {
            captureKeyboard = false
            session = nil
            endAwake()
            problem = Self.describe(new)
        }
    }

    func announce(_ text: String, interrupt: Bool = false) {
        let priority: NSAccessibilityPriorityLevel = interrupt ? .high : .medium
        let element: Any = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp as Any
        NSAccessibility.post(
            element: element, notification: .announcementRequested,
            userInfo: [.announcement: text, .priority: priority.rawValue])
    }
}

/// Takes the session's status from its threads to the model.
private final class StatusListener: ControllerListener, @unchecked Sendable {
    private weak var model: RemoteModel?

    init(model: RemoteModel) { self.model = model }

    func onStatus(status: ControllerStatus) {
        Task { @MainActor [weak model] in model?.update(status) }
    }
}

/// The access key, kept in the keychain.
struct Keychain {
    let account: String
    private let service = "com.madgamer.BacktalkRemoteMac"

    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    func read() -> String? {
        var q = query
        q[kSecReturnData as String] = true
        var item: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    func delete() {
        SecItemDelete(query as CFDictionary)
    }

    func write(_ value: String) {
        if read() == value { return }
        SecItemDelete(query as CFDictionary)
        var q = query
        q[kSecValueData as String] = Data(value.utf8)
        SecItemAdd(q as CFDictionary, nil)
    }
}
