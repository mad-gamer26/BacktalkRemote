import AppKit
import SwiftUI

/// The phone's screen on the Mac, for the mouse: a click is a touch, and a
/// drag moves a finger. Keys go to the phone by keyboard passthrough
/// (KeyboardPassthrough.swift), not through this view.
struct TouchSurface: NSViewRepresentable {
    let label: String
    let touch: (TouchEvent) -> Void

    func makeNSView(context: Context) -> TouchSurfaceView {
        let view = TouchSurfaceView()
        update(view)
        return view
    }

    func updateNSView(_ view: TouchSurfaceView, context: Context) {
        update(view)
    }

    private func update(_ view: TouchSurfaceView) {
        view.setAccessibilityLabel(label)
        view.onTouch = touch
    }
}

final class TouchSurfaceView: NSView {
    var onTouch: ((TouchEvent) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityHelp("With the mouse, a click here touches the phone's screen, and a drag moves a finger.")
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.controlBackgroundColor.setFill()
        bounds.fill()
        NSColor.separatorColor.setStroke()
        NSBezierPath(rect: bounds.insetBy(dx: 0.5, dy: 0.5)).stroke()
    }

    private func pointer(_ event: NSEvent) -> TouchPointer {
        let p = convert(event.locationInWindow, from: nil)
        let w = max(bounds.width, 1), h = max(bounds.height, 1)
        // AppKit's y goes up; the phone's goes down.
        return TouchPointer(
            id: 0,
            x: Float(min(max(p.x / w, 0), 1)),
            y: Float(min(max(1 - p.y / h, 0), 1)))
    }

    private func emit(_ action: TouchAction, _ event: NSEvent) {
        onTouch?(TouchEvent(
            action: action, changed: 0, timeMs: UInt64(event.timestamp * 1000),
            pointers: [pointer(event)]))
    }

    override func mouseDown(with event: NSEvent) { emit(.down, event) }
    override func mouseDragged(with event: NSEvent) { emit(.move, event) }
    override func mouseUp(with event: NSEvent) { emit(.up, event) }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}
