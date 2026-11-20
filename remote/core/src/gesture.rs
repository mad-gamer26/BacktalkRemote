//! Backtalk gestures made from touches, for controlling devices without a
//! touch screen (a Mac's keyboard): a gesture becomes the touch events a
//! finger drawing it would make, with timing that Backtalk's gesture
//! detection recognizes, so the phone handles it exactly as a gesture on
//! its own screen, with no change on the phone.

use std::time::Duration;

use crate::peer::{Screen, TouchAction, TouchPointer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SwipeDirection {
    Up,
    Down,
    Left,
    Right,
}

/// A gesture on the phone's screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Gesture {
    /// `fingers` (1 to 4) swipe one way, or one way then another: one or two
    /// `directions` (right then down, up then down).
    Swipe {
        fingers: u8,
        directions: Vec<SwipeDirection>,
    },
    /// `fingers` (1 to 4) tap `taps` times (1 to 3). With `hold`, the last
    /// tap stays down, as for double tap and hold.
    Tap { fingers: u8, taps: u8, hold: bool },
    /// Two fingers drag slowly, which scrolls rather than being a gesture:
    /// `Up` moves the fingers up, showing what is further down.
    Scroll { direction: SwipeDirection },
}

/// One touch event of a gesture, `wait` after the one before.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub wait: Duration,
    pub action: TouchAction,
    pub changed: u32,
    pub pointers: Vec<TouchPointer>,
}

/// How often a moving finger reports, as a touch screen does.
const SAMPLE_MS: u64 = 10;
/// A one-finger stroke: fast enough for a swipe, never a drag.
const STROKE_MS: u64 = 120;
const MULTI_STROKE_MS: u64 = 150;
const SCROLL_MS: u64 = 600;
/// A tap: down long enough to count, short enough not to explore.
const TAP_DOWN_MS: u64 = 40;
/// Between taps of a double or triple tap: well inside the multi-tap
/// timeout (Android's double tap timeout less 50 ms, 250 ms by default).
const TAP_GAP_MS: u64 = 80;
const HOLD_MS: u64 = 1_200;
/// Between fingers going down or up, as hands do: inside the tap timeout
/// (100 ms for all of them).
const FINGER_GAP_MS: u64 = 10;
/// After a tap gesture, before the next gesture: past the multi-tap
/// completion timeout, so a next tap is not taken as part of this one.
const AFTER_TAP_MS: u64 = 450;
const AFTER_SWIPE_MS: u64 = 60;
/// How far apart fingers are, across the screen.
const FINGER_SPACING: f32 = 0.08;
/// A stroke's length across the screen; up and down, the same distance on
/// the phone's screen.
const STROKE_ACROSS: f32 = 0.36;

impl Gesture {
    /// How long to leave after this gesture before the next.
    pub fn pause_after(&self) -> Duration {
        Duration::from_millis(match self {
            Gesture::Tap { .. } => AFTER_TAP_MS,
            _ => AFTER_SWIPE_MS,
        })
    }

    /// The touch events of this gesture, on a screen of `screen`'s shape
    /// (a 9 by 20 phone held upright if unknown).
    pub fn steps(&self, screen: Option<Screen>) -> Vec<Step> {
        // Height over width, so that a stroke up is as long on the phone as
        // one across.
        let aspect = screen
            .filter(|s| s.width > 0 && s.height > 0)
            .map_or(20.0 / 9.0, |s| s.height as f32 / s.width as f32);
        let mut plan = Plan::default();
        match self {
            Gesture::Swipe {
                fingers,
                directions,
            } => {
                let fingers = (*fingers).clamp(1, 4);
                let directions: Vec<_> = directions.iter().take(2).copied().collect();
                if directions.is_empty() {
                    return Vec::new();
                }
                let path = stroke_path(&directions, aspect);
                let per = if fingers == 1 {
                    STROKE_MS
                } else {
                    MULTI_STROKE_MS
                };
                plan.drag(fingers, &path, per * directions.len() as u64);
            }
            Gesture::Scroll { direction } => {
                let d = offset(*direction, 0.4, aspect);
                let start = (0.5 - d.0 / 2.0, 0.5 - d.1 / 2.0);
                plan.drag(2, &[start, (start.0 + d.0, start.1 + d.1)], SCROLL_MS);
            }
            Gesture::Tap {
                fingers,
                taps,
                hold,
            } => {
                let fingers = (*fingers).clamp(1, 4);
                let taps = (*taps).clamp(1, 3);
                for i in 0..taps {
                    if i > 0 {
                        plan.wait(TAP_GAP_MS);
                    }
                    let held = *hold && i + 1 == taps;
                    plan.tap(fingers, if held { HOLD_MS } else { TAP_DOWN_MS });
                }
            }
        }
        plan.steps
    }
}

