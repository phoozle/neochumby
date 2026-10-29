//! cmatrix on the Chumby, on neochumby's terminal: the falling green
//! characters of The Matrix, until a tap, which types `q`. cmatrix (C,
//! GPL-3.0-or-later, third_party/cmatrix) is linked in, its main() as
//! cmatrix_main (build.rs), with neochumby-ncurses' curses, and the binary
//! runs itself on the terminal:
//!
//!   neochumby-cmatrix                 on the screen
//!   neochumby-cmatrix cmatrix [ARG..] cmatrix itself, on this terminal

// FFI to cmatrix's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses, which cmatrix's object needs.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn cmatrix_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn cmatrix(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("cmatrix".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { cmatrix_main(args.len() as c_int, argv.as_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-cmatrix");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("cmatrix") {
        cmatrix(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-cmatrix: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    // -b: bold characters, brighter on the small screen.
    cmd.args(["cmatrix", "-b"]).env("LANG", "C.UTF-8");
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-cmatrix: cmatrix: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-cmatrix: {e}");
            std::process::exit(1);
        }
    }
}
