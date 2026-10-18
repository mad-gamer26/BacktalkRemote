import SwiftUI

@main
struct BacktalkRemoteApp: App {
    @StateObject private var model = RemoteModel()

    var body: some Scene {
        WindowGroup {
            Group {
                if model.isActive {
                    ControlView()
                } else {
                    ConnectView()
                }
            }
            .environmentObject(model)
        }
    }
}

/// Where to connect: the phone's access key and the relay server.
struct ConnectView: View {
    @EnvironmentObject private var model: RemoteModel

    var body: some View {
        NavigationStack {
            // Labels and explanations are ordinary text in each row, not
            // placeholders or section headers and footers: those are a light
            // gray that misses full contrast, whatever color they are given.
            Form {
                Section {
                    LabeledField(
                        label: "Access key",
                        explanation: "The access key shown on the phone, in Backtalk settings, Remote access. Capital letters count.",
                        text: $model.accessKey, identifier: "accessKey"
                    ) {
                        $0.submitLabel(.go).onSubmit { model.connect() }
                    }
                }
                Section {
                    LabeledField(
                        label: "Relay server",
                        explanation: "The same relay server the phone uses. \(RemoteModel.defaultServer) unless you run your own.",
                        text: $model.server, identifier: "relayServer"
                    ) {
                        $0.keyboardType(.URL)
                    }
                }
                Section {
                    Button("Connect") { model.connect() }
                        .bold()
                        // The system blue on white is just short of full contrast.
                        .tint(.primary)
                }
                if let problem = model.problem {
                    Section {
                        Text(problem)
                    }
                }
            }
            .navigationTitle("Backtalk Remote")
        }
    }
}

/// A text field with its name above it and what it is for below it, all in
/// full-contrast text. The field grows rather than cut text off at large
/// text sizes.
struct LabeledField<Field: View>: View {
    let label: LocalizedStringKey
    let explanation: LocalizedStringKey
    @Binding var text: String
    let identifier: String
    let configure: (AnyView) -> Field

    init(
        label: LocalizedStringKey, explanation: LocalizedStringKey, text: Binding<String>,
        identifier: String, configure: @escaping (AnyView) -> Field
    ) {
        self.label = label
        self.explanation = explanation
        self._text = text
        self.identifier = identifier
        self.configure = configure
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label)
                .font(.headline)
                // VoiceOver reads the field by this name already.
                .accessibilityHidden(true)
            configure(AnyView(
                TextField("", text: $text, axis: .vertical)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityLabel(Text(label))
                    .accessibilityHint(Text(explanation))
                    .accessibilityIdentifier(identifier)
                    .padding(8)
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(.primary.opacity(0.6)))))
            Text(explanation)
                .font(.footnote)
                // Said as the field's hint.
                .accessibilityHidden(true)
        }
    }
}

/// Controlling the phone: what is happening, actions, and the phone's
/// screen.
struct ControlView: View {
    @EnvironmentObject private var model: RemoteModel
    @State private var typing = false
    @State private var showingDetails = false

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                Text(model.statusText)
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                    .frame(maxWidth: .infinity, alignment: .leading)
                ViewThatFits(in: .horizontal) {
                    HStack { controls }
                    VStack(alignment: .leading) { controls }
                }
                if model.isConnected {
                    ViewThatFits(in: .horizontal) {
                        HStack { actions }
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 120))], alignment: .leading) { actions }
                    }
                }
            }
            .buttonStyle(.bordered)
            // Primary-colored text on the button's own background: full contrast in light and
            // dark mode, as the accent color is not on that background.
            .tint(.primary)
            .padding()
            if model.isConnected {
                TouchSurface(
                    label: String(localized: "\(model.status?.phone ?? "Phone") screen"),
                    send: model.send)
                    .ignoresSafeArea(edges: .bottom)
            } else {
                Spacer()
            }
        }
        // VoiceOver's escape gesture (a two-finger scrub) disconnects.
        .accessibilityAction(.escape) { model.disconnect() }
        .sheet(isPresented: $typing) { TypeTextView() }
        .sheet(isPresented: $showingDetails) { AudioDetailsView() }
    }

    @ViewBuilder private var controls: some View {
        Button("Disconnect") { model.disconnect() }
        if model.isConnected {
            Button("Type text") { typing = true }
            Button("Audio details") { showingDetails = true }
        }
    }

    @ViewBuilder private var actions: some View {
        Button("Back") { model.send(.back) }
        Button("Home") { model.send(.home) }
        Button("Recent apps") { model.send(.recents) }
        Button("Notifications") { model.send(.notifications) }
        Button("Quick settings") { model.send(.quickSettings) }
    }
}

/// Text typed here goes into the phone's focused text field.
struct TypeTextView: View {
    @EnvironmentObject private var model: RemoteModel
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Text", text: $text, axis: .vertical)
                        .focused($focused)
                } footer: {
                    Text("Goes into the text field that has focus on the phone, at its cursor.")
                        .foregroundStyle(.primary)
                }
            }
            .navigationTitle("Type text")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") {
                        model.type(text)
                        dismiss()
                    }
                    .disabled(text.isEmpty)
                }
            }
            .onAppear { focused = true }
        }
    }
}

/// The audio stream's state in words, line by line.
struct AudioDetailsView: View {
    @EnvironmentObject private var model: RemoteModel
    @Environment(\.dismiss) private var dismiss
    @State private var details = ""

    var body: some View {
        NavigationStack {
            List {
                ForEach(details.split(separator: "\n").map(String.init), id: \.self) { line in
                    Text(line)
                }
            }
            .navigationTitle("Audio details")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button("Refresh") { details = model.audioDetails() }
                }
            }
            .onAppear { details = model.audioDetails() }
        }
    }
}