/// How far a stroke of `length` across goes in `direction`, in the
/// controller's 0-to-1 coordinates.
fn offset(direction: SwipeDirection, length: f32, aspect: f32) -> (f32, f32) {
    let down = length / aspect;
    match direction {
        SwipeDirection::Up => (0.0, -down),
        SwipeDirection::Down => (0.0, down),
        SwipeDirection::Left => (-length, 0.0),
        SwipeDirection::Right => (length, 0.0),
    }
}

/// The corners of a stroke in `directions`, centered on the screen.
fn stroke_path(directions: &[SwipeDirection], aspect: f32) -> Vec<(f32, f32)> {
    let mut points = vec![(0.0f32, 0.0f32)];
    for &d in directions {
        let (dx, dy) = offset(d, STROKE_ACROSS, aspect);
        let last = *points.last().expect("has a point");
        points.push((last.0 + dx, last.1 + dy));
    }
    // Center the whole path, so that it stays on the screen.
    let (min_x, max_x) = points
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (min_y, max_y) = points
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let shift = (0.5 - (min_x + max_x) / 2.0, 0.5 - (min_y + max_y) / 2.0);
    points
        .into_iter()
        .map(|p| (p.0 + shift.0, p.1 + shift.1))
        .collect()
}

/// Where along `path` a finger is at `t` from 0 to 1, by distance.
fn along(path: &[(f32, f32)], t: f32) -> (f32, f32) {
    let lengths: Vec<f32> = path
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .collect();
    let total: f32 = lengths.iter().sum();
    if total <= 0.0 {
        return path[0];
    }
    let mut left = t.clamp(0.0, 1.0) * total;
    for (w, len) in path.windows(2).zip(&lengths) {
        if left <= *len && *len > 0.0 {
            let f = left / len;
            return (
                w[0].0 + (w[1].0 - w[0].0) * f,
                w[0].1 + (w[1].1 - w[0].1) * f,
            );
        }
        left -= len;
    }
    *path.last().expect("has a point")
}

#[derive(Default)]
struct Plan {
    steps: Vec<Step>,
    pending_wait: u64,
}

impl Plan {
    fn wait(&mut self, ms: u64) {
        self.pending_wait += ms;
    }

    fn push(&mut self, action: TouchAction, changed: u32, pointers: Vec<TouchPointer>) {
        self.steps.push(Step {
            wait: Duration::from_millis(std::mem::take(&mut self.pending_wait)),
            action,
            changed,
            pointers,
        });
    }

    /// `fingers` side by side, the first at `at`.
    fn hand(fingers: u8, at: (f32, f32)) -> Vec<TouchPointer> {
        let left = at.0 - FINGER_SPACING * f32::from(fingers - 1) / 2.0;
        (0..u32::from(fingers))
            .map(|id| TouchPointer {
                id,
                x: left + FINGER_SPACING * id as f32,
                y: at.1,
            })
            .collect()
    }

    fn put_down(&mut self, fingers: u8, at: (f32, f32)) {
        let hand = Self::hand(fingers, at);
        for n in 1..=hand.len() {
            if n > 1 {
                self.wait(FINGER_GAP_MS);
            }
            let action = if n == 1 {
                TouchAction::Down
            } else {
                TouchAction::PointerDown
            };
            self.push(action, n as u32 - 1, hand[..n].to_vec());
        }
    }

    /// Lifts the last finger to touch first, as the touch screen reports:
    /// the lifting finger is still in its event.
    fn lift(&mut self, fingers: u8, at: (f32, f32)) {
        let hand = Self::hand(fingers, at);
        for n in (1..=hand.len()).rev() {
            let action = if n == 1 {
                TouchAction::Up
            } else {
                TouchAction::PointerUp
            };
            self.push(action, n as u32 - 1, hand[..n].to_vec());
            if n > 1 {
                self.wait(FINGER_GAP_MS);
            }
        }
    }

