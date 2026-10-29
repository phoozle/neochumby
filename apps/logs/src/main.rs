//! neochumby-logs, the app: the Chumby's logs, and the host's through the
//! companion (companion.rs), on the screen (see lib.rs).

fn main() {
    #[cfg(target_arch = "arm")]
    {
        neochumby::debug::install("neochumby-logs");
        std::process::exit(neochumby_logs::device::run());
    }
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!("neochumby-logs: needs a Chumby");
        std::process::exit(2);
    }
}
