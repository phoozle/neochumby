//! Now playing's pieces that need no Chumby, so that they build and are
//! tested on the host as well: the frames between its halves (`frames`), the
//! service's memory of the track (`hub`), the local socket between the
//! service and the screen (`link`), and the screen's model (`nowplaying`)
//! and drawing (`ui`). The binaries are thin: the service (main.rs), the
//! screen (screen.rs) and the companion (companion/).

pub mod frames;
pub mod hub;
pub mod link;
pub mod nowplaying;
pub mod ui;
