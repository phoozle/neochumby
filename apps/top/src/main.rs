//! top on the Chumby, on neochumby's terminal, until the screen is tapped
//! (which types its q). procps' top (C, GPL-2.0-or-later, third_party/procps)
//! is linked in, its main() as top_main (build.rs), with neochumby-ncurses'
//! terminfo, and the binary runs itself on the terminal:
//!
//!   neochumby-top              on the screen
//!   neochumby-top top [ARG..]  top itself, on this terminal

// FFI to top's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses, which top's objects need.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn top_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn top(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("top".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { top_main(args.len() as c_int, argv.as_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-top");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("top") {
        top(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-top: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    cmd.arg("top");
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-top: top: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-top: {e}");
            std::process::exit(1);
        }
    }
}
