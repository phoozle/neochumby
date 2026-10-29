//! neochumby: the Chumby Classic's screen, inputs and speaker, for the
//! launcher, the apps and the services (stock 2.6.16 kernel, static musl,
//! ARM926).

pub mod apps;
pub mod audio;
pub mod companion;
pub mod conf;
pub mod debug;
pub mod fb;
pub mod input;
pub mod service;
pub mod session;
pub mod settings;
pub mod term;

/// `companion!()`: the compiling app's companion, for [`companion::open`];
/// an app whose Cargo.toml declares none does not compile.
pub use neochumby_macros::companion;

// Linked for its symbols: see neochumby-softfloat.
#[cfg(feature = "softfloat")]
use neochumby_softfloat as _;

/// Sleeps about `ms` milliseconds; use it instead of std::thread::sleep,
/// which panics when this kernel fails an interrupted nanosleep with EINVAL.
pub fn sleep_ms(ms: u32) {
    neochumby_sys::time::sleep_ms(ms);
}
