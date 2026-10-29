//! The app's side of the session's request pipe (proposal 0002), behind a
//! small trait so the engine and its tests don't need the device library:
//! on the Chumby it is `neochumby::session::Pipe` (device.rs).
//!
//! classic is a start app. It opens other apps by asking the session
//! (`open NAME`) and then exiting; Home's CHUMBY asks for `chumby-mode`.
//! The top button belongs to the session: while classic is in front, a
//! press arrives as a `button` line, and classic flips between the
//! slideshow and Home as the button always did. If the line can't be
//! delivered, the session starts classic again with NEOCHUMBY_REASON=button,
//! and it opens on Home.

/// What the session told the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// The top button was pressed.
    Button,
}

/// What the app asks the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Run this app next (then the app exits).
    Open(String),
    /// Hand the Chumby back to its own software (then the app exits).
    ChumbyMode,
}

pub trait Session {
    /// What has arrived since the last look, without waiting.
    fn heard(&mut self) -> Vec<Heard>;
    /// Sends a request; false if it couldn't be.
    fn ask(&mut self, a: &Ask) -> bool;
}

/// An app's name as the session takes it: apps/<name>/ in the tree,
/// `[a-z0-9_-]{1,32}`.
pub fn valid_app(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_names() {
        for ok in ["doom", "quake", "a", "web_admin-2", &"x".repeat(32)] {
            assert!(super::valid_app(ok), "{ok}");
        }
        for bad in ["", "Doom", "../doom", "doom now", "doom\n", &"x".repeat(33)] {
            assert!(!super::valid_app(bad), "{bad:?}");
        }
    }
}
