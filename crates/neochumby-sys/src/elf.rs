//! What an app may run: a static ARM ELF whose segments stay above the
//! first user page.
//!
//! The ARM 2.6.16 `mmap` path rejects `MAP_FIXED` below `PAGE_SIZE`
//! (`FIRST_USER_ADDRESS` in `arch/arm/kernel/sys_arm.c`), but the ELF loader
//! does not: a `PT_LOAD` at virtual address 0 is mapped at 0. That is the
//! first half of a NULL-pointer kernel exploit (CVE-2009-2698, among
//! others). The session can refuse such a file before it execs it, and can
//! refuse one hidden among the app's own executables.
//!
//! This is not a substitute for a kernel fix. It closes the page-zero
//! route, not every local-root bug in a 2006 kernel.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The first page above the ARM kernel's `FIRST_USER_ADDRESS`.
const PAGE_SIZE: u32 = 0x1000;
/// The ARM user address limit (`TASK_SIZE` in `asm-arm/memory.h`).
const TASK_SIZE: u32 = 0xbf00_0000;

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_ARM: u16 = 40;

/// The longest program header table accepted, so a damaged file cannot make
/// the session allocate an arbitrary amount.
const MAX_PHDRS: u16 = 256;
const MAX_PHENT: u16 = 512;
/// How deep [`check_app_dir`] walks: app binaries live at the top, data in
/// subdirectories.
const MAX_DEPTH: usize = 8;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Whether `path` is a static ARM ELF whose `PT_LOAD` segments do not map
/// page 0. Reads only the ELF header and program headers.
pub fn check(path: &Path) -> io::Result<()> {
    let mut f = fs::File::open(path).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    let len = f.metadata()?.len();
    let mut eh = [0u8; 52];
    f.read_exact(&mut eh).map_err(|e| invalid(format!("{}: {e}", path.display())))?;

    if &eh[0..4] != b"\x7fELF" {
        return Err(invalid(format!("{}: not an ELF", path.display())));
    }
    if eh[4] != 1 || eh[5] != 1 || eh[6] != 1 {
        return Err(invalid(format!("{}: not a 32-bit little-endian ELF", path.display())));
    }
    let e_type = u16_at(&eh, 16);
    let e_machine = u16_at(&eh, 18);
    let e_version = u32_at(&eh, 20);
    let e_phoff = u32_at(&eh, 28);
    let e_ehsize = u16_at(&eh, 40);
    let e_phentsize = u16_at(&eh, 42);
    let e_phnum = u16_at(&eh, 44);

    if e_version != 1 {
        return Err(invalid(format!("{}: unknown ELF version {e_version}", path.display())));
    }
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(invalid(format!("{}: not an executable ELF (type {e_type})", path.display())));
    }
    if e_machine != EM_ARM {
        return Err(invalid(format!("{}: not an ARM ELF (machine {e_machine})", path.display())));
    }
    if e_ehsize < 52 || e_phentsize < 32 || e_phnum == 0 {
        return Err(invalid(format!("{}: no usable program headers", path.display())));
    }
    if e_phnum > MAX_PHDRS || e_phentsize > MAX_PHENT {
        return Err(invalid(format!("{}: too many program headers", path.display())));
    }
    let table_len = (e_phnum as u64) * (e_phentsize as u64);
    if e_phoff as u64 + table_len > len {
        return Err(invalid(format!("{}: program headers past the end", path.display())));
    }

    let mut table = vec![0u8; table_len as usize];
    f.seek(SeekFrom::Start(e_phoff as u64))?;
    f.read_exact(&mut table)?;
    for i in 0..e_phnum as usize {
        let p = &table[i * e_phentsize as usize..];
        let p_type = u32_at(p, 0);
        let p_offset = u32_at(p, 4);
        let p_vaddr = u32_at(p, 8);
        let p_filesz = u32_at(p, 16);
        let p_memsz = u32_at(p, 20);
        if p_type == PT_INTERP {
            return Err(invalid(format!("{}: needs an interpreter, which the jail doesn't have", path.display())));
        }
        if p_type != PT_LOAD {
            continue;
        }
        if p_memsz != 0 && p_vaddr < PAGE_SIZE {
            return Err(invalid(format!("{}: a PT_LOAD maps page 0 (vaddr 0x{p_vaddr:x})", path.display())));
        }
        let Some(end) = p_vaddr.checked_add(p_memsz) else {
            return Err(invalid(format!("{}: a PT_LOAD wraps the address space", path.display())));
        };
        if end > TASK_SIZE {
            return Err(invalid(format!("{}: a PT_LOAD ends above the user address space", path.display())));
        }
        if p_offset as u64 + p_filesz as u64 > len {
            return Err(invalid(format!("{}: a PT_LOAD is past the end of the file", path.display())));
        }
    }
    Ok(())
}

