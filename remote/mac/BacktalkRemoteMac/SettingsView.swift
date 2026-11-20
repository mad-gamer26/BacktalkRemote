import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: RemoteModel

    var body: some View {
        Form {
            Toggle("Speak each gesture with VoiceOver as it is sent", isOn: $model.speakCommands)
            Text("For the gestures in the Phone menu. Off by default, as the phone's Backtalk says what the gesture did.")
                .font(.callout)
                .accessibilityHidden(true)
        }
        .formStyle(.grouped)
        .frame(width: 480)
    }
}
