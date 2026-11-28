use super::Gesture;
use std::time::{Duration, Instant};

pub const PAD_END: i32 = 1000;
const CORNER: i32 = 110;
const EDGE: i32 = 85;
const SWIPE: i32 = 155;

#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub center: Point,
    pub fingers: u8,
    pub ids: u32,
    pub button: bool,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Action(Gesture),
    Seek(i32),
    FinishSeek,
}

struct Contact {
    start: Frame,
    at: Instant,
    button: bool,
    seeking: bool,
}

#[derive(Default)]
pub struct Gestures {
    contact: Option<Contact>,
    wait_for_lift: bool,
}

impl Gestures {
    pub fn abort(&mut self) {
        self.contact = None;
        self.wait_for_lift = true;
    }

    pub fn frame(
        &mut self,
        now: Instant,
        input: Frame,
        actions: u32,
        media: bool,
    ) -> Option<Event> {
        if input.fingers == 0 {
            self.wait_for_lift = false;
            return self
                .contact
                .take()
                .filter(|contact| contact.seeking)
                .map(|_| Event::FinishSeek);
        }
        if self.wait_for_lift {
            return None;
        }
        if let Some(contact) = &self.contact
            && contact.start.ids != input.ids
        {
            if contact.seeking || input.fingers < contact.start.fingers {
                let finish = contact.seeking;
                self.abort();
                return finish.then_some(Event::FinishSeek);
            }
            self.contact = None;
        }
        let contact = self.contact.get_or_insert(Contact {
            start: input,
            at: now,
            button: false,
            seeking: false,
        });
        let dx = input.center.x - contact.start.center.x;
        let dy = input.center.y - contact.start.center.y;
        if input.fingers == 4 && media {
            if contact.seeking || dx.abs() >= 45 && aligned(dx, dy) {
                contact.seeking = true;
                return Some(Event::Seek(dx));
            }
            return None;
        }
        let enabled = |gesture: Gesture| actions & (1 << gesture as u8) != 0;
        let Point { x, y } = contact.start.center;
        let clicked = input.button && !contact.button;
        contact.button = input.button;
        let action = if input.fingers == 1
            && clicked
            && dx.abs().max(dy.abs()) < 60
            && y >= PAD_END - CORNER
        {
            if x <= CORNER {
                Some(Gesture::BottomLeft)
            } else if x >= PAD_END - CORNER {
                Some(Gesture::BottomRight)
            } else {
                None
            }
        } else if !input.button && now.duration_since(contact.at) <= Duration::from_millis(650) {
            match input.fingers {
                1 | 2 if x <= EDGE && dx >= SWIPE && aligned(dx, dy) => {
                    Some(if input.fingers == 1 {
                        Gesture::LeftOne
                    } else {
                        Gesture::LeftTwo
                    })
                }
                1 | 2 if x >= PAD_END - EDGE && dx <= -SWIPE && aligned(dx, dy) => {
                    Some(if input.fingers == 1 {
                        Gesture::RightOne
                    } else {
                        Gesture::Inward
                    })
                }
                1 | 2 if y <= EDGE && dy >= SWIPE && aligned(dy, dx) => {
                    Some(if input.fingers == 1 {
                        Gesture::TopOne
                    } else {
                        Gesture::TopTwo
                    })
                }
                1 | 2 if y >= PAD_END - EDGE && dy <= -SWIPE && aligned(dy, dx) => {
                    Some(if input.fingers == 1 {
                        Gesture::BottomOne
                    } else {
                        Gesture::BottomTwo
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        let action = action.filter(|gesture| enabled(*gesture))?;
        self.abort();
        Some(Event::Action(action))
    }
}

fn aligned(primary: i32, perpendicular: i32) -> bool {
    primary.abs() * 2 >= perpendicular.abs() * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(fingers: u8, x: i32, y: i32, button: bool) -> Frame {
        Frame {
            fingers,
            center: Point { x, y },
            ids: (1 << fingers) - 1,
            button,
        }
    }

    #[test]
    fn bottom_corners_require_a_click_and_fire_once() {
        let mut tracker = Gestures::default();
        let t = Instant::now();
        let actions = 1 << Gesture::BottomRight as u8;
        assert_eq!(
            tracker.frame(t, frame(1, 970, 970, false), actions, false),
            None
        );
        assert_eq!(
            tracker.frame(t, frame(1, 965, 965, true), actions, false),
            Some(Event::Action(Gesture::BottomRight))
        );
        assert_eq!(
            tracker.frame(t, frame(1, 965, 965, true), actions, false),
            None
        );
    }

    #[test]
    fn two_finger_swipe_must_start_at_the_edge_and_move_inward() {
        let mut tracker = Gestures::default();
        let t = Instant::now();
        let actions = 1 << Gesture::TopTwo as u8;
        assert_eq!(
            tracker.frame(t, frame(1, 302, 32, false), actions, false),
            None
        );
        assert_eq!(
            tracker.frame(t, frame(2, 323, 35, false), actions, false),
            None
        );
        assert_eq!(
            tracker.frame(t, frame(2, 390, 90, false), actions, false),
            None
        );
        assert_eq!(
            tracker.frame(t, frame(2, 369, 245, false), actions, false),
            Some(Event::Action(Gesture::TopTwo))
        );
    }

    #[test]
    fn four_finger_seek_is_reversible_and_ends_on_lift() {
        let mut tracker = Gestures::default();
        let t = Instant::now();
        assert_eq!(tracker.frame(t, frame(4, 240, 394, false), 0, true), None);
        assert_eq!(tracker.frame(t, frame(4, 245, 519, false), 0, true), None);
        assert_eq!(
            tracker.frame(t, frame(4, 452, 400, false), 0, true),
            Some(Event::Seek(212))
        );
        assert_eq!(
            tracker.frame(t, frame(4, 216, 398, false), 0, true),
            Some(Event::Seek(-24))
        );
        assert_eq!(
            tracker.frame(t, frame(3, 210, 388, false), 0, true),
            Some(Event::FinishSeek)
        );
        assert_eq!(tracker.frame(t, Frame::default(), 0, true), None);
    }
}