/// Whether `path` starts with a 32-bit little-endian ARM ELF header. A
/// non-ARM file in an app's directory is not executable on the Chumby, so
/// the directory scan ignores it (the companion half of an app is x86_64).
fn is_arm_elf(path: &Path) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut h = [0u8; 20];
    f.read_exact(&mut h).is_ok() && &h[0..4] == b"\x7fELF" && h[4] == 1 && h[5] == 1 && u16_at(&h, 18) == EM_ARM
}

/// Checks every executable regular file under `dir`, so a helper ELF cannot
/// hide beside the app's `exec`. Symlinks are checked by their target; a
/// symlink whose target is not an executable file is ignored. The tree is
/// read-only to the app, so one scan when it starts is enough.
pub fn check_app_dir(dir: &Path) -> io::Result<()> {
    check_dir(dir, 0)
}

fn check_dir(dir: &Path, depth: usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_dir() {
            check_dir(&path, depth + 1)?;
            continue;
        }
        let target = if meta.file_type().is_symlink() {
            match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            }
        } else {
            meta
        };
        // A server-side companion (x86_64) or a data file marked
        // executable cannot run on the Chumby. Only an ARM ELF is a
        // candidate; the app's own exec/service are checked strictly
        // before this scan.
        if target.is_file() && target.permissions().mode() & 0o111 != 0 && is_arm_elf(&path) {
            check(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A minimal ELF with one 32-byte program header per tuple:
    /// (type, offset, vaddr, filesz, memsz).
    fn elf(entries: &[(u32, u32, u32, u32, u32)]) -> Vec<u8> {
        let mut b = vec![0u8; 0x2000];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 1;
        b[5] = 1;
        b[6] = 1;
        b[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        b[18..20].copy_from_slice(&EM_ARM.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[28..32].copy_from_slice(&52u32.to_le_bytes());
        b[40..42].copy_from_slice(&52u16.to_le_bytes());
        b[42..44].copy_from_slice(&32u16.to_le_bytes());
        b[44..46].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for (i, &(kind, off, vaddr, filesz, memsz)) in entries.iter().enumerate() {
            let at = 52 + i * 32;
            b[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            b[at + 4..at + 8].copy_from_slice(&off.to_le_bytes());
            b[at + 8..at + 12].copy_from_slice(&vaddr.to_le_bytes());
            b[at + 16..at + 20].copy_from_slice(&filesz.to_le_bytes());
            b[at + 20..at + 24].copy_from_slice(&memsz.to_le_bytes());
        }
        b
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("neochumby-elf-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, bytes: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn ordinary_static_elf_is_allowed() {
        let d = temp("ok");
        let p = d.join("app");
        write(&p, &elf(&[(PT_LOAD, 0x1000, 0x10000, 0x100, 0x100)]));
        check(&p).unwrap();
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_load_at_page_zero_is_refused() {
        let d = temp("page0");
        let p = d.join("app");
        write(&p, &elf(&[(PT_LOAD, 0x1000, 0, 0x1000, 0x1000)]));
        let e = check(&p).unwrap_err();
        assert!(e.to_string().contains("page 0"), "{e}");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn an_interpreter_is_refused() {
        let d = temp("interp");
        let p = d.join("app");
        write(&p, &elf(&[(PT_INTERP, 0x1000, 0, 4, 4), (PT_LOAD, 0x1000, 0x10000, 0x100, 0x100)]));
        let e = check(&p).unwrap_err();
        assert!(e.to_string().contains("interpreter"), "{e}");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn another_machine_and_a_short_file_are_refused() {
        let d = temp("bad");
        let p = d.join("machine");
        let mut b = elf(&[(PT_LOAD, 0x1000, 0x10000, 0x100, 0x100)]);
        b[18..20].copy_from_slice(&3u16.to_le_bytes());
        write(&p, &b);
        assert!(check(&p).is_err());
        write(&p, b"\x7fELF");
        assert!(check(&p).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_non_arm_companion_is_ignored() {
        let d = temp("companion");
        let p = d.join("companion");
        let mut b = elf(&[(PT_LOAD, 0x1000, 0, 0x1000, 0x1000)]);
        b[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        write(&p, &b);
        let mut mode = fs::metadata(&p).unwrap().permissions();
        mode.set_mode(0o755);
        fs::set_permissions(&p, mode).unwrap();
        check_app_dir(&d).unwrap();
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_hidden_helper_is_found() {
        let d = temp("helpers");
        let good = d.join("good");
        write(&good, &elf(&[(PT_LOAD, 0x1000, 0x10000, 0x100, 0x100)]));
        check_app_dir(&d).unwrap();

        let bad = d.join("bad");
        write(&bad, &elf(&[(PT_LOAD, 0x1000, 0, 0x1000, 0x1000)]));
        let mut mode = fs::metadata(&bad).unwrap().permissions();
        mode.set_mode(0o755);
        fs::set_permissions(&bad, mode).unwrap();
        assert!(check_app_dir(&d).is_err());

        fs::remove_file(&bad).unwrap();
        check_app_dir(&d).unwrap();
        fs::remove_dir_all(d).unwrap();
    }
}
