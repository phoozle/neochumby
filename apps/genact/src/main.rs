//! genact on the Chumby: fake compiling, deploying and mining, on
//! neochumby's terminal, until the screen is touched. genact (Rust, MIT) is
//! a dependency, its library run on a current-thread runtime, and the binary
//! runs itself on the terminal:
//!
//!   neochumby-genact                 on the screen
//!   neochumby-genact genact [ARG..]  genact itself, on this terminal

use std::io::Read;
use std::process::Command;

use clap::Parser;
use genact::args::AppConfig;
use genact::modules::ALL_MODULES;

fn genact(args: impl Iterator<Item = String>) -> ! {
    // A tap types a newline (below): the terminal is in line mode, and has
    // no controlling terminal to turn ^C into a signal. Any input quits.
    std::thread::spawn(|| {
        let mut b = [0u8; 1];
        let _ = std::io::stdin().lock().read(&mut b);
        genact::exit_handler()
    });
    let mut appconfig = AppConfig::parse_from(std::iter::once("genact".to_string()).chain(args));
    if appconfig.modules.is_empty() {
        appconfig.modules = ALL_MODULES.keys().map(|m| m.to_string()).collect();
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
        eprintln!("neochumby-genact: no runtime");
        std::process::exit(1);
    };
    rt.block_on(async {
        *genact::SPEED_FACTOR.lock().await = appconfig.speed_factor;
        genact::INSTANT_PRINT_LINES.store(appconfig.instant_print_lines, std::sync::atomic::Ordering::SeqCst);
        genact::run(appconfig).await
    });
    std::process::exit(0)
}

fn main() {
    neochumby::debug::install("neochumby-genact");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("genact") {
        genact(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-genact: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    cmd.arg("genact");
    match neochumby::term::run_tapping(cmd, b"\n") {
        Ok(s) if !s.success() => eprintln!("neochumby-genact: genact: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-genact: {e}");
            std::process::exit(1);
        }
    }
}
