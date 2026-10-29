//! The app tree over NFSv3, read-only (nfsserve, one TCP port for NFS and
//! MOUNT).
//!
//! - File ids are 32 bits, from a hash of the path and inode: the Chumby's
//!   32-bit userland cannot stat larger ones. They hold across restarts,
//!   and a file replaced in the tree gets a new one, so a Chumby holding
//!   the old file's handle is told it is stale instead of reading the new
//!   file's bytes as the old's.
//! - Handles carry a fixed generation, so they survive restarts too.
//! - Read-only by refusing every write; the capability reported is
//!   read-write all the same, as only then does nfsserve grant execute in
//!   ACCESS, which the Chumby's kernel asks for before running a binary.
//! - Every file is served as root's, without setuid or setgid bits
//!   ([`attr`]); the Chumby mounts the tree `nosuid,nodev` as well.
//! - Symbolic links are never followed here: a link is served as a link,
//!   for the client to resolve, so nothing outside the tree is reachable.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use nfsserve::fs_util::metadata_to_fattr3;
use nfsserve::nfs::{fattr3, fileid3, filename3, nfs_fh3, nfspath3, nfsstat3, sattr3};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

const ROOT: fileid3 = 1;
/// In every handle: "neochumb".
const GENERATION: u64 = 0x6e65_6f63_6875_6d62;

pub struct TreeFs {
    root: PathBuf,
    /// id → path relative to the root.
    paths: RwLock<HashMap<fileid3, PathBuf>>,
}

/// FNV-1a over the path and inode, folded to 32 bits, clear of 0 (reserved)
/// and 1 (the root).
fn id_for(rel: &Path, meta: &Metadata) -> fileid3 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in rel.as_os_str().as_bytes().iter().chain(&[0]).chain(&meta.ino().to_le_bytes()) {
        h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let id = (h ^ (h >> 32)) & 0xffff_ffff;
    if id < 2 { id + 2 } else { id }
}

/// A file's attributes as served: owned by root, with no setuid or setgid
/// bit and no device number, whatever the tree holds. A file in an app's
/// directory can't hand out another uid on the Chumby, nor stand for one
/// of its devices, and no app's uid can match the tree's owner here.
fn attr(id: fileid3, meta: &Metadata) -> fattr3 {
    let mut a = metadata_to_fattr3(id, meta);
    a.mode &= !0o6000;
    a.uid = 0;
    a.gid = 0;
    a.rdev = Default::default();
    a
}

impl TreeFs {
    pub fn new(root: PathBuf) -> TreeFs {
        let paths = HashMap::from([(ROOT, PathBuf::new())]);
        TreeFs { root, paths: RwLock::new(paths) }
    }

    /// The relative path of `id`, if it is still the file it was.
    fn path(&self, id: fileid3) -> Result<PathBuf, nfsstat3> {
        if let Some(p) = self.paths.read().unwrap().get(&id) {
            return Ok(p.clone());
        }
        // A handle from before a restart: index the tree and look again.
        self.index(Path::new(""));
        self.paths.read().unwrap().get(&id).cloned().ok_or(nfsstat3::NFS3ERR_STALE)
    }

