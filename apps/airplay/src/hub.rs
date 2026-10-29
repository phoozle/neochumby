//! The service's memory of what the companion said about the track, so
//! that a screen opened mid-song is told it all at once, and whether a
//! frame starts a stream (the service then asks the session to open the
//! screen).

use crate::frames::{COVER, Frame, LINK, NAME, Play, STATE, TRACK};

#[derive(Default)]
pub struct Hub {
    linked: bool,
    /// The latest NAME, TRACK, COVER and STATE frames, as they came.
    last: [Option<Vec<u8>>; 4],
    play: Play,
}

/// What to do with a frame from the companion.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// Pass it on to the screens.
    pub forward: bool,
    /// Playing has begun from stopped: a new stream, not a resume.
    pub started: bool,
}

fn slot(ty: u8) -> Option<usize> {
    [NAME, TRACK, COVER, STATE].iter().position(|&t| t == ty)
}

impl Hub {
    /// Takes in a frame from the companion (not audio).
    pub fn update(&mut self, frame: &[u8]) -> Update {
        let Some(i) = frame.first().and_then(|&t| slot(t)) else { return Update::default() };
        // Only a frame that parses is kept: a screen is never sent one it
        // would drop.
        let Some(parsed) = Frame::parse(frame) else { return Update::default() };
        self.last[i] = Some(frame.to_vec());
        let mut started = false;
        if let Frame::State(s) = parsed {
            started = self.play == Play::Stopped && s.play == Play::Playing;
            self.play = s.play;
        }
        Update { forward: true, started }
    }

    /// The companion came or went; the frame to send the screens. With it
    /// gone, what it said is forgotten.
    pub fn link(&mut self, up: bool) -> Vec<u8> {
        self.linked = up;
        if !up {
            self.last = Default::default();
            self.play = Play::Stopped;
        }
        vec![LINK, up as u8]
    }

    /// Everything a new screen needs, in order.
    pub fn replay(&self) -> Vec<Vec<u8>> {
        let mut out = vec![vec![LINK, self.linked as u8]];
        out.extend(self.last.iter().flatten().cloned());
        out
    }

    /// Whether the companion is connected.
    pub fn linked(&self) -> bool {
        self.linked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{AUDIO, END, State, Track};

    fn state(play: Play) -> Vec<u8> {
        Frame::State(State { play, ..State::default() }).encode()
    }

    #[test]
    fn a_new_screen_is_told_everything() {
        let mut h = Hub::default();
        assert_eq!(h.link(true), [LINK, 1]);
        let name = Frame::Name("Den".into()).encode();
        let t1 = Frame::Track(Track { title: "One".into(), ..Track::default() }).encode();
        let t2 = Frame::Track(Track { title: "Two".into(), ..Track::default() }).encode();
        for f in [&name, &t1, &t2] {
            assert!(h.update(f).forward);
        }
        assert_eq!(h.replay(), [vec![LINK, 1], name, t2]);
        assert_eq!(h.update(&[AUDIO, 1, 2]), Update::default());
        assert_eq!(h.update(&[END]), Update::default());
        assert_eq!(h.update(&[TRACK, 9]), Update::default(), "a frame that doesn't parse");
        h.link(false);
        assert_eq!(h.replay(), [vec![LINK, 0]]);
    }

    #[test]
    fn only_a_start_from_stopped_starts() {
        let mut h = Hub::default();
        h.link(true);
        assert!(h.update(&state(Play::Playing)).started);
        assert!(!h.update(&state(Play::Playing)).started);
        assert!(!h.update(&state(Play::Paused)).started);
        assert!(!h.update(&state(Play::Playing)).started, "a resume");
        assert!(!h.update(&state(Play::Stopped)).started);
        assert!(h.update(&state(Play::Playing)).started);
        h.link(false);
        h.link(true);
        assert!(h.update(&state(Play::Playing)).started);
    }
}
