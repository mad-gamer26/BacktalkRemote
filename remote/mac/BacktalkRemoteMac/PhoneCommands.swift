import AppKit
import Foundation

/// Something the Mac's keyboard can do on the phone: a Backtalk gesture,
/// drawn on the phone's screen with touches, or one of Android's buttons.
enum PhoneCommand: String, CaseIterable, Codable, Identifiable, Sendable {
    // One finger.
    case swipeRight, swipeLeft, swipeUp, swipeDown
    case swipeUpThenDown, swipeDownThenUp, swipeLeftThenRight, swipeRightThenLeft
    case swipeUpThenLeft, swipeUpThenRight, swipeDownThenLeft, swipeDownThenRight
    case swipeLeftThenUp, swipeLeftThenDown, swipeRightThenUp, swipeRightThenDown
    case tap, doubleTap, tripleTap, doubleTapAndHold
    // Several fingers.
    case twoFingerSwipeRight, twoFingerSwipeLeft, twoFingerSwipeUp, twoFingerSwipeDown
    case threeFingerSwipeRight, threeFingerSwipeLeft, threeFingerSwipeUp, threeFingerSwipeDown
    case fourFingerSwipeRight, fourFingerSwipeLeft, fourFingerSwipeUp, fourFingerSwipeDown
    case twoFingerTap, twoFingerDoubleTap, twoFingerTripleTap, twoFingerDoubleTapAndHold
    case threeFingerTap, threeFingerDoubleTap, threeFingerTripleTap
    case fourFingerTap, fourFingerDoubleTap, fourFingerTripleTap
    case scrollUp, scrollDown, scrollLeft, scrollRight
    // Android's buttons.
    case back, home, recents, notifications, quickSettings

    var id: String { rawValue }

    enum Group: String, CaseIterable, Identifiable {
        case oneFinger = "One-finger gestures"
        case angles = "Angle gestures"
        case multiFinger = "Multi-finger gestures"
        case scrolling = "Scrolling"
        case buttons = "Android buttons"
        var id: String { rawValue }
        var commands: [PhoneCommand] { PhoneCommand.allCases.filter { $0.group == self } }
    }

    var group: Group {
        switch self {
        case .swipeRight, .swipeLeft, .swipeUp, .swipeDown,
             .swipeUpThenDown, .swipeDownThenUp, .swipeLeftThenRight, .swipeRightThenLeft,
             .tap, .doubleTap, .tripleTap, .doubleTapAndHold:
            return .oneFinger
        case .swipeUpThenLeft, .swipeUpThenRight, .swipeDownThenLeft, .swipeDownThenRight,
             .swipeLeftThenUp, .swipeLeftThenDown, .swipeRightThenUp, .swipeRightThenDown:
            return .angles
        case .scrollUp, .scrollDown, .scrollLeft, .scrollRight:
            return .scrolling
        case .back, .home, .recents, .notifications, .quickSettings:
            return .buttons
        default:
            return .multiFinger
        }
    }

    var title: String {
        switch self {
        case .swipeRight: "Swipe right"
        case .swipeLeft: "Swipe left"
        case .swipeUp: "Swipe up"
        case .swipeDown: "Swipe down"
        case .swipeUpThenDown: "Swipe up then down"
        case .swipeDownThenUp: "Swipe down then up"
        case .swipeLeftThenRight: "Swipe left then right"
        case .swipeRightThenLeft: "Swipe right then left"
        case .swipeUpThenLeft: "Swipe up then left"
        case .swipeUpThenRight: "Swipe up then right"
        case .swipeDownThenLeft: "Swipe down then left"
        case .swipeDownThenRight: "Swipe down then right"
        case .swipeLeftThenUp: "Swipe left then up"
        case .swipeLeftThenDown: "Swipe left then down"
        case .swipeRightThenUp: "Swipe right then up"
        case .swipeRightThenDown: "Swipe right then down"
        case .tap: "Tap"
        case .doubleTap: "Double tap"
        case .tripleTap: "Triple tap"
        case .doubleTapAndHold: "Double tap and hold"
        case .twoFingerSwipeRight: "Two-finger swipe right"
        case .twoFingerSwipeLeft: "Two-finger swipe left"
        case .twoFingerSwipeUp: "Two-finger swipe up"
        case .twoFingerSwipeDown: "Two-finger swipe down"
        case .threeFingerSwipeRight: "Three-finger swipe right"
        case .threeFingerSwipeLeft: "Three-finger swipe left"
        case .threeFingerSwipeUp: "Three-finger swipe up"
        case .threeFingerSwipeDown: "Three-finger swipe down"
        case .fourFingerSwipeRight: "Four-finger swipe right"
        case .fourFingerSwipeLeft: "Four-finger swipe left"
        case .fourFingerSwipeUp: "Four-finger swipe up"
        case .fourFingerSwipeDown: "Four-finger swipe down"
        case .twoFingerTap: "Two-finger tap"
        case .twoFingerDoubleTap: "Two-finger double tap"
        case .twoFingerTripleTap: "Two-finger triple tap"
        case .twoFingerDoubleTapAndHold: "Two-finger double tap and hold"
        case .threeFingerTap: "Three-finger tap"
        case .threeFingerDoubleTap: "Three-finger double tap"
        case .threeFingerTripleTap: "Three-finger triple tap"
        case .fourFingerTap: "Four-finger tap"
        case .fourFingerDoubleTap: "Four-finger double tap"
        case .fourFingerTripleTap: "Four-finger triple tap"
        case .scrollUp: "Scroll up (two fingers drag down)"
        case .scrollDown: "Scroll down (two fingers drag up)"
        case .scrollLeft: "Scroll left (two fingers drag right)"
        case .scrollRight: "Scroll right (two fingers drag left)"
        case .back: "Back"
        case .home: "Home"
        case .recents: "Recent apps"
        case .notifications: "Notifications"
        case .quickSettings: "Quick settings"
        }
    }