    /// Metadata of `id`'s file, which must still be the one `id` names.
    fn meta(&self, id: fileid3) -> Result<(PathBuf, Metadata), nfsstat3> {
        let rel = self.path(id)?;
        let meta = std::fs::symlink_metadata(self.root.join(&rel)).map_err(|_| nfsstat3::NFS3ERR_STALE)?;
        if id != ROOT && id_for(&rel, &meta) != id {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok((rel, meta))
    }

    fn remember(&self, rel: &Path, meta: &Metadata) -> Result<fileid3, nfsstat3> {
        let id = id_for(rel, meta);
        let mut paths = self.paths.write().unwrap();
        match paths.get(&id) {
            Some(p) if p != rel => {
                eprintln!("neochumbyd: file id collision: {} and {}", p.display(), rel.display());
                Err(nfsstat3::NFS3ERR_IO)
            }
            _ => {
                paths.insert(id, rel.to_path_buf());
                Ok(id)
            }
        }
    }

    fn index(&self, dir: &Path) {
        let Ok(rd) = std::fs::read_dir(self.root.join(dir)) else { return };
        for e in rd.flatten() {
            let rel = dir.join(e.file_name());
            if let Ok(meta) = std::fs::symlink_metadata(self.root.join(&rel)) {
                let _ = self.remember(&rel, &meta);
                if meta.is_dir() {
                    self.index(&rel);
                }
            }
        }
    }

    /// A directory's entries, sorted by name, with their ids and metadata.
    fn entries(&self, dirid: fileid3) -> Result<Vec<(fileid3, filename3, Metadata)>, nfsstat3> {
        let (rel, meta) = self.meta(dirid)?;
        if !meta.is_dir() {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let mut names: Vec<_> = std::fs::read_dir(self.root.join(&rel))
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let child = rel.join(&name);
            let Ok(meta) = std::fs::symlink_metadata(self.root.join(&child)) else { continue };
            let id = self.remember(&child, &meta)?;
            out.push((id, filename3::from(name.as_bytes()), meta));
        }
        Ok(out)
    }
}

#[async_trait]
impl NFSFileSystem for TreeFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        ROOT
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let (rel, meta) = self.meta(dirid)?;
        if !meta.is_dir() {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let name = filename.as_ref();
        match name {
            b"." => return Ok(dirid),
            b".." => {
                let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
                if parent.as_os_str().is_empty() {
                    return Ok(ROOT);
                }
                let meta = std::fs::symlink_metadata(self.root.join(&parent)).map_err(|_| nfsstat3::NFS3ERR_STALE)?;
                return self.remember(&parent, &meta);
            }
            _ if name.is_empty() || name.contains(&b'/') => return Err(nfsstat3::NFS3ERR_NOENT),
            _ => {}
        }
        let child = rel.join(std::ffi::OsStr::from_bytes(name));
        let meta = std::fs::symlink_metadata(self.root.join(&child)).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;
        self.remember(&child, &meta)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let (_, meta) = self.meta(id)?;
        Ok(attr(id, &meta))
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let (rel, meta) = self.meta(id)?;
        if !meta.is_file() {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        let mut f = File::open(self.root.join(rel)).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        f.seek(SeekFrom::Start(offset)).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let mut buf = Vec::with_capacity(count as usize);
        f.take(count as u64).read_to_end(&mut buf).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let eof = offset + buf.len() as u64 >= meta.len();
        Ok((buf, eof))
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let all = self.entries(dirid)?;
        let skip = match start_after {
            0 => 0,
            after => all.iter().position(|(id, ..)| *id == after).map(|i| i + 1).ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?,
        };
        let rest = &all[skip..];
        let take = rest.len().min(max_entries);
        Ok(ReadDirResult {
            entries: rest[..take]
                .iter()
                .map(|(id, name, meta)| DirEntry { fileid: *id, name: name.clone(), attr: attr(*id, meta) })
                .collect(),
            end: take == rest.len(),
        })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let (rel, meta) = self.meta(id)?;
        if !meta.is_symlink() {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        let target = std::fs::read_link(self.root.join(rel)).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        Ok(nfspath3::from(target.as_os_str().as_bytes()))
    }

    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        let mut data = GENERATION.to_le_bytes().to_vec();
        data.extend_from_slice(&id.to_le_bytes());
        nfs_fh3 { data }
    }

    fn fh_to_id(&self, fh: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        let (generation, id) = fh.data.split_at_checked(8).ok_or(nfsstat3::NFS3ERR_BADHANDLE)?;
        if id.len() != 8 {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        if generation != GENERATION.to_le_bytes() {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok(u64::from_le_bytes(id.try_into().expect("8 bytes")))
    }

    async fn setattr(&self, _: fileid3, _: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn write(&self, _: fileid3, _: u64, _: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(&self, _: fileid3, _: &filename3, _: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(&self, _: fileid3, _: &filename3) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(&self, _: fileid3, _: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(&self, _: fileid3, _: &filename3, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn symlink(
        &self,
        _: fileid3,
        _: &filename3,
        _: &nfspath3,
        _: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn tree() -> (PathBuf, TreeFs) {
        let root = std::env::temp_dir().join(format!("neochumbyd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps/doom")).unwrap();
        std::fs::write(root.join("neochumby"), b"launcher").unwrap();
        std::fs::write(root.join("apps/doom/manifest"), b"title=DOOM\n").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("apps/escape")).unwrap();
        (root.clone(), TreeFs::new(root))
    }

    #[tokio::test]
    async fn serves_reads_refuses_writes() {
        let (root, fs) = tree();
        let apps = fs.lookup(ROOT, &b"apps".as_slice().into()).await.unwrap();
        let doom = fs.lookup(apps, &b"doom".as_slice().into()).await.unwrap();
        let m = fs.lookup(doom, &b"manifest".as_slice().into()).await.unwrap();
        assert!(m < 1 << 32);
        assert_eq!(fs.read(m, 0, 100).await.unwrap(), (b"title=DOOM\n".to_vec(), true));
        assert_eq!(fs.read(m, 6, 4).await.unwrap(), (b"DOOM".to_vec(), false));
        assert_eq!(fs.lookup(doom, &b"..".as_slice().into()).await.unwrap(), apps);

        // The link is a link: not followed to /etc.
        let esc = fs.lookup(apps, &b"escape".as_slice().into()).await.unwrap();
        assert_eq!(fs.readlink(esc).await.unwrap().as_ref(), b"/etc");
        assert!(fs.lookup(esc, &b"passwd".as_slice().into()).await.is_err());
        assert!(fs.read(esc, 0, 10).await.is_err());

        assert!(matches!(fs.write(m, 0, b"x").await, Err(nfsstat3::NFS3ERR_ROFS)));

        // No setuid or setgid bit, and root's.
        std::fs::set_permissions(root.join("apps/doom/manifest"), std::fs::Permissions::from_mode(0o6755)).unwrap();
        let a = fs.getattr(m).await.unwrap();
        assert_eq!((a.mode & 0o7777, a.uid, a.gid), (0o755, 0, 0));
        let names: Vec<_> = fs.readdir(apps, 0, 10).await.unwrap().entries.into_iter().map(|e| e.name.0).collect();
        assert_eq!(names, [b"doom".to_vec(), b"escape".to_vec()]);

        // A restarted server knows the handle; a replaced file's is stale.
        let fresh = TreeFs::new(root.clone());
        let fh = fs.id_to_fh(m);
        assert_eq!(fresh.getattr(fresh.fh_to_id(&fh).unwrap()).await.unwrap().fileid, m);
        std::fs::write(root.join("apps/doom/manifest.new"), b"title=DOOM2\n").unwrap();
        std::fs::rename(root.join("apps/doom/manifest.new"), root.join("apps/doom/manifest")).unwrap();
        assert!(matches!(fs.getattr(m).await, Err(nfsstat3::NFS3ERR_STALE)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
