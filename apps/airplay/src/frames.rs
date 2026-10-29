//! AirPlay's frames: what the companion sends the Chumby, what the Chumby
//! sends back, and what the service and the screen say to each other. Each
//! frame's first byte is its type; the rest is its body.
//!
//! From the companion (neochumby_proto::frame, as Msg::Data):
//!
//!   [0] AUDIO + PCM      S16LE stereo at 44.1 kHz, as it comes
//!   [1] END              the audio has stopped for now: the speaker can go
//!   [2] NAME + UTF-8     the receiver's name, as phones list it
//!   [3] TRACK            title, artist, album and the sender's name, each
//!                        a u8 length and that many bytes of UTF-8
//!   [4] COVER            u16 width, u16 height (big-endian), then the
//!                        cover art as RGB565 pixels, little-endian, a row
//!                        at a time; 0 by 0 for none. The companion scales
//!                        it to fit [`COVER_SIDE`], as the ARM926 has no
//!                        FPU and shouldn't decode JPEGs.
//!   [5] STATE            u8 play (0 stopped, 1 playing, 2 paused), u8
//!                        flags (1: the sender takes remote control), u32
//!                        elapsed and u32 duration in ms (big-endian; a
//!                        duration of 0 is unknown)
//!
//! From the service to the screen, besides 2 to 5 as they came:
//!
//!   [6] LINK + u8        1 while the service has its companion, 0 not
//!
//! From the screen to the service, and on to the companion:
//!
//!   [8] CONTROL + u8     1 play/pause, 2 next, 3 previous; and from the
//!                        service alone, 4 pause and 5 play (the session's
//!                        `pause` and `resume`, for a player app)
//!
//! A frame this end doesn't know is ignored, so types can be added.

pub const AUDIO: u8 = 0;
pub const END: u8 = 1;
pub const NAME: u8 = 2;
pub const TRACK: u8 = 3;
pub const COVER: u8 = 4;
pub const STATE: u8 = 5;
pub const LINK: u8 = 6;
pub const CONTROL: u8 = 8;

/// The cover art's largest side, in pixels: the screen's cover box.
pub const COVER_SIDE: u16 = 128;

/// The longest text field a TRACK carries, in bytes.
const FIELD: usize = 255;

/// What is playing, as the sender describes it. Any field may be empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The sender: "Matt's iPhone".
    pub source: String,
}

impl Track {
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.artist.is_empty() && self.album.is_empty()
    }
}

/// Cover art, ready for the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    pub width: u16,
    pub height: u16,
    /// RGB565, `width` by `height`, a row at a time.
    pub pixels: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Play {
    #[default]
    Stopped = 0,
    Playing = 1,
    Paused = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub play: Play,
    /// The sender takes remote control (play/pause, next, previous).
    pub controls: bool,
    pub elapsed_ms: u32,
    /// 0 if not known.
    pub duration_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    PlayPause = 1,
    Next = 2,
    Previous = 3,
    Pause = 4,
    Play = 5,
}

/// A frame other than audio, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Name(String),
    Track(Track),
    Cover(Cover),
    State(State),
    Link(bool),
    Control(Control),
}

