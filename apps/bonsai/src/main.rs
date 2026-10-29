//! cbonsai on the Chumby, on neochumby's terminal: a bonsai tree that grows
//! on the screen, a new one every so often, until a tap, which quits.
//! cbonsai (C, GPL-3.0, third_party/cbonsai) is linked in, its main() as
//! bonsai_main (build.rs), with neochumby-ncurses' curses, and the binary
//! runs itself on the terminal:
//!
//!   neochumby-bonsai                 on the screen
//!   neochumby-bonsai cbonsai [ARG..] cbonsai itself, on this terminal

// FFI to cbonsai's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses and panel, which cbonsai's object needs.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn bonsai_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn cbonsai(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("cbonsai".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { bonsai_main(args.len() as c_int, argv.as_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-bonsai");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("cbonsai") {
        cbonsai(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-bonsai: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    // Live, so that it grows in front of you, and a new tree every 20
    // seconds. UTF-8, for the pot and the message border (musl's C locale
    // is ASCII).
    cmd.args(["cbonsai", "-l", "-i", "-w", "20"]).env("LANG", "C.UTF-8");
    // A tap types q, which quits it.
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-bonsai: cbonsai: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-bonsai: {e}");
            std::process::exit(1);
        }
    }
}
