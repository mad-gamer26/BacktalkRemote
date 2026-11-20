import AppKit

/// The Mac's keys as Android names them: Android's `KeyEvent` key codes
/// and `META_` bits. Keys are matched by where they are on the keyboard
/// (the Mac's key codes), as a keyboard plugged into the phone would be;
/// the phone's keyboard layout turns them into characters.
enum AndroidKeys {
    /// Android's key code for a Mac key code.
    static func keyCode(forMac code: UInt16) -> UInt32? { map[code] }

    /// Modifier keys, by Mac key code.
    enum Modifier: CaseIterable {
        case shiftLeft, shiftRight, controlLeft, controlRight
        case optionLeft, optionRight, commandLeft, commandRight

        init?(macCode: UInt16) {
            switch macCode {
            case 56: self = .shiftLeft
            case 60: self = .shiftRight
            case 59: self = .controlLeft
            case 62: self = .controlRight
            case 58: self = .optionLeft
            case 61: self = .optionRight
            case 55: self = .commandLeft
            case 54: self = .commandRight
            default: return nil
            }
        }

        /// Android's key: Command is Meta, Option is Alt.
        var androidCode: UInt32 {
            switch self {
            case .shiftLeft: 59
            case .shiftRight: 60
            case .controlLeft: 113
            case .controlRight: 114
            case .optionLeft: 57
            case .optionRight: 58
            case .commandLeft: 117
            case .commandRight: 118
            }
        }

        /// Android's `META_` bits while this key is down.
        var metaBits: UInt32 {
            switch self {
            case .shiftLeft: 0x1 | 0x40
            case .shiftRight: 0x1 | 0x80
            case .controlLeft: 0x1000 | 0x2000
            case .controlRight: 0x1000 | 0x4000
            case .optionLeft: 0x2 | 0x10
            case .optionRight: 0x2 | 0x20
            case .commandLeft: 0x10000 | 0x20000
            case .commandRight: 0x10000 | 0x40000
            }
        }

        /// The Mac's flag for this key's kind, to tell whether any is down.
        var flag: NSEvent.ModifierFlags {
            switch self {
            case .shiftLeft, .shiftRight: .shift
            case .controlLeft, .controlRight: .control
            case .optionLeft, .optionRight: .option
            case .commandLeft, .commandRight: .command
            }
        }

        var isControl: Bool { flag == .control }
        var isOption: Bool { flag == .option }
    }

    /// Num Lock is on, so that the keypad types numbers, as a Mac's does.
    static let numLockOn: UInt32 = 0x200000

    private static let map: [UInt16: UInt32] = [
        // Letters: Android's A is 29, in alphabetical order.
        0: 29, 11: 30, 8: 31, 2: 32, 14: 33, 3: 34, 5: 35, 4: 36, 34: 37, 38: 38, 40: 39,
        37: 40, 46: 41, 45: 42, 31: 43, 35: 44, 12: 45, 15: 46, 1: 47, 17: 48, 32: 49,
        9: 50, 13: 51, 7: 52, 16: 53, 6: 54,
        // Digits: Android's 0 is 7.
        29: 7, 18: 8, 19: 9, 20: 10, 21: 11, 23: 12, 22: 13, 26: 14, 28: 15, 25: 16,
        // Punctuation.
        43: 55, 47: 56, 50: 68, 27: 69, 24: 70, 33: 71, 30: 72, 42: 73, 41: 74, 39: 75,
        44: 76, 10: 68,
        // Whitespace and editing.
        48: 61, 49: 62, 36: 66, 51: 67, 117: 112, 53: 111, 114: 124,
        // Navigation.
        123: 21, 124: 22, 126: 19, 125: 20, 115: 122, 119: 123, 116: 92, 121: 93,
        // Function keys.
        122: 131, 120: 132, 99: 133, 118: 134, 96: 135, 97: 136, 98: 137, 100: 138,
        101: 139, 109: 140, 103: 141, 111: 142,
        // Keypad.
        82: 144, 83: 145, 84: 146, 85: 147, 86: 148, 87: 149, 88: 150, 89: 151, 91: 152,
        92: 153, 75: 154, 67: 155, 78: 156, 69: 157, 65: 158, 76: 160, 81: 161, 71: 143,
        // Japanese keyboards.
        93: 216, 94: 217, 102: 212, 104: 218,
    ]
}
