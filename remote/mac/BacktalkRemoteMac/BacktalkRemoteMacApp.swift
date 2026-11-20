import AppKit
import SwiftUI

@main
struct BacktalkRemoteMacApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = RemoteModel()

    var body: some Scene {
        // A window group, not a single window, so that a window always opens
        // at launch, even if the last one was closed.
        WindowGroup("Backtalk Remote", id: "main") {
            Group {
                if model.isActive {
                    ControlView()
                } else {
                    ConnectView()
                }
            }
            .frame(minWidth: 520, minHeight: 520)
            .environmentObject(model)
            .onAppear { delegate.model = model }
        }
        .commands {
            // One window: it controls the one phone.
            CommandGroup(replacing: .newItem) {}
            PhoneMenus(model: model)
        }

        Settings {
            SettingsView()
                .environmentObject(model)
        }

        Window("Audio Details", id: "audioDetails") {
            AudioDetailsView()
                .environmentObject(model)
        }
        .defaultSize(width: 420, height: 360)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var model: RemoteModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Caps Lock's remap, if a crash or force-quit left it on.
        CapsLockRemapper.shared.recoverAtLaunch()
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { model?.captureKeyboard = false }
        CapsLockRemapper.shared.restore()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

/// The Connection and Phone menus.
struct PhoneMenus: Commands {
    @ObservedObject var model: RemoteModel
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandMenu("Connection") {
            if model.isActive {
                Button("Disconnect") { model.disconnect() }
                    .keyboardShortcut("d", modifiers: [.command, .shift])
            } else {
                Button("Connect") { model.connect() }
                    .keyboardShortcut(.return, modifiers: .command)
            }
            Divider()
            Button("Pass Keyboard to Phone") { model.toggleKeyboardPassthrough() }
                .keyboardShortcut("g", modifiers: .command)
                .disabled(!model.isConnected)
            Button("Type Text") { model.focusTypingRequest += 1 }
                .keyboardShortcut("t")
                .disabled(!model.isConnected)
            Button("Audio Details") { openWindow(id: "audioDetails") }
                .keyboardShortcut("i")
                .disabled(!model.isActive)
            Divider()
            Toggle("Speak Gestures", isOn: $model.speakCommands)
        }
        CommandMenu("Phone") {
            ForEach(PhoneCommand.Group.allCases) { group in
                Menu(group.rawValue) {
                    ForEach(group.commands) { command in
                        Button(command.title) { model.perform(command) }
                    }
                }
            }
            .disabled(!model.isConnected)
        }
    }
}

/// Where to connect: the phone's access key and the relay server.
struct ConnectView: View {
    @EnvironmentObject private var model: RemoteModel

    var body: some View {
        Form {
            Section {
                TextField("Access key", text: $model.accessKey)
                    .onSubmit { model.connect() }
                Text("The access key shown on the phone, in Backtalk settings, Remote access. Capital letters count.")
                    .font(.callout)
                    .accessibilityHidden(true)
                TextField("Relay server", text: $model.server)
                    .onSubmit { model.connect() }
                Text("The same relay server the phone uses. \(RemoteModel.defaultServer) unless you run your own.")
                    .font(.callout)
                    .accessibilityHidden(true)
            }
            Section {
                Button("Connect") { model.connect() }
                    .keyboardShortcut(.defaultAction)
                if let problem = model.problem {
                    Text(problem)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Backtalk Remote")
    }
}

/// Controlling the phone. While the keyboard passes to the phone, as in
/// NVRC, the window says so and nothing else; otherwise it has the status,
/// typing, Android's buttons and the phone's screen for the mouse.
struct ControlView: View {
    @EnvironmentObject private var model: RemoteModel
    @Environment(\.openWindow) private var openWindow
    @State private var text = ""
    @FocusState private var typing: Bool
    @State private var accessibilityPermissionGranted = AXIsProcessTrusted()

    var body: some View {
        if model.isConnected && model.captureKeyboard {
            controllingScreen
        } else {
            controls
        }
    }

    private var controllingScreen: some View {
        VStack(spacing: 18) {
            Spacer()
            Text(model.status?.phone ?? "Phone")
                .foregroundStyle(.secondary)
            Text("Controlling phone")
                .font(.largeTitle.weight(.semibold))
            Text("Control-Command-Escape returns the keyboard to the Mac.")
                .foregroundStyle(.secondary)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(32)
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(model.statusText)
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
                .frame(maxWidth: .infinity, alignment: .leading)
            HStack {
                Button("Disconnect") { model.disconnect() }
                Button("Audio details") { openWindow(id: "audioDetails") }
            }
            if model.isConnected {
                if !model.phoneTakesKeys {
                    Text("This phone's Backtalk Remote is too old to take keys. Update Backtalk Remote on the phone. Until then, the Phone menu has its gestures.")
                } else if accessibilityPermissionGranted {
                    Text("Press Command-G to pass the keyboard to the phone. Control-Command-Escape returns it to the Mac.")
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Backtalk Remote needs Accessibility permission to pass your Mac keyboard to the phone. Press Command-G, or choose Continue, and turn Backtalk Remote on in System Settings, Privacy and Security, Accessibility.")
                        Button("Continue") { model.toggleKeyboardPassthrough() }
                            .buttonStyle(.borderedProminent)
                    }
                }
                HStack {
                    TextField("Type on the phone", text: $text)
                        .focused($typing)
                        .onSubmit(sendText)
                        .accessibilityHint("Goes into the text field that has focus on the phone, at its cursor, when you press Return.")
                    Button("Send", action: sendText)
                        .disabled(text.isEmpty)
                }
                HStack {
                    ForEach(PhoneCommand.Group.buttons.commands) { command in
                        Button(command.title) { model.perform(command) }
                    }
                }
                TouchSurface(
                    label: "\(model.status?.phone ?? "Phone") screen",
                    touch: model.send)
                    .aspectRatio(phoneAspect, contentMode: .fit)
                    .frame(maxWidth: .infinity, minHeight: 200)
            } else {
                Spacer()
            }
        }
        .padding()
        .onChange(of: model.focusTypingRequest) { typing = true }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            accessibilityPermissionGranted = AXIsProcessTrusted()
        }
    }

    /// The phone's screen shape, so the mouse touches where it looks.
    private var phoneAspect: CGFloat {
        guard let s = model.status?.screen, s.width > 0, s.height > 0 else { return 9.0 / 20.0 }
        return CGFloat(s.width) / CGFloat(s.height)
    }

    private func sendText() {
        guard !text.isEmpty else { return }
        model.type(text)
        model.announce("Sent", interrupt: true)
        text = ""
    }
}

/// The audio stream's state, line by line.
struct AudioDetailsView: View {
    @EnvironmentObject private var model: RemoteModel
    @State private var details = ""

    var body: some View {
        VStack(alignment: .leading) {
            List(details.split(separator: "\n").map(String.init), id: \.self) { line in
                Text(line)
            }
            Button("Refresh") { details = model.audioDetails() }
                .keyboardShortcut("r")
                .padding([.horizontal, .bottom])
        }
        .onAppear { details = model.audioDetails() }
    }
}
