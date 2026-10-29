//! pipes.sh on the Chumby, by pipes-rs (Rust, GPL-3.0,
//! third_party/pipes-rs): pipes growing across the screen, on neochumby's
//! terminal, until the screen is touched. pipes-rs is a binary, not a
//! library, so its main.rs is compiled in as a module, unchanged, and the
//! binary runs itself on the terminal:
//!
//!   neochumby-pipes                          on the screen
//!   NEOCHUMBY_PIPES=1 neochumby-pipes [ARG..]  pipes-rs itself, on this terminal

use std::process::Command;

// pipes-rs's modules, where its `crate::` paths find them.
#[path = "../third_party/pipes-rs/src/pipe.rs"]
#[allow(clippy::all)]
#[rustfmt::skip]
mod pipe;
#[path = "../third_party/pipes-rs/src/random.rs"]
#[allow(clippy::all)]
#[rustfmt::skip]
mod random;
#[path = "../third_party/pipes-rs/src/screen.rs"]
#[allow(clippy::all)]
#[rustfmt::skip]
mod screen;
#[path = "../third_party/pipes-rs/src/utils.rs"]
#[allow(clippy::all)]
#[rustfmt::skip]
mod utils;

// Its main.rs, whose own `mod` lines look in its directory. Its main is
// private to the module: run() is the way in. Its modules are compiled
// twice, and the copies in here go unused.
#[path = "../third_party/pipes-rs/src"]
#[allow(dead_code, unused_imports, clippy::all)]
#[rustfmt::skip]
mod upstream {
    include!("../third_party/pipes-rs/src/main.rs");

    pub fn run() -> anyhow::Result<()> {
        main()
    }
}

/// The screen is small: three pipes, at a rate the pty can carry, cleared
/// often enough to stay lively.
const ARGS: [&str; 6] = ["--pipes", "3", "--fps", "20", "--frame-clear", "400"];

fn main() {
    neochumby::debug::install("neochumby-pipes");
    if std::env::var_os("NEOCHUMBY_PIPES").is_some() {
        if let Err(e) = upstream::run() {
            eprintln!("neochumby-pipes: {e}");
            std::process::exit(1);
        }
        return;
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-pipes: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    cmd.args(ARGS).env("NEOCHUMBY_PIPES", "1");
    // A tap types q, which ends pipes-rs.
    match neochumby::term::run_tapping(cmd, b"q") {
        Ok(s) if !s.success() => eprintln!("neochumby-pipes: pipes-rs: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-pipes: {e}");
            std::process::exit(1);
        }
    }
}
