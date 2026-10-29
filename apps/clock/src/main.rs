//! tty-clock on the Chumby, on neochumby's terminal: a big digital clock,
//! with seconds, in the middle of the screen, until a tap, which types `q`.
//! tty-clock (C, BSD-3-Clause, third_party/tty-clock) is linked in, its
//! main() as clock_main (build.rs), with neochumby-ncurses' curses, and the
//! binary runs itself on the terminal:
//!
//!   neochumby-clock                on the screen
//!   neochumby-clock clock [ARG..]  tty-clock itself, on this terminal

// FFI to tty-clock's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses, which tty-clock's object needs.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn clock_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn clock(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("tty-clock".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { clock_main(args.len() as c_int, argv.as_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-clock");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("clock") {
        clock(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-clock: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    // -c: centred; -s: with seconds; -b: bold; -C 4: blue.
    cmd.args(["clock", "-c", "-s", "-b", "-C", "4"]).env("LANG", "C.UTF-8");
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-clock: tty-clock: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-clock: {e}");
            std::process::exit(1);
        }
    }
}
