//! neochumby-logs: logs on the Chumby's screen, scrolled by touch.
//!
//! The Chumby's own come first: the session's (/tmp/neochumby.log, which
//! the boot hook and every app write to), the kernel's (dmesg), and any
//! other log in /tmp or /var/log. With a paired neochumbyd, the host's
//! follow, as its companion (companion.rs) is configured to serve them
//! ([`config`]). Each is followed while it is shown, as `tail -f` would.
//!
//! The companion's frames:
//!
//!   companion → Chumby
//!     [0] + names, '\n' between   the host's logs, as it starts
//!     [1, i]                      log i's text starts here: clear the screen
//!     [2] + text                  more of it
//!   Chumby → companion
//!     [0, i]                      show log i (and stop the last)

pub mod config;
pub mod tail;
pub mod text;
pub mod view;

#[cfg(target_arch = "arm")]
pub mod device;

/// The companion's frames, as above.
pub mod frame {
    pub const SOURCES: u8 = 0;
    pub const START: u8 = 1;
    pub const TEXT: u8 = 2;
    pub const SHOW: u8 = 0;
}

/// How much of a log is read as it is opened: its end, this many bytes.
pub const TAIL: u64 = 64 * 1024;
