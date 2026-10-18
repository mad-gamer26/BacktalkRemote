//! What the phone and its controllers say to each other, sealed (see
//! [`crate::seal`]). JSON, `snake_case`, tagged by `type`.
//!
//! A connection starts with the controller's hello and the host's welcome
//! under the handshake key. The welcome echoes the controller's nonce, so a
//! recorded welcome is no use later, and carries the host's nonce and the
//! audio stream's nonce: from then on both sides use the session key, and
//! the controller can open the audio. Session messages carry a sequence
//! number that only goes up; anything else is dropped as a replay.

use serde::{Deserialize, Serialize};

/// The peer protocol version, in the hello and welcome.
pub const PEER_VERSION: u32 = 1;

/// What one finger, or all of them, did. The same actions as Android's
/// `MotionEvent`, which the phone rebuilds from these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum TouchAction {
    /// The first finger touched.
    Down,
    /// Another finger touched; `changed` says which.
    PointerDown,
    /// Fingers moved.
    Move,
    /// A finger lifted while others stay; `changed` says which.
    PointerUp,
    /// The last finger lifted.
    Up,
    /// The touch was taken away (by the system on the controlling device).
    Cancel,
}

/// One finger: an id that stays the same while it is down, and where it
/// is, from 0 to 1 across and down the controlling device's touch area.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct TouchPointer {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// A change to the touch on the controlling device, with every finger that
/// is down. `time_ms` is the controlling device's own clock, only compared
/// with other events of the same touch, so that the phone replays them
/// with the timing they had.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct TouchEvent {
    pub action: TouchAction,
    pub changed: u32,
    pub time_ms: u64,
    pub pointers: Vec<TouchPointer>,
}

/// Actions the controller can ask for directly, as well as by gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum GlobalAction {
    Back,
    Home,
    Recents,
    Notifications,
    QuickSettings,
}

/// The phone's screen, in pixels, as it is turned now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
}

/// From a controller to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHost {
    /// Handshake. `nonce` is 32 hexadecimal digits; `name` names the
    /// controlling device for the phone's user ("Matthew's iPhone").
    Hello {
        version: u32,
        nonce: String,
        name: String,
    },
    Touch {
        seq: u64,
        event: TouchEvent,
    },
    Global {
        seq: u64,
        action: GlobalAction,
    },
    /// Text to type into the focused field, at the cursor.
    Type {
        seq: u64,
        text: String,
    },
    /// The controller is leaving on purpose.
    Bye {
        seq: u64,
    },
}

/// From the host to a controller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToController {
    /// Handshake. `controller_nonce` echoes the hello's.
    Welcome {
        version: u32,
        controller_nonce: String,
        host_nonce: String,
        stream_nonce: String,
        name: String,
        screen: Screen,
    },
    /// The screen turned or changed size.
    Screen { seq: u64, screen: Screen },
    /// The phone stopped sharing on purpose; `reason` is in words.
    Bye { seq: u64, reason: String },
}

impl ToHost {
    pub fn seq(&self) -> Option<u64> {
        match self {
            ToHost::Hello { .. } => None,
            ToHost::Touch { seq, .. }
            | ToHost::Global { seq, .. }
            | ToHost::Type { seq, .. }
            | ToHost::Bye { seq } => Some(*seq),
        }
    }
}

impl ToController {
    pub fn seq(&self) -> Option<u64> {
        match self {
            ToController::Welcome { .. } => None,
            ToController::Screen { seq, .. } | ToController::Bye { seq, .. } => Some(*seq),
        }
    }
}

/// Keeps sequence numbers going up in each direction.
#[derive(Debug, Default)]
pub struct Sequence {
    sent: u64,
    received: u64,
}

impl Sequence {
    pub fn next_out(&mut self) -> u64 {
        self.sent += 1;
        self.sent
    }

    /// Whether `seq` is new; it then becomes the newest.
    pub fn accept(&mut self, seq: u64) -> bool {
        if seq > self.received {
            self.received = seq;
            true
        } else {
            false
        }
    }
}

