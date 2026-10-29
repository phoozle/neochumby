//! neochumby-classic, the app:
//!
//! ```text
//! neochumby-classic [DIR]                  on the Chumby, run by the session
//! neochumby-classic render DIR PREFIX ...  anywhere: frames to files (host.rs)
//! ```
//!
//! The pictures are in DIR, else $NEOCHUMBY_CLASSIC_DIR, else /tmp/classic.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("render") {
        std::process::exit(neochumby_classic::host::render(&args[2..]));
    }
    let dir = neochumby_classic::picture_dir(args.get(1).map(String::as_str));
    #[cfg(target_arch = "arm")]
    std::process::exit(neochumby_classic::device::run(dir));
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!(
            "neochumby-classic: {} needs a Chumby; on this machine, use: neochumby-classic render DIR PREFIX",
            dir.display()
        );
        std::process::exit(2);
    }
}
