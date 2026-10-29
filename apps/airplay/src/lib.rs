//! AirPlay's pieces that need no Chumby, so that they build and are tested
//! on the host as well: the frames between its halves (`frames`), the
//! service's memory of the track (`hub`), and the screen's model
//! (`nowplaying`) and drawing (`ui`). The service and the screen talk over
//! neochumby::service. The binaries are thin: the service (main.rs), the
//! screen (screen.rs) and the companion (companion/).

pub mod frames;
pub mod hub;
pub mod nowplaying;
pub mod ui;
