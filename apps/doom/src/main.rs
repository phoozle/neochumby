//! Doom as a neochumby app: doomgeneric (C, GPL-2.0) on the Chumby's screen,
//! touchscreen, squeeze switch and speaker (sound effects; no music yet). The IWAD is found beside the binary
//! (freedoom1.wad, doom1.wad), or in the working directory, unless NEOCHUMBY_WAD or -iwad says
//! otherwise.

// FFI to a C engine (GPL), so allowed unsafe code, as neochumby-sys is. Its
// unsafe blocks follow the engine's contract and carry no SAFETY comments.
#![allow(unsafe_code)]
#![allow(clippy::undocumented_unsafe_blocks)]

mod platform;
mod sound;

use std::path::PathBuf;

fn iwad() -> Option<PathBuf> {
    // Jailed, without /proc, the binary can't be found: the session runs
    // it in its directory.
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())?;
    std::env::var_os("NEOCHUMBY_WAD")
        .map(PathBuf::from)
        .into_iter()
        .chain(["freedoom1.wad", "doom1.wad"].map(|w| dir.join(w)))
        .find(|p| p.exists())
}

fn main() {
    neochumby::debug::install("neochumby-doom");
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "-iwad") {
        let Some(w) = iwad() else {
            eprintln!("neochumby-doom: no IWAD found (set NEOCHUMBY_WAD)");
            std::process::exit(1);
        };
        args.splice(0..0, ["-iwad".to_string(), w.display().to_string()]);
    }
    // doomgeneric keeps its config and saves in the working directory; the
    // app's own is the read-only export.
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    let _ = std::env::set_current_dir(home);
    platform::run(&args)
}
