//! fastfetch on the Chumby: the system's details beside a Chumby, on
//! neochumby's terminal, until the screen is touched. fastfetch (C, MIT,
//! third_party/fastfetch) is linked in, its main() as fastfetch_main
//! (csrc/), and the binary runs itself on the terminal:
//!
//!   neochumby-fetch                    on the screen, from the app's directory
//!   neochumby-fetch fastfetch [ARG..]  fastfetch itself, on this terminal
//!
//! fastfetch.jsonc and chumby.txt (files/) are its configuration and logo:
//! our own drawing of the device, not Chumby's artwork (docs/legal.md).

// FFI to fastfetch's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

use neochumby::input::{Touch, Touchscreen};

unsafe extern "C" {
    fn fastfetch_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn fastfetch(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("fastfetch".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: argv is argc C strings then a null, as main() takes them, and
    // they outlive the call.
    let code = unsafe { fastfetch_main(args.len() as c_int, argv.as_ptr()) };
    // exit(3), so that C's stdio is flushed.
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-fetch");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("fastfetch") {
        fastfetch(args);
    }
    let (Some(exe), Ok(dir)) = (neochumby::apps::exe(), std::env::current_dir()) else {
        eprintln!("neochumby-fetch: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    cmd.arg("fastfetch")
        .arg("--config")
        .arg(dir.join("fastfetch.jsonc"))
        .arg("--logo-type")
        .arg("file")
        .arg("--logo")
        .arg(dir.join("chumby.txt"));
    match neochumby::term::run(cmd) {
        Ok(s) if !s.success() => eprintln!("neochumby-fetch: fastfetch: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-fetch: {e}");
            std::process::exit(1);
        }
    }
    // Its output stays until a touch ends.
    let mut touch = Touchscreen::open();
    while !touch.poll().contains(&Touch::Up) {
        neochumby::sleep_ms(50);
    }
}