    enum Action: Equatable {
        case gesture(Gesture)
        case global(GlobalAction)
    }

    var action: Action {
        func swipe(_ fingers: UInt8, _ d: SwipeDirection...) -> Action {
            .gesture(.swipe(fingers: fingers, directions: d))
        }
        func tap(_ fingers: UInt8, _ taps: UInt8, hold: Bool = false) -> Action {
            .gesture(.tap(fingers: fingers, taps: taps, hold: hold))
        }
        switch self {
        case .swipeRight: return swipe(1, .right)
        case .swipeLeft: return swipe(1, .left)
        case .swipeUp: return swipe(1, .up)
        case .swipeDown: return swipe(1, .down)
        case .swipeUpThenDown: return swipe(1, .up, .down)
        case .swipeDownThenUp: return swipe(1, .down, .up)
        case .swipeLeftThenRight: return swipe(1, .left, .right)
        case .swipeRightThenLeft: return swipe(1, .right, .left)
        case .swipeUpThenLeft: return swipe(1, .up, .left)
        case .swipeUpThenRight: return swipe(1, .up, .right)
        case .swipeDownThenLeft: return swipe(1, .down, .left)
        case .swipeDownThenRight: return swipe(1, .down, .right)
        case .swipeLeftThenUp: return swipe(1, .left, .up)
        case .swipeLeftThenDown: return swipe(1, .left, .down)
        case .swipeRightThenUp: return swipe(1, .right, .up)
        case .swipeRightThenDown: return swipe(1, .right, .down)
        case .tap: return tap(1, 1)
        case .doubleTap: return tap(1, 2)
        case .tripleTap: return tap(1, 3)
        case .doubleTapAndHold: return tap(1, 2, hold: true)
        case .twoFingerSwipeRight: return swipe(2, .right)
        case .twoFingerSwipeLeft: return swipe(2, .left)
        case .twoFingerSwipeUp: return swipe(2, .up)
        case .twoFingerSwipeDown: return swipe(2, .down)
        case .threeFingerSwipeRight: return swipe(3, .right)
        case .threeFingerSwipeLeft: return swipe(3, .left)
        case .threeFingerSwipeUp: return swipe(3, .up)
        case .threeFingerSwipeDown: return swipe(3, .down)
        case .fourFingerSwipeRight: return swipe(4, .right)
        case .fourFingerSwipeLeft: return swipe(4, .left)
        case .fourFingerSwipeUp: return swipe(4, .up)
        case .fourFingerSwipeDown: return swipe(4, .down)
        case .twoFingerTap: return tap(2, 1)
        case .twoFingerDoubleTap: return tap(2, 2)
        case .twoFingerTripleTap: return tap(2, 3)
        case .twoFingerDoubleTapAndHold: return tap(2, 2, hold: true)
        case .threeFingerTap: return tap(3, 1)
        case .threeFingerDoubleTap: return tap(3, 2)
        case .threeFingerTripleTap: return tap(3, 3)
        case .fourFingerTap: return tap(4, 1)
        case .fourFingerDoubleTap: return tap(4, 2)
        case .fourFingerTripleTap: return tap(4, 3)
        // Fingers dragging down show what is further up.
        case .scrollUp: return .gesture(.scroll(direction: .down))
        case .scrollDown: return .gesture(.scroll(direction: .up))
        case .scrollLeft: return .gesture(.scroll(direction: .right))
        case .scrollRight: return .gesture(.scroll(direction: .left))
        case .back: return .global(.back)
        case .home: return .global(.home)
        case .recents: return .global(.recents)
        case .notifications: return .global(.notifications)
        case .quickSettings: return .global(.quickSettings)
        }
    }
}
