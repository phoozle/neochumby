//! Termination as a flag: for a program that has to tidy up (put a
//! device back as it found it) when it is asked to stop, instead of dying
//! mid-step.

use std::sync::atomic::{AtomicBool, Ordering};

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    TERM.store(true, Ordering::Relaxed);
}

/// From now on SIGTERM and SIGINT only set a flag, which this returns: the
/// program sees it, tidies up and exits. A blocking call they interrupt
/// returns early (no SA_RESTART), so a program sleeping in poll(2) wakes
/// to see it. Replaces any handler installed before (such as the profile
/// dump of neochumby's debug module).
pub fn catch_term() -> &'static AtomicBool {
    // SAFETY: all-zero is a valid sigaction: no flags and an empty mask.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = on_term as *const () as usize;
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: a live sigaction whose handler only stores to an atomic;
        // the old action isn't asked for.
        unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
    }
    &TERM
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Signal, kill};

    #[test]
    fn sigterm_sets_the_flag() {
        let term = catch_term();
        assert!(!term.load(Ordering::Relaxed));
        kill(std::process::id(), Signal::Term).unwrap();
        // Delivered to some thread of this process before kill returns, or
        // very soon after.
        for _ in 0..100 {
            if term.load(Ordering::Relaxed) {
                return;
            }
            crate::time::sleep_ms(10);
        }
        panic!("SIGTERM didn't set the flag");
    }
}