/// Hard limits on what the host accepts, so a controller cannot make it
/// do unbounded work.
pub const MAX_POINTERS: usize = 10;
pub const MAX_TYPED_CHARS: usize = 10_000;
pub const MAX_NAME_CHARS: usize = 100;

/// Shortens `name` to [`MAX_NAME_CHARS`] and removes control characters.
pub fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Unnamed device".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Whether a touch event is one the host can rebuild: a known number of
/// fingers, coordinates in range, and the changed finger among them.
pub fn touch_is_valid(event: &TouchEvent) -> bool {
    let count = event.pointers.len();
    if count == 0 || count > MAX_POINTERS {
        return false;
    }
    let in_range = |v: f32| v.is_finite() && (-0.05..=1.05).contains(&v);
    if !event
        .pointers
        .iter()
        .all(|p| in_range(p.x) && in_range(p.y))
    {
        return false;
    }
    let mut ids: Vec<u32> = event.pointers.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != count {
        return false;
    }
    match event.action {
        TouchAction::Down | TouchAction::Up => count == 1,
        TouchAction::PointerDown | TouchAction::PointerUp => {
            count >= 2 && ids.contains(&event.changed)
        }
        TouchAction::Move | TouchAction::Cancel => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(action: TouchAction, ids: &[u32], changed: u32) -> TouchEvent {
        TouchEvent {
            action,
            changed,
            time_ms: 5,
            pointers: ids
                .iter()
                .map(|&id| TouchPointer {
                    id,
                    x: 0.5,
                    y: 0.25,
                })
                .collect(),
        }
    }

    #[test]
    fn messages_are_tagged_snake_case() {
        let m = ToHost::Touch {
            seq: 4,
            event: event(TouchAction::PointerDown, &[0, 1], 1),
        };
        let text = serde_json::to_string(&m).unwrap();
        assert!(
            text.starts_with(
                r#"{"type":"touch","seq":4,"event":{"action":"pointer_down","changed":1"#
            ),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<ToHost>(&text).unwrap(), m);
        let g = serde_json::to_string(&ToHost::Global {
            seq: 1,
            action: GlobalAction::QuickSettings,
        })
        .unwrap();
        assert_eq!(g, r#"{"type":"global","seq":1,"action":"quick_settings"}"#);
    }

    #[test]
    fn sequence_numbers_only_go_up() {
        let mut s = Sequence::default();
        assert_eq!(s.next_out(), 1);
        assert_eq!(s.next_out(), 2);
        assert!(s.accept(1));
        assert!(!s.accept(1));
        assert!(s.accept(5));
        assert!(!s.accept(4));
    }

    #[test]
    fn touch_validation() {
        assert!(touch_is_valid(&event(TouchAction::Down, &[3], 3)));
        assert!(!touch_is_valid(&event(TouchAction::Down, &[0, 1], 0)));
        assert!(touch_is_valid(&event(TouchAction::PointerUp, &[0, 1], 1)));
        assert!(!touch_is_valid(&event(TouchAction::PointerUp, &[0, 1], 7)));
        assert!(!touch_is_valid(&event(TouchAction::Move, &[], 0)));
        assert!(!touch_is_valid(&event(TouchAction::Move, &[1, 1], 0)));
        let ids: Vec<u32> = (0..11).collect();
        assert!(!touch_is_valid(&event(TouchAction::Move, &ids, 0)));
        let mut far = event(TouchAction::Move, &[0], 0);
        far.pointers[0].x = 3.0;
        assert!(!touch_is_valid(&far));
        far.pointers[0].x = f32::NAN;
        assert!(!touch_is_valid(&far));
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Pixel\n8 "), "Pixel8");
        assert_eq!(clean_name("\u{7}"), "Unnamed device");
        assert_eq!(clean_name(&"a".repeat(300)).len(), MAX_NAME_CHARS);
    }
}