/// `s` cut to at most `max` bytes, at a character boundary.
fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl Frame {
    /// The frame as bytes, type first.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Name(n) => [&[NAME][..], cut(n, FIELD).as_bytes()].concat(),
            Frame::Track(t) => {
                let mut f = vec![TRACK];
                for field in [&t.title, &t.artist, &t.album, &t.source] {
                    let field = cut(field, FIELD);
                    f.push(field.len() as u8);
                    f.extend_from_slice(field.as_bytes());
                }
                f
            }
            Frame::Cover(c) => {
                let mut f = Vec::with_capacity(5 + c.pixels.len() * 2);
                f.push(COVER);
                f.extend_from_slice(&c.width.to_be_bytes());
                f.extend_from_slice(&c.height.to_be_bytes());
                for p in &c.pixels {
                    f.extend_from_slice(&p.to_le_bytes());
                }
                f
            }
            Frame::State(s) => {
                let mut f = vec![STATE, s.play as u8, s.controls as u8];
                f.extend_from_slice(&s.elapsed_ms.to_be_bytes());
                f.extend_from_slice(&s.duration_ms.to_be_bytes());
                f
            }
            Frame::Link(up) => vec![LINK, *up as u8],
            Frame::Control(c) => vec![CONTROL, *c as u8],
        }
    }

    /// The frame `f` holds, or None for audio, a type this end doesn't
    /// know, or a body that doesn't add up.
    pub fn parse(f: &[u8]) -> Option<Frame> {
        let (&ty, body) = f.split_first()?;
        Some(match ty {
            NAME => Frame::Name(String::from_utf8_lossy(body).into_owned()),
            TRACK => {
                let mut fields = Vec::with_capacity(4);
                let mut rest = body;
                for _ in 0..4 {
                    let (&n, r) = rest.split_first()?;
                    let (field, r) = r.split_at_checked(n as usize)?;
                    fields.push(String::from_utf8_lossy(field).into_owned());
                    rest = r;
                }
                let [title, artist, album, source] = <[String; 4]>::try_from(fields).ok()?;
                Frame::Track(Track { title, artist, album, source })
            }
            COVER => {
                let (head, px) = body.split_at_checked(4)?;
                let width = u16::from_be_bytes([head[0], head[1]]);
                let height = u16::from_be_bytes([head[2], head[3]]);
                if width > COVER_SIDE || height > COVER_SIDE || px.len() != width as usize * height as usize * 2 {
                    return None;
                }
                let pixels = px.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect();
                Frame::Cover(Cover { width, height, pixels })
            }
            STATE => {
                let b: &[u8; 10] = body.try_into().ok()?;
                let play = match b[0] {
                    1 => Play::Playing,
                    2 => Play::Paused,
                    _ => Play::Stopped,
                };
                Frame::State(State {
                    play,
                    controls: b[1] & 1 != 0,
                    elapsed_ms: u32::from_be_bytes([b[2], b[3], b[4], b[5]]),
                    duration_ms: u32::from_be_bytes([b[6], b[7], b[8], b[9]]),
                })
            }
            LINK => Frame::Link(*body.first()? != 0),
            CONTROL => Frame::Control(match body {
                [1] => Control::PlayPause,
                [2] => Control::Next,
                [3] => Control::Previous,
                [4] => Control::Pause,
                [5] => Control::Play,
                _ => return None,
            }),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neochumby_proto::frame::MAX_FRAME;

    #[test]
    fn frames_go_and_come_back() {
        let frames = [
            Frame::Name("Kitchen Chumby".into()),
            Frame::Track(Track {
                title: "Ünïcode Title".into(),
                artist: "An Artist".into(),
                album: String::new(),
                source: "Someone's iPhone".into(),
            }),
            Frame::Cover(Cover { width: 2, height: 3, pixels: vec![0xf800, 0x07e0, 0x001f, 0, 0xffff, 0x1234] }),
            Frame::Cover(Cover::default()),
            Frame::State(State { play: Play::Paused, controls: true, elapsed_ms: 61_500, duration_ms: 245_000 }),
            Frame::Link(true),
            Frame::Control(Control::Previous),
            Frame::Control(Control::Play),
        ];
        for f in frames {
            assert_eq!(Frame::parse(&f.encode()), Some(f));
        }
    }

    #[test]
    fn long_fields_are_cut_at_a_character() {
        let title = "é".repeat(200); // 400 bytes
        let f = Frame::Track(Track { title, ..Track::default() }).encode();
        let Some(Frame::Track(t)) = Frame::parse(&f) else { panic!("not a track") };
        assert_eq!(t.title, "é".repeat(127));
    }

    #[test]
    fn what_does_not_add_up_is_ignored() {
        assert_eq!(Frame::parse(&[]), None);
        assert_eq!(Frame::parse(&[AUDIO, 1, 2]), None);
        assert_eq!(Frame::parse(&[99]), None);
        assert_eq!(Frame::parse(&[TRACK, 5, b'a']), None);
        assert_eq!(Frame::parse(&[STATE, 1, 0, 0]), None);
        assert_eq!(Frame::parse(&[CONTROL, 9]), None);
        // A cover bigger than the box, or short of pixels.
        let big = Frame::Cover(Cover { width: 129, height: 1, pixels: vec![0; 129] }).encode();
        assert_eq!(Frame::parse(&big), None);
        let short = Frame::Cover(Cover { width: 2, height: 2, pixels: vec![0; 3] }).encode();
        assert_eq!(Frame::parse(&short), None);
    }

    #[test]
    fn the_largest_cover_fits_a_frame() {
        let side = COVER_SIDE as usize;
        let f = Frame::Cover(Cover { width: COVER_SIDE, height: COVER_SIDE, pixels: vec![0; side * side] }).encode();
        assert!(f.len() <= MAX_FRAME);
    }
}