    fn tap(&mut self, fingers: u8, down_ms: u64) {
        let at = (0.5, 0.5);
        self.put_down(fingers, at);
        // A long hold reports now and then, as a resting finger does.
        let mut left = down_ms;
        while left > 100 {
            self.wait(100);
            left -= 100;
            self.push(TouchAction::Move, 0, Self::hand(fingers, at));
        }
        self.wait(left);
        self.lift(fingers, at);
    }

    fn drag(&mut self, fingers: u8, path: &[(f32, f32)], ms: u64) {
        let start = path[0];
        self.put_down(fingers, start);
        let samples = (ms / SAMPLE_MS).max(1);
        for i in 1..=samples {
            self.wait(SAMPLE_MS);
            let p = along(path, i as f32 / samples as f32);
            self.push(TouchAction::Move, 0, Self::hand(fingers, p));
        }
        self.lift(fingers, *path.last().expect("has a point"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{TouchEvent, touch_is_valid};

    fn valid(steps: &[Step]) {
        assert!(!steps.is_empty());
        assert_eq!(steps.first().unwrap().action, TouchAction::Down);
        assert_eq!(steps.last().unwrap().action, TouchAction::Up);
        for s in steps {
            let event = TouchEvent {
                action: s.action,
                changed: s.changed,
                time_ms: 0,
                pointers: s.pointers.clone(),
            };
            assert!(touch_is_valid(&event), "{s:?}");
            for p in &s.pointers {
                assert!((0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y));
            }
        }
    }

    #[test]
    fn every_gesture_makes_valid_touches() {
        use SwipeDirection::*;
        let screen = Some(Screen {
            width: 1080,
            height: 2400,
        });
        for fingers in 1..=4 {
            for a in [Up, Down, Left, Right] {
                valid(
                    &Gesture::Swipe {
                        fingers,
                        directions: vec![a],
                    }
                    .steps(screen),
                );
                for b in [Up, Down, Left, Right] {
                    valid(
                        &Gesture::Swipe {
                            fingers,
                            directions: vec![a, b],
                        }
                        .steps(screen),
                    );
                }
            }
            for taps in 1..=3 {
                for hold in [false, true] {
                    valid(
                        &Gesture::Tap {
                            fingers,
                            taps,
                            hold,
                        }
                        .steps(screen),
                    );
                }
            }
        }
        for d in [Up, Down, Left, Right] {
            valid(&Gesture::Scroll { direction: d }.steps(None));
        }
    }

    #[test]
    fn a_swipe_right_moves_right_in_time() {
        let steps = Gesture::Swipe {
            fingers: 1,
            directions: vec![SwipeDirection::Right],
        }
        .steps(None);
        let first = &steps[0].pointers[0];
        let last = &steps.last().unwrap().pointers[0];
        assert!(last.x - first.x > 0.3);
        assert!((last.y - first.y).abs() < 0.001);
        let total: Duration = steps.iter().map(|s| s.wait).sum();
        assert_eq!(total, Duration::from_millis(STROKE_MS));
    }

    #[test]
    fn a_double_tap_is_two_taps() {
        let steps = Gesture::Tap {
            fingers: 1,
            taps: 2,
            hold: false,
        }
        .steps(None);
        let downs = steps
            .iter()
            .filter(|s| s.action == TouchAction::Down)
            .count();
        let ups = steps.iter().filter(|s| s.action == TouchAction::Up).count();
        assert_eq!((downs, ups), (2, 2));
    }

    #[test]
    fn three_fingers_go_down_and_up_in_order() {
        let steps = Gesture::Tap {
            fingers: 3,
            taps: 1,
            hold: false,
        }
        .steps(None);
        let actions: Vec<_> = steps.iter().map(|s| (s.action, s.pointers.len())).collect();
        assert_eq!(
            actions,
            vec![
                (TouchAction::Down, 1),
                (TouchAction::PointerDown, 2),
                (TouchAction::PointerDown, 3),
                (TouchAction::PointerUp, 3),
                (TouchAction::PointerUp, 2),
                (TouchAction::Up, 1),
            ]
        );
    }
}
