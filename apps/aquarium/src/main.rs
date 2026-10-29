//! asciiquarium on the Chumby, by asciiquarium-rs (Rust, GPL-2.0-or-later,
//! third_party/asciiquarium-rs): fish, a whale, a ship and the rest, on
//! neochumby's terminal, until the screen is touched. Its main.rs is
//! compiled in as a module, unchanged, and the binary runs itself on the
//! terminal:
//!
//!   neochumby-aquarium                               on the screen
//!   NEOCHUMBY_AQUARIUM=1 neochumby-aquarium [ARG..]  asciiquarium-rs itself

use std::process::Command;

// Its main.rs (build.rs). Its main is private to the module: start() is the
// way in.
#[allow(dead_code, unused_imports, clippy::all)]
mod upstream {
    include!(concat!(env!("OUT_DIR"), "/upstream.rs"));

    pub fn start() -> std::io::Result<()> {
        main()
    }
}

fn main() {
    neochumby::debug::install("neochumby-aquarium");
    if std::env::var_os("NEOCHUMBY_AQUARIUM").is_some() {
        if let Err(e) = upstream::start() {
            eprintln!("neochumby-aquarium: {e}");
            std::process::exit(1);
        }
        return;
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-aquarium: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    cmd.env("NEOCHUMBY_AQUARIUM", "1");
    // A tap types q, which ends it.
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-aquarium: asciiquarium-rs: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-aquarium: {e}");
            std::process::exit(1);
        }
    }
}
