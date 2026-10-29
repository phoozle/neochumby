//! Building the emulator's NAND image: a JFFS2 image of a settings (PSP)
//! directory, and the whole 64 MiB chip assembled from partition dumps.

use std::fs;
use std::path::Path;

/// The NAND chip: 512-byte pages with 16 spare bytes, 131072 pages. An
/// image is a record of data then spare per page, as nanddump writes.
const PAGE: usize = 512;
const SPARE: usize = 16;
const RECORD: usize = PAGE + SPARE;
const PAGES: usize = 131_072;

/// The Chumby's MTD partitions: name in the backup, offset, size.
pub const PARTITIONS: [(&str, usize, usize); 8] = [
    ("mtd0-Boot_Loader", 0x0000000, 0x0100000),
    ("mtd1-PSP", 0x0100000, 0x0200000),
    ("mtd2-Kernel2", 0x0300000, 0x0280000),
    ("mtd3-Root_Disk2", 0x0580000, 0x0800000),
    ("mtd4-Kernel1", 0x0d80000, 0x0300000),
    ("mtd5-Root_Disk1", 0x1080000, 0x1600000),
    ("mtd6-Cache", 0x2680000, 0x1800000),
    ("mtd7-MSP", 0x3f80000, 0x0080000),
];

const ERASE_BLOCK: usize = 0x4000;

/// JFFS2's CRC: CRC-32 (reflected, polynomial 0xEDB88320) from 0, with
/// no final inversion.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    crc
}

struct Jffs2 {
    out: Vec<u8>,
    next_ino: u32,
    /// A fixed time for every node, so images are reproducible.
    time: u32,
}

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

impl Jffs2 {
    /// Appends a node, moving to the next erase block if it would cross
    /// one.
    fn push(&mut self, node: Vec<u8>) {
        let len = (node.len() + 3) & !3;
        let room = ERASE_BLOCK - self.out.len() % ERASE_BLOCK;
        if len > room {
            self.out.resize(self.out.len() + room, 0xff);
        }
        self.out.extend_from_slice(&node);
        self.out.resize(self.out.len() + (len - node.len()), 0);
    }

    fn header(node: &mut Vec<u8>, kind: u16, total: usize) {
        node.extend_from_slice(&0x1985u16.to_le_bytes());
        node.extend_from_slice(&kind.to_le_bytes());
        node.extend_from_slice(&(total as u32).to_le_bytes());
        let c = crc32(&node[0..8]);
        node.extend_from_slice(&c.to_le_bytes());
    }

    fn dirent(&mut self, pino: u32, version: u32, ino: u32, name: &[u8], dtype: u8) {
        let mut n = Vec::new();
        Self::header(&mut n, 0xe001, 40 + name.len());
        for v in [pino, version, ino, self.time] {
            n.extend_from_slice(&v.to_le_bytes());
        }
        n.extend_from_slice(&[name.len() as u8, dtype, 0, 0]);
        let c = crc32(&n[0..32]);
        n.extend_from_slice(&c.to_le_bytes());
        n.extend_from_slice(&crc32(name).to_le_bytes());
        n.extend_from_slice(name);
        self.push(n);
    }

    fn inode(&mut self, ino: u32, version: u32, mode: u32, isize: u32, offset: u32, data: &[u8]) {
        let mut n = Vec::new();
        Self::header(&mut n, 0xe002, 68 + data.len());
        for v in [ino, version, mode] {
            n.extend_from_slice(&v.to_le_bytes());
        }
        n.extend_from_slice(&[0, 0, 0, 0]); // uid, gid: root
        for v in [isize, self.time, self.time, self.time, offset, data.len() as u32, data.len() as u32] {
            n.extend_from_slice(&v.to_le_bytes());
        }
        n.extend_from_slice(&[0, 0, 0, 0]); // no compression, flags 0
        n.extend_from_slice(&crc32(data).to_le_bytes());
        let c = crc32(&n[0..60]);
        n.extend_from_slice(&c.to_le_bytes());
        n.extend_from_slice(data);
        self.push(n);
    }

    fn add_dir(&mut self, dir: &Path, ino: u32) -> std::io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for (version, e) in (1..).zip(entries) {
            let name = e.file_name();
            let name = name.as_encoded_bytes();
            let meta = fs::symlink_metadata(e.path())?;
            let perm = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o7777
            };
            let child = self.next_ino;
            self.next_ino += 1;
            if meta.is_dir() {
                self.inode(child, 1, S_IFDIR | perm, 0, 0, &[]);
                self.dirent(ino, version, child, name, DT_DIR);
                self.add_dir(&e.path(), child)?;
            } else if meta.file_type().is_symlink() {
                let target = fs::read_link(e.path())?;
                let t = target.as_os_str().as_encoded_bytes();
                self.inode(child, 1, S_IFLNK | 0o777, t.len() as u32, 0, t);
                self.dirent(ino, version, child, name, DT_LNK);
            } else {
                let data = fs::read(e.path())?;
                if data.is_empty() {
                    self.inode(child, 1, S_IFREG | perm, 0, 0, &[]);
                }
                for (i, chunk) in data.chunks(4096).enumerate() {
                    self.inode(child, 1 + i as u32, S_IFREG | perm, data.len() as u32, (i * 4096) as u32, chunk);
                }
                self.dirent(ino, version, child, name, DT_REG);
            }
        }
        Ok(())
    }
}

/// A JFFS2 image (little-endian, 16 KiB erase blocks, uncompressed, no
/// in-band clean markers as suits NAND) of `dir`, padded with erased flash
/// to `size`.
pub fn mkjffs2(dir: &Path, size: usize) -> Result<Vec<u8>, String> {
    let mut j = Jffs2 { out: Vec::new(), next_ino: 2, time: 1_230_768_000 };
    j.add_dir(dir, 1).map_err(|e| format!("{}: {e}", dir.display()))?;
    if j.out.len() > size {
        return Err(format!("{} doesn't fit in {size} bytes", dir.display()));
    }
    j.out.resize(size, 0xff);
    Ok(j.out)
}

/// Interleaves plain data with spare areas into 528-byte records.
fn with_spare(data: &[u8], spare: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / PAGE * RECORD);
    for page in data.chunks(PAGE) {
        out.extend_from_slice(page);
        out.resize(out.len() + SPARE, spare);
    }
    out
}

/// Where a partition's contents come from.
pub enum Source {
    /// A nanddump file (data and spare), used as is.
    Dump(Vec<u8>),
    /// Plain data, written with blank spare areas.
    Data(Vec<u8>),
    Erased,
}

/// Assembles the whole chip from one source per partition.
pub fn mknand(parts: Vec<Source>) -> Result<Vec<u8>, String> {
    let mut chip = vec![0xffu8; PAGES * RECORD];
    for ((name, off, size), src) in PARTITIONS.iter().zip(parts) {
        let records = match src {
            Source::Dump(d) => d,
            Source::Data(d) => with_spare(&d, 0xff),
            Source::Erased => continue,
        };
        let want = size / PAGE * RECORD;
        if records.len() != want {
            return Err(format!("{name}: {} bytes, expected {want}", records.len()));
        }
        let at = off / PAGE * RECORD;
        chip[at..at + want].copy_from_slice(&records);
    }
    Ok(chip)
}
