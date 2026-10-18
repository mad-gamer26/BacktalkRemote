import AVFoundation
import Foundation
import UIKit

/// The app's state: the settings, and the session controlling the phone.
///
/// The session (the Rust core) reports its status on its own threads; the
/// model takes it on the main actor, says what changed with VoiceOver, and
/// keeps the iPhone awake while it controls the phone.
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
    /// Between Connect and the session starting.
    @Published private(set) var connecting = false

    static let defaultServer = "mad-gamer.com"
    private static let serverKey = "relayServer"
    private let keychain = Keychain(account: "accessKey")

    init() {
        if ProcessInfo.processInfo.arguments.contains("-resetSettings") {
            // UI tests start from a new install's settings.
            UserDefaults.standard.removeObject(forKey: Self.serverKey)
            Keychain(account: "accessKey").delete()
        }
        server = UserDefaults.standard.string(forKey: Self.serverKey) ?? Self.defaultServer
        accessKey = Keychain(account: "accessKey").read() ?? ""
    }

    var isActive: Bool { session != nil || connecting }
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
        connecting = true
        // The audio session first: the core opens the audio output once the
        // phone answers.
        RemoteAudioSession.start { [weak self] in
            guard let self, self.connecting else { return }
            self.connecting = false
            self.session = ControllerSession.start(
                server: relay, accessKey: key, name: UIDevice.current.name,
                listener: StatusListener(model: self))
            UIApplication.shared.isIdleTimerDisabled = true
        }
    }

    func disconnect() {
        connecting = false
        session?.stop()
        session = nil
        status = nil
        RemoteAudioSession.stop()
        UIApplication.shared.isIdleTimerDisabled = false
        announce(String(localized: "Disconnected"))
    }

    func send(_ event: TouchEvent) { session?.sendTouch(event: event) }

    func send(_ action: GlobalAction) { session?.sendGlobalAction(action: action) }

    func type(_ text: String) {
        guard !text.isEmpty else { return }
        session?.sendText(text: text)
    }

    func audioDetails() -> String {
        session?.audioDetails() ?? String(localized: "No audio yet.")
    }

    /// The status in words, as the screen and VoiceOver say it.
    var statusText: String {
        guard let status else { return String(localized: "Connecting to the relay server") }
        return Self.describe(status)
    }

    static func describe(_ s: ControllerStatus) -> String {
        var text: String
        switch s.state {
        case .connecting: text = String(localized: "Connecting to the relay server")
        case .waitingForPhone:
            text = String(localized: "Waiting for the phone. No phone is sharing with this access key right now.")
        case .connected:
            text = String(localized: "Controlling \(s.phone ?? "the phone").")
            switch s.audio {
            case .playing: text += " " + String(localized: "Audio playing.")
            case .waiting, .off: text += " " + String(localized: "Waiting for audio.")
            case .failed: break
            }
        case .reconnecting: text = String(localized: "Connection lost. Connecting again.")
        case .stopped: text = String(localized: "Off")
        case .failed: return String(localized: "Stopped: \(s.detail ?? "")")
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
                announce(String(localized: "Controlling \(new.phone ?? "the phone"). The lower part of the screen is now the phone's screen."))
            case .waitingForPhone, .reconnecting, .failed:
                announce(Self.describe(new))
            default: break
            }
        } else if new.audio != old?.audio, new.audio == .failed {
            announce(Self.describe(new))
        }
        if new.state == .failed {
            session = nil
            RemoteAudioSession.stop()
            UIApplication.shared.isIdleTimerDisabled = false
            problem = Self.describe(new)
        }
    }

    private func announce(_ text: String) {
        UIAccessibility.post(notification: .announcement, argument: text)
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

/// The audio session: playback alongside other audio, active only while
/// controlling, so the iPhone's own audio is untouched otherwise.
/// Activating can take a while, so never on the main thread.
enum RemoteAudioSession {
    private static let queue = DispatchQueue(label: "RemoteAudioSession", qos: .userInitiated)

    /// Activates the session, then runs `then` on the main actor.
    static func start(then: @escaping @MainActor @Sendable () -> Void) {
        queue.async {
            let session = AVAudioSession.sharedInstance()
            try? session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try? session.setActive(true)
            Task { @MainActor in then() }
        }
    }

    static func stop() {
        queue.async {
            try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        }
    }
}

/// The access key, kept in the keychain, on this device only.
struct Keychain {
    let account: String
    private let service = "com.madgamer.BacktalkRemote"

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
        SecItemDelete(query as CFDictionary)
        var q = query
        q[kSecValueData as String] = Data(value.utf8)
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(q as CFDictionary, nil)
    }
}
