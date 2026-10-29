//! neochumby-classic: the 2008 Chumby's look, as a neochumby start app
//! (`start=classic`). A feeder on a server renders every pixel (the
//! weather, news, stocks, the aurora screensaver, the Home, Music and
//! Settings screens) and this shows them: see engine.rs for what it does
//! and manifest.rs for what it reads. It is the Rust port of wxshow.c, and
//! reads the same pictures and manifest.
//!
//! Two programs: the app (main.rs) and its service (bin/service.rs, svc.rs),
//! which keeps the backlight and the music going while another app is in
//! front. They share the settings in /psp (state.rs).

pub mod cache;
#[cfg(target_arch = "arm")]
pub mod device;
pub mod draw;
pub mod engine;
pub mod host;
pub mod loader;
pub mod logo;
pub mod manifest;
pub mod plat;
pub mod scan;
pub mod session;
pub mod state;
pub mod svc;
pub mod values;

/// Where the pictures are when neither the command line nor the
/// environment says.
pub const DEFAULT_DIR: &str = "/tmp/classic";

/// The picture directory: `arg`, else $NEOCHUMBY_CLASSIC_DIR, else
/// [`DEFAULT_DIR`].
pub fn picture_dir(arg: Option<&str>) -> std::path::PathBuf {
    arg.map(Into::into)
        .or_else(|| std::env::var_os("NEOCHUMBY_CLASSIC_DIR").map(Into::into))
        .unwrap_or_else(|| DEFAULT_DIR.into())
}
