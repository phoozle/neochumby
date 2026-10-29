//! chumby-emu: builds the images the Chumby Classic emulator (QEMU's
//! chumby-classic machine; see README.md) boots from: a settings partition
//! and a whole NAND chip.

mod image;

use std::process::ExitCode;

const USAGE: &str = "\
usage: chumby-emu mkpsp DIR OUT        JFFS2 image of a settings directory
       chumby-emu mknand BACKUP PSP OUT
                     a full NAND image: the partitions from a backup's
                     nanddumps, PSP (a JFFS2 image) as the settings
                     partition, and the Cache partition erased
";

fn mkpsp(dir: &str, out: &str) -> Result<(), String> {
    let psp = image::PARTITIONS[1].2;
    let img = image::mkjffs2(std::path::Path::new(dir), psp)?;
    std::fs::write(out, img).map_err(|e| format!("{out}: {e}"))
}

fn mknand(backup: &str, psp: &str, out: &str) -> Result<(), String> {
    let read = |p: String| std::fs::read(&p).map_err(|e| format!("{p}: {e}"));
    let mut parts = Vec::new();
    for (name, _, _) in image::PARTITIONS {
        parts.push(match name {
            "mtd1-PSP" => image::Source::Data(read(psp.to_string())?),
            "mtd6-Cache" => image::Source::Erased,
            _ => image::Source::Dump(read(format!("{backup}/{name}.nanddump"))?),
        });
    }
    let chip = image::mknand(parts)?;
    std::fs::write(out, chip).map_err(|e| format!("{out}: {e}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("mkpsp") if args.len() == 3 => mkpsp(&args[1], &args[2]),
        Some("mknand") if args.len() == 4 => mknand(&args[1], &args[2], &args[3]),
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
