//! What the screen shows, from the frames the service passes on: the
//! receiver's name, the track, its cover and where it has got to. Time is
//! the screen's own clock in milliseconds, so that the progress moves
//! between the sender's reports; all of it is integer arithmetic.

use crate::frames::{Control, Cover, Frame, Play, State, Track};

#[derive(Default)]
pub struct NowPlaying {
    /// The service has its companion on neochumbyd.
    pub linked: bool,
    /// The receiver's name, as phones list it.
    pub name: String,
    pub track: Track,
    /// 0 by 0 when there is none.
    pub cover: Cover,
    pub state: State,
    /// When `state` came, on the screen's clock.
    at_ms: u64,
}

impl NowPlaying {
    /// Takes in a frame from the service; whether anything shown changed.
    pub fn apply(&mut self, f: Frame, now_ms: u64) -> bool {
        match f {
            Frame::Link(up) => {
                let changed = up != self.linked;
                self.linked = up;
                if !up {
                    // What played came over the link that's gone.
                    self.track = Track::default();
                    self.cover = Cover::default();
                    self.state = State::default();
                }
                changed
            }
            Frame::Name(n) => replace(&mut self.name, n),
            Frame::Track(t) => replace(&mut self.track, t),
            Frame::Cover(c) => replace(&mut self.cover, c),
            Frame::State(s) => {
                self.at_ms = now_ms;
                replace(&mut self.state, s)
            }
            Frame::Control(_) => false,
        }
    }

    /// Nothing to show but the receiver: the idle screen.
    pub fn idle(&self) -> bool {
        !self.linked || (self.state.play == Play::Stopped && self.track.is_empty())
    }

    /// How far into the track, now: the last report, plus the time since
    /// while playing, never past the end.
    pub fn elapsed_ms(&self, now_ms: u64) -> u32 {
        let mut e = self.state.elapsed_ms as u64;
        if self.state.play == Play::Playing {
            e += now_ms.saturating_sub(self.at_ms);
        }
        if self.state.duration_ms > 0 {
            e = e.min(self.state.duration_ms as u64);
        }
        e.min(u32::MAX as u64) as u32
    }

    /// A button was pressed: show its effect at once, as the sender takes
    /// a second or two to say so. Its own report then has the last word.
    pub fn pressed(&mut self, c: Control, now_ms: u64) {
        let play = match (c, self.state.play) {
            (Control::PlayPause, Play::Playing) | (Control::Pause, _) => Play::Paused,
            (Control::PlayPause, _) | (Control::Play, _) => Play::Playing,
            (Control::Next | Control::Previous, _) => return,
        };
        let elapsed_ms = self.elapsed_ms(now_ms);
        self.state = State { play, elapsed_ms, ..self.state };
        self.at_ms = now_ms;
    }
}

fn replace<T: PartialEq>(slot: &mut T, v: T) -> bool {
    let changed = *slot != v;
    *slot = v;
    changed
}

/// A time as a player shows it: 4:05, or 1:02:03 past the hour.
pub fn clock(ms: u32) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(elapsed_ms: u32, duration_ms: u32) -> Frame {
        Frame::State(State { play: Play::Playing, controls: true, elapsed_ms, duration_ms })
    }

    #[test]
    fn progress_moves_while_playing_and_stops_at_the_end() {
        let mut np = NowPlaying::default();
        np.apply(Frame::Link(true), 0);
        np.apply(playing(10_000, 20_000), 1_000);
        assert_eq!(np.elapsed_ms(1_000), 10_000);
        assert_eq!(np.elapsed_ms(4_500), 13_500);
        assert_eq!(np.elapsed_ms(60_000), 20_000);
        np.apply(
            Frame::State(State { play: Play::Paused, controls: true, elapsed_ms: 12_000, duration_ms: 20_000 }),
            5_000,
        );
        assert_eq!(np.elapsed_ms(9_000), 12_000);
    }

    #[test]
    fn idle_until_something_plays_and_after_the_link_goes() {
        let mut np = NowPlaying::default();
        assert!(np.idle());
        assert!(np.apply(Frame::Link(true), 0));
        assert!(np.apply(Frame::Name("Den".into()), 0));
        assert!(np.idle());
        np.apply(Frame::Track(Track { title: "T".into(), ..Track::default() }), 0);
        assert!(!np.idle());
        assert!(!np.apply(Frame::Track(Track { title: "T".into(), ..Track::default() }), 0));
        np.apply(Frame::Link(false), 0);
        assert!(np.idle());
        assert!(np.track.is_empty());
        assert_eq!(np.name, "Den");
    }

    #[test]
    fn play_pause_shows_at_once() {
        let mut np = NowPlaying::default();
        np.apply(playing(1_000, 0), 0);
        np.pressed(Control::PlayPause, 2_000);
        assert_eq!(np.state.play, Play::Paused);
        assert_eq!(np.elapsed_ms(10_000), 3_000);
        np.pressed(Control::PlayPause, 10_000);
        assert_eq!(np.state.play, Play::Playing);
        assert_eq!(np.elapsed_ms(11_000), 4_000);
        np.pressed(Control::Next, 11_000);
        assert_eq!(np.state.play, Play::Playing);
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(65_999), "1:05");
        assert_eq!(clock(3_723_000), "1:02:03");
    }
}
