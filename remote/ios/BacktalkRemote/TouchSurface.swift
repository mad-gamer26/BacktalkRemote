import SwiftUI
import UIKit

/// The phone's screen on this iPhone: touches here go to the phone as they
/// happen, and its Backtalk handles them as its own, so Backtalk's gestures
/// work here as on the phone.
///
/// VoiceOver passes touches in this area straight through (the "allows
/// direct interaction" trait), while the rest of the screen works with
/// VoiceOver as usual.
struct TouchSurface: UIViewRepresentable {
    let label: String
    let send: (TouchEvent) -> Void

    func makeUIView(context: Context) -> TouchSurfaceView {
        let view = TouchSurfaceView()
        view.send = send
        view.accessibilityLabel = label
        return view
    }

    func updateUIView(_ view: TouchSurfaceView, context: Context) {
        view.send = send
        view.accessibilityLabel = label
    }
}

final class TouchSurfaceView: UIView {
    var send: ((TouchEvent) -> Void)?
    /// The fingers down, in the order they touched, with their ids.
    private var fingers: [(touch: UITouch, id: UInt32)] = []

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .secondarySystemBackground
        isAccessibilityElement = true
        accessibilityTraits = [.allowsDirectInteraction]
        accessibilityHint = String(localized: "Touches here go straight to the phone. Use Backtalk's gestures, as on the phone.")
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private func id(of touch: UITouch) -> UInt32? {
        fingers.first { $0.touch === touch }?.id
    }

    /// Every finger down, from 0 to 1 across and down this area.
    private func pointers() -> [TouchPointer] {
        let size = bounds.size
        return fingers.map { finger in
            let p = finger.touch.location(in: self)
            return TouchPointer(
                id: finger.id,
                x: Float(min(max(p.x / max(size.width, 1), 0), 1)),
                y: Float(min(max(p.y / max(size.height, 1), 0), 1)))
        }
    }

    private func emit(_ action: TouchAction, changed: UInt32, at time: TimeInterval) {
        send?(TouchEvent(action: action, changed: changed, timeMs: UInt64(time * 1000), pointers: pointers()))
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches.sorted(by: { $0.timestamp < $1.timestamp }) {
            let used = Set(fingers.map(\.id))
            let id = (0..<UInt32(10)).first { !used.contains($0) } ?? UInt32(fingers.count)
            fingers.append((touch, id))
            emit(fingers.count == 1 ? .down : .pointerDown, changed: id, at: touch.timestamp)
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let first = fingers.first else { return }
        emit(.move, changed: first.id, at: event?.timestamp ?? first.touch.timestamp)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches.sorted(by: { $0.timestamp < $1.timestamp }) {
            guard let id = id(of: touch) else { continue }
            // As on Android, the lifting finger is still in this event.
            emit(fingers.count == 1 ? .up : .pointerUp, changed: id, at: touch.timestamp)
            fingers.removeAll { $0.touch === touch }
        }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let first = fingers.first else { return }
        emit(.cancel, changed: first.id, at: event?.timestamp ?? first.touch.timestamp)
        fingers.removeAll()
    }
}
