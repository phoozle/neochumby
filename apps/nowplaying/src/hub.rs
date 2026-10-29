//! The service's memory of what the companion said about the track, so
//! that a screen opened mid-song is told it all at once.

use crate::frames::{COVER, Frame, LINK, NAME, STATE, TRACK};

#[derive(Default)]
pub struct Hub {
    linked: bool,
    /// The latest NAME, TRACK, COVER and STATE frames, as they came.
    last: [Option<Vec<u8>>; 4],
}

fn slot(ty: u8) -> Option<usize> {
    [NAME, TRACK, COVER, STATE].iter().position(|&t| t == ty)
}

impl Hub {
    /// Takes in a frame from the companion; whether to pass it on to the
    /// screens. Only a frame that parses is kept and passed on: a screen is
    /// never sent one it would drop.
    pub fn update(&mut self, frame: &[u8]) -> bool {
        let Some(i) = frame.first().and_then(|&t| slot(t)) else { return false };
        if Frame::parse(frame).is_none() {
            return false;
        }
        self.last[i] = Some(frame.to_vec());
        true
    }

    /// The service has both its companion and the airplay app's feed, or
    /// not; the frame to send the screens, if that changed. Without them,
    /// what the companion said is forgotten.
    pub fn link(&mut self, up: bool) -> Option<Vec<u8>> {
        if up == self.linked {
            return None;
        }
        self.linked = up;
        if !up {
            self.last = Default::default();
        }
        Some(vec![LINK, up as u8])
    }

    /// Everything a new screen needs, in order.
    pub fn replay(&self) -> Vec<Vec<u8>> {
        let mut out = vec![vec![LINK, self.linked as u8]];
        out.extend(self.last.iter().flatten().cloned());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::Track;

    #[test]
    fn a_new_screen_is_told_everything() {
        let mut h = Hub::default();
        assert_eq!(h.link(true), Some(vec![LINK, 1]));
        assert_eq!(h.link(true), None);
        let name = Frame::Name("Den".into()).encode();
        let t1 = Frame::Track(Track { title: "One".into(), ..Track::default() }).encode();
        let t2 = Frame::Track(Track { title: "Two".into(), ..Track::default() }).encode();
        for f in [&name, &t1, &t2] {
            assert!(h.update(f));
        }
        assert_eq!(h.replay(), [vec![LINK, 1], name, t2]);
        assert!(!h.update(&[0, 1, 2]), "not a frame for the screen");
        assert!(!h.update(&[TRACK, 9]), "a frame that doesn't parse");
        assert_eq!(h.link(false), Some(vec![LINK, 0]));
        assert_eq!(h.replay(), [vec![LINK, 0]]);
    }
}
