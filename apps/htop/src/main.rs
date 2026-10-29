//! htop on the Chumby, on neochumby's terminal, with the touchscreen as its
//! mouse: a tap on a process selects it, on a column's heading sorts by it,
//! on the function keys along the bottom presses them, F10 quitting. htop
//! (C, GPL-2.0-or-later, third_party/htop) is linked in, its main() as
//! htop_main (build.rs), with neochumby-ncurses' curses, and the binary runs
//! itself on the terminal:
//!
//!   neochumby-htop               on the screen
//!   neochumby-htop htop [ARG..]  htop itself, on this terminal

// FFI to htop's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses, which htop's objects need.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn htop_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn htop(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("htop".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { htop_main(args.len() as c_int, argv.as_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-htop");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("htop") {
        htop(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-htop: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    // UTF-8, for htop's tree and meters in line drawing (musl's C locale is
    // ASCII).
    cmd.arg("htop").env("LANG", "C.UTF-8");
    match neochumby::term::run(cmd) {
        Ok(s) if !s.success() => eprintln!("neochumby-htop: htop: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-htop: {e}");
            std::process::exit(1);
        }
    }
}
