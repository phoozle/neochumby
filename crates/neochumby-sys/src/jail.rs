//! Starting a program jailed: in a mount namespace of its own, chrooted into
//! a tmpfs laid out for it, with rlimits, as a user that isn't root.
//!
//! The Chumby's kernel (2.6.16) has no user, pid or net namespaces, no
//! seccomp filters and no unshare(2) on ARM, so the jail is what it does
//! have:
//!
//! - **A mount namespace**, from clone(2) with CLONE_NEWNS (2.4.19): what is
//!   mounted for the jail exists only in it, and goes when its last process
//!   does.
//! - **chroot(2)** into a tmpfs (`nosuid`) holding only what [`Jail`] puts
//!   there: directories, other tmpfs (`nosuid,nodev`), bind mounts, device
//!   nodes made like the host's, files, /proc and /sys.
//! - **A uid and gid that aren't root**, set last, so the program can't
//!   chroot out, mount, or make device nodes.
//! - **rlimits**, set before the uid, so it can't raise them again.
//!
//! The child is made by a raw clone(2), not libc's fork(), and between it
//! and execve(2) it makes only system calls, with everything they read
//! built beforehand: the session that calls this has threads, so the child
//! may allocate nothing, take no lock and touch no libc state. The calls
//! musl applies to every thread (setuid, setgid, setgroups, setrlimit) are
//! made raw for the same reason: in the child musl still counts the
//! parent's threads.
//!
//! What failed before execve comes back to [`Jail::spawn`] as its error,
//! naming the step, through a pipe closed on exec.

// libc's constants and ids differ in type between glibc and musl: the casts
// that are needed on one are no-ops on the other.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ExitStatus};

/// A process started by [`Jail::spawn`], or a std [`Child`] taken over: its
/// pid, and its exit once reaped.
#[derive(Debug)]
pub struct Process {
    pid: u32,
    status: Option<ExitStatus>,
}

impl Process {
    /// The process's id, which is also its group's (it leads one).
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Waits for it to exit and reaps it; its exit. Once reaped, the same
    /// exit again.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(s) = self.status {
            return Ok(s);
        }
        let mut raw: libc::c_int = 0;
        loop {
            // SAFETY: waitpid(2) writes the status into `raw`, a live
            // c_int; the pid is our own child's.
            let r = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut raw, 0) };
            if r >= 0 {
                break;
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
        let s = ExitStatus::from_raw(raw);
        self.status = Some(s);
        Ok(s)
    }
}

impl From<Child> for Process {
    /// Takes over `child`, which was started in a process group of its own
    /// and is waited on only through this from now on. (Dropping a Child
    /// neither waits for nor kills it.)
    fn from(child: Child) -> Process {
        Process { pid: child.id(), status: None }
    }
}

/// A resource the jailed program is limited in (setrlimit(2)); the soft and
/// hard limits are the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// Processes its user may have at once (RLIMIT_NPROC).
    Processes(u32),
    /// Descriptors it may have open (RLIMIT_NOFILE).
    Files(u32),
    /// Its address space, in bytes (RLIMIT_AS).
    Memory(u32),
    /// The largest core dump, in bytes (RLIMIT_CORE); 0 for none.
    Core(u32),
}

impl Limit {
    fn raw(self) -> (libc::c_int, libc::c_ulong) {
        match self {
            Limit::Processes(n) => (libc::RLIMIT_NPROC as libc::c_int, n as libc::c_ulong),
            Limit::Files(n) => (libc::RLIMIT_NOFILE as libc::c_int, n as libc::c_ulong),
            Limit::Memory(n) => (libc::RLIMIT_AS as libc::c_int, n as libc::c_ulong),
            Limit::Core(n) => (libc::RLIMIT_CORE as libc::c_int, n as libc::c_ulong),
        }
    }
}

/// One step in laying out the jail, its paths already the host's (the
/// jail's root joined on) and every argument built.
#[derive(Debug)]
enum Step {
    Mount {
        what: &'static str,
        source: CString,
        target: CString,
        fstype: Option<CString>,
        flags: libc::c_ulong,
        data: Option<CString>,
    },
    Dir {
        path: CString,
        mode: libc::mode_t,
        uid: libc::uid_t,
        gid: libc::gid_t,
    },
    File {
        path: CString,
        contents: Vec<u8>,
        mode: libc::mode_t,
        uid: libc::uid_t,
        gid: libc::gid_t,
    },
    Node {
        path: CString,
        rdev: libc::dev_t,
        mode: libc::mode_t,
        uid: libc::uid_t,
        gid: libc::gid_t,
    },
}

impl Step {
    fn what(&self) -> &'static str {
        match self {
            Step::Mount { what, .. } => what,
            Step::Dir { .. } => "a directory",
            Step::File { .. } => "a file",
            Step::Node { .. } => "a device node",
        }
    }
}

/// Who the jailed program runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
    /// Its supplementary groups.
    pub groups: Vec<u32>,
}

/// A jail, laid out step by step, then started with [`Jail::spawn`]. Paths
/// given to it are the jail's (absolute: "/dev/fb0"); paths it reads from
/// are the host's.
#[derive(Debug)]
pub struct Jail {
    root: Vec<u8>,
    steps: Vec<Step>,
    limits: Vec<Limit>,
}

fn cstring(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL in it"))
}

fn jail_path(p: &str) -> io::Result<&str> {
    if !p.starts_with('/') || p.split('/').any(|c| c == "..") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{p}: not an absolute path in the jail")));
    }
    Ok(p)
}

/// Mounts a fresh tmpfs at `path`, `nosuid,nodev` and, with `noexec`,
/// `noexec`. The session uses it for the speaker's shared handover
/// directory: that directory is the one host path a jailed app with
/// `speaker` may write, and it must not be able to put an executable there.
pub fn mount_tmpfs(path: &Path, size_kib: u32, mode: u32, uid: u32, gid: u32, noexec: bool) -> io::Result<()> {
    let source = cstring(b"tmpfs")?;
    let target = cstring(path.as_os_str().as_bytes())?;
    let fstype = cstring(b"tmpfs")?;
    let data = cstring(format!("mode={mode:o},uid={uid},gid={gid},size={size_kib}k").as_bytes())?;
    let mut flags = (libc::MS_NOSUID | libc::MS_NODEV) as libc::c_ulong;
    if noexec {
        flags |= libc::MS_NOEXEC as libc::c_ulong;
    }
    // SAFETY: mount(2) reads the three NUL-terminated strings and the data
    // string, all of which outlive the call; it touches no memory of ours.
    if unsafe { libc::mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), flags, data.as_ptr().cast()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Jail {
    /// A jail whose root is a tmpfs of `size_kib` mounted (in its own
    /// namespace) on `mount_point`, an empty directory on the host that
    /// every jail may share: each mounts its own tmpfs there.
    pub fn new(mount_point: &Path, size_kib: u32) -> io::Result<Jail> {
        let root = mount_point.as_os_str().as_bytes().to_vec();
        let mut j = Jail { root: root.clone(), steps: Vec::new(), limits: Vec::new() };
        j.steps.push(Step::Mount {
            what: "the jail's root",
            source: cstring(b"none")?,
            target: cstring(&root)?,
            fstype: Some(cstring(b"tmpfs")?),
            flags: libc::MS_NOSUID as libc::c_ulong,
            data: Some(cstring(format!("mode=755,size={size_kib}k").as_bytes())?),
        });
        Ok(j)
    }

    fn host(&self, p: &str) -> io::Result<CString> {
        let mut h = self.root.clone();
        h.extend_from_slice(jail_path(p)?.as_bytes());
        cstring(&h)
    }

    /// A directory, owned by `uid`:`gid`.
    pub fn dir(&mut self, path: &str, mode: u32, uid: u32, gid: u32) -> io::Result<&mut Jail> {
        let path = self.host(path)?;
        self.steps.push(Step::Dir { path, mode: mode as libc::mode_t, uid, gid });
        Ok(self)
    }

    /// A directory and every one above it not made yet, root's, 0755.
    pub fn dirs(&mut self, path: &str) -> io::Result<&mut Jail> {
        let mut at = String::new();
        for c in jail_path(path)?.split('/').filter(|c| !c.is_empty()) {
            at.push('/');
            at.push_str(c);
            let host = self.host(&at)?;
            if !self.steps.iter().any(|s| matches!(s, Step::Dir { path, .. } if *path == host)) {
                self.dir(&at, 0o755, 0, 0)?;
            }
        }
        Ok(self)
    }

    /// A tmpfs of `size_kib` (`nosuid,nodev`) on the directory `path`,
    /// owned by `uid`:`gid` with `mode`.
    pub fn tmpfs(&mut self, path: &str, size_kib: u32, mode: u32, uid: u32, gid: u32) -> io::Result<&mut Jail> {
        self.dirs(path)?;
        let target = self.host(path)?;
        self.steps.push(Step::Mount {
            what: "a tmpfs",
            source: cstring(b"none")?,
            target,
            fstype: Some(cstring(b"tmpfs")?),
            flags: (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong,
            data: Some(cstring(format!("mode={mode:o},uid={uid},gid={gid},size={size_kib}k").as_bytes())?),
        });
        Ok(self)
    }

    /// The host's directory or file `from`, bound on `to`. A bind mount
    /// keeps the flags of the mount it is from (2.6.16 can't change them),
    /// so what is bound must be `nosuid` where it comes from, or hold
    /// nothing setuid.
    pub fn bind(&mut self, from: &Path, to: &str) -> io::Result<&mut Jail> {
        let meta = std::fs::metadata(from)?;
        if meta.is_dir() {
            self.dirs(to)?;
        } else {
            let parent = to.rsplit_once('/').map(|(p, _)| p).filter(|p| !p.is_empty());
            if let Some(p) = parent {
                self.dirs(p)?;
            }
            let path = self.host(to)?;
            self.steps.push(Step::File { path, contents: Vec::new(), mode: 0o444, uid: 0, gid: 0 });
        }
        let target = self.host(to)?;
        self.steps.push(Step::Mount {
            what: "a bind mount",
            source: cstring(from.as_os_str().as_bytes())?,
            target,
            fstype: None,
            flags: libc::MS_BIND as libc::c_ulong,
            data: None,
        });
        Ok(self)
    }

    /// A file holding `contents`, owned by `uid`:`gid`.
    pub fn file(&mut self, path: &str, contents: Vec<u8>, mode: u32, uid: u32, gid: u32) -> io::Result<&mut Jail> {
        if let Some((p, _)) = path.rsplit_once('/').filter(|(p, _)| !p.is_empty()) {
            self.dirs(p)?;
        }
        let path = self.host(path)?;
        self.steps.push(Step::File { path, contents, mode: mode as libc::mode_t, uid, gid });
        Ok(self)
    }

    /// A character device node at `path`, the same device as the host's
    /// `like`, owned by `uid`:`gid` with `mode`.
    pub fn device(&mut self, path: &str, like: &Path, mode: u32, uid: u32, gid: u32) -> io::Result<&mut Jail> {
        let meta = std::fs::metadata(like)?;
        if !meta.file_type().is_char_device() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{}: not a device", like.display())));
        }
        if let Some((p, _)) = path.rsplit_once('/').filter(|(p, _)| !p.is_empty()) {
            self.dirs(p)?;
        }
        let path = self.host(path)?;
        self.steps.push(Step::Node { path, rdev: meta.rdev() as libc::dev_t, mode: mode as libc::mode_t, uid, gid });
        Ok(self)
    }

    /// A filesystem with no device (`proc`, `sysfs`) on `path`, `nosuid,
    /// nodev,noexec`.
    pub fn kernel_fs(&mut self, fstype: &'static str, path: &str) -> io::Result<&mut Jail> {
        self.dirs(path)?;
        let target = self.host(path)?;
        self.steps.push(Step::Mount {
            what: fstype,
            source: cstring(fstype.as_bytes())?,
            target,
            fstype: Some(cstring(fstype.as_bytes())?),
            flags: (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong,
            data: None,
        });
        Ok(self)
    }

    /// Limits the program in `limit`.
    pub fn limit(&mut self, limit: Limit) -> &mut Jail {
        self.limits.push(limit);
        self
    }

    /// Starts `exe` (the jail's path to it) in the jail, as `user`, in
    /// `dir` (the jail's), with `args` after it and only `env`. It leads a
    /// process group of its own. Of this process's descriptors it gets
    /// stdin, stdout, stderr and those in `keep`; the rest are
    /// close-on-exec, as std opens them.
    pub fn spawn(
        &self,
        exe: &str,
        args: &[&OsStr],
        env: &[(String, String)],
        dir: &str,
        user: &User,
        keep: &[RawFd],
    ) -> io::Result<Process> {
        let plan = Plan::new(Some(self), exe, args, env, dir, Some(user), keep)?;
        plan.run(libc::CLONE_NEWNS)
    }
}

/// Everything the child needs, built before the clone.
struct Plan<'a> {
    jail: Option<&'a Jail>,
    root: Option<CString>,
    exe: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    dir: CString,
    user: Option<(libc::uid_t, libc::gid_t, Vec<libc::gid_t>)>,
    keep: &'a [RawFd],
}

impl<'a> Plan<'a> {
    fn new(
        jail: Option<&'a Jail>,
        exe: &str,
        args: &[&OsStr],
        env: &[(String, String)],
        dir: &str,
        user: Option<&User>,
        keep: &'a [RawFd],
    ) -> io::Result<Plan<'a>> {
        let mut argv = vec![cstring(exe.as_bytes())?];
        for a in args {
            argv.push(cstring(a.as_bytes())?);
        }
        let envp = env.iter().map(|(k, v)| cstring(format!("{k}={v}").as_bytes())).collect::<io::Result<_>>()?;
        Ok(Plan {
            jail,
            root: jail.map(|j| cstring(&j.root)).transpose()?,
            exe: cstring(exe.as_bytes())?,
            argv,
            envp,
            dir: cstring(dir.as_bytes())?,
            user: user.map(|u| (u.uid, u.gid, u.groups.clone())),
            keep,
        })
    }

    /// Clones with `flags` (and SIGCHLD), runs the plan in the child, and
    /// returns once it has exec'd, or with what failed.
    fn run(&self, flags: libc::c_int) -> io::Result<Process> {
        let mut argv: Vec<*const libc::c_char> = self.argv.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut envp: Vec<*const libc::c_char> = self.envp.iter().map(|e| e.as_ptr()).collect();
        envp.push(std::ptr::null());
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe(2) writes two descriptors into `fds`, a live array
        // of two. (pipe2 and O_CLOEXEC are newer than 2.6.16.)
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (rd, wr) = (fds[0], fds[1]);
        for fd in fds {
            // SAFETY: fcntl(2) on a descriptor we just opened.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: a raw clone(2) without CLONE_VM is a fork: the child gets
        // a copy of this process's memory and returns here with 0. It runs
        // only `child`, which makes system calls on what was built above
        // and never returns.
        let pid = unsafe {
            libc::syscall(libc::SYS_clone, (flags | libc::SIGCHLD) as libc::c_ulong, 0usize, 0usize, 0usize, 0usize)
        };
        if pid < 0 {
            let e = io::Error::last_os_error();
            // SAFETY: closing the pipe we opened.
            unsafe {
                libc::close(rd);
                libc::close(wr);
            }
            return Err(e);
        }
        if pid == 0 {
            // SAFETY: in the cloned child, as above.
            unsafe { self.child(wr, &argv, &envp) }
        }
        // SAFETY: closing our copy of the write end; the child has its own.
        unsafe { libc::close(wr) };
        let mut report = [0u8; 8];
        let mut got = 0;
        while got < report.len() {
            // SAFETY: read(2) into the rest of `report`, a live buffer.
            let n = unsafe { libc::read(rd, report[got..].as_mut_ptr().cast(), report.len() - got) };
            if n > 0 {
                got += n as usize;
            } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
        // SAFETY: closing the read end, which we opened.
        unsafe { libc::close(rd) };
        let mut process = Process { pid: pid as u32, status: None };
        if got == 0 {
            // The pipe closed on exec: the program is running.
            return Ok(process);
        }
        let _ = process.wait();
        let step = u32::from_ne_bytes([report[0], report[1], report[2], report[3]]) as usize;
        let errno = i32::from_ne_bytes([report[4], report[5], report[6], report[7]]);
        let what = self.describe(step);
        let e = io::Error::from_raw_os_error(errno);
        Err(io::Error::new(e.kind(), format!("{what}: {e}")))
    }

    fn describe(&self, step: usize) -> String {
        let steps = self.jail.map(|j| j.steps.as_slice()).unwrap_or_default();
        match step {
            s if s < steps.len() => format!("laying out the jail ({})", steps[s].what()),
            s => match s - steps.len() {
                0 => "starting a process group".into(),
                1 => "entering the jail".into(),
                2 => "changing directory".into(),
                3 => "passing descriptors".into(),
                4 => "setting limits".into(),
                5 => "becoming the app's user".into(),
                _ => format!("running {}", self.exe.to_string_lossy()),
            },
        }
    }

    /// The child, from clone to execve. Reports the step that failed, with
    /// errno, on `wr`, and exits 127.
    ///
    /// # Safety
    ///
    /// Only in the child of [`Plan::run`]'s clone.
    unsafe fn child(&self, wr: RawFd, argv: &[*const libc::c_char], envp: &[*const libc::c_char]) -> ! {
        let fail = |step: usize| -> ! {
            let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
            let mut report = [0u8; 8];
            report[..4].copy_from_slice(&(step as u32).to_ne_bytes());
            report[4..].copy_from_slice(&errno.to_ne_bytes());
            // SAFETY: write(2) from a live buffer, then _exit(2): both are
            // plain system calls.
            unsafe {
                libc::write(wr, report.as_ptr().cast(), report.len());
                libc::_exit(127)
            }
        };
        // SAFETY: everything below is a system call on descriptors and
        // C strings built before the clone, or on plain integers.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::umask(0o022);

            let steps = self.jail.map(|j| j.steps.as_slice()).unwrap_or_default();
            for (i, s) in steps.iter().enumerate() {
                let ok = match s {
                    Step::Mount { source, target, fstype, flags, data, .. } => {
                        let t = fstype.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
                        let d = data.as_ref().map_or(std::ptr::null(), |d| d.as_ptr().cast());
                        libc::mount(source.as_ptr(), target.as_ptr(), t, *flags, d) == 0
                    }
                    Step::Dir { path, mode, uid, gid } => {
                        libc::mkdir(path.as_ptr(), *mode) == 0
                            && libc::chown(path.as_ptr(), *uid, *gid) == 0
                            && libc::chmod(path.as_ptr(), *mode) == 0
                    }
                    Step::File { path, contents, mode, uid, gid } => {
                        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, *mode);
                        let mut ok = fd >= 0;
                        let mut at = 0;
                        while ok && at < contents.len() {
                            let n = libc::write(fd, contents[at..].as_ptr().cast(), contents.len() - at);
                            ok = n > 0;
                            at += n.max(0) as usize;
                        }
                        if fd >= 0 {
                            libc::close(fd);
                        }
                        ok && libc::chown(path.as_ptr(), *uid, *gid) == 0 && libc::chmod(path.as_ptr(), *mode) == 0
                    }
                    Step::Node { path, rdev, mode, uid, gid } => {
                        libc::mknod(path.as_ptr(), libc::S_IFCHR | *mode, *rdev) == 0
                            && libc::chown(path.as_ptr(), *uid, *gid) == 0
                            && libc::chmod(path.as_ptr(), *mode) == 0
                    }
                };
                if !ok {
                    fail(i);
                }
            }
            let n = steps.len();
            if libc::setpgid(0, 0) != 0 {
                fail(n);
            }
            if let Some(root) = &self.root
                && libc::chroot(root.as_ptr()) != 0
            {
                fail(n + 1);
            }
            if libc::chdir(self.dir.as_ptr()) != 0 {
                fail(n + 2);
            }
            for &fd in self.keep {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    fail(n + 3);
                }
            }
            for l in self.jail.map(|j| j.limits.as_slice()).unwrap_or_default() {
                let (resource, value) = l.raw();
                // The kernel's own struct: two unsigned longs (musl's
                // rlim_t is 64 bits, the old call's are not).
                let lim: [libc::c_ulong; 2] = [value, value];
                if libc::syscall(libc::SYS_setrlimit, resource, lim.as_ptr()) != 0 {
                    fail(n + 4);
                }
            }
            if let Some((uid, gid, groups)) = &self.user
                && (raw_setgroups(groups) != 0 || raw_setgid(*gid) != 0 || raw_setuid(*uid) != 0)
            {
                fail(n + 5);
            }
            libc::execve(self.exe.as_ptr(), argv.as_ptr(), envp.as_ptr());
            fail(n + 6)
        }
    }
}

// The 32-bit-id calls on ARM; the plain ones elsewhere.
#[cfg(not(target_arch = "arm"))]
use libc::{SYS_setgid as SYS_SETGID, SYS_setgroups as SYS_SETGROUPS, SYS_setuid as SYS_SETUID};
#[cfg(target_arch = "arm")]
use libc::{SYS_setgid32 as SYS_SETGID, SYS_setgroups32 as SYS_SETGROUPS, SYS_setuid32 as SYS_SETUID};

/// setgroups(2), raw.
///
/// # Safety
///
/// A system call reading `groups`, which outlives it.
unsafe fn raw_setgroups(groups: &[libc::gid_t]) -> libc::c_long {
    // SAFETY: the kernel reads `groups.len()` gids from a live slice.
    unsafe { libc::syscall(SYS_SETGROUPS, groups.len() as libc::c_long, groups.as_ptr()) }
}

/// setgid(2), raw.
///
/// # Safety
///
/// A system call on an integer.
unsafe fn raw_setgid(gid: libc::gid_t) -> libc::c_long {
    // SAFETY: an integer argument only.
    unsafe { libc::syscall(SYS_SETGID, gid as libc::c_long) }
}

/// setuid(2), raw.
///
/// # Safety
///
/// A system call on an integer.
unsafe fn raw_setuid(uid: libc::uid_t) -> libc::c_long {
    // SAFETY: an integer argument only.
    unsafe { libc::syscall(SYS_SETUID, uid as libc::c_long) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(exe: &str, args: &[&str], env: &[(&str, &str)], keep: &[RawFd]) -> io::Result<Process> {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Plan::new(None, exe, &args, &env, "/", None, keep)?.run(0)
    }

    #[test]
    fn runs_with_only_the_env_given_in_a_group_of_its_own() {
        let p = run(
            "/bin/sh",
            &["-c", r#"test "$A" = b && test -z "$HOME" && test "$(cut -d' ' -f5 /proc/$$/stat)" = $$"#],
            &[("A", "b")],
            &[],
        )
        .unwrap();
        let mut p = p;
        assert!(p.wait().unwrap().success());
        assert!(p.wait().unwrap().success(), "the same exit again");
    }

    #[test]
    fn what_failed_comes_back() {
        let e = run("/nonexistent", &[], &[], &[]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().starts_with("running /nonexistent"), "{e}");
        let e = run("/bin/true", &[], &[], &[9999]).unwrap_err();
        assert!(e.to_string().starts_with("passing descriptors"), "{e}");
    }

    #[test]
    fn passes_the_descriptors_kept() {
        use std::os::fd::AsRawFd;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = a.as_raw_fd().to_string();
        let mut p = run("/bin/sh", &["-c", &format!("echo hi >&{fd}")], &[], &[a.as_raw_fd()]).unwrap();
        drop(a);
        assert!(p.wait().unwrap().success());
        let mut s = String::new();
        std::io::Read::read_to_string(&mut b, &mut s).unwrap();
        assert_eq!(s, "hi\n");
    }

    #[test]
    fn refuses_paths_outside_the_jail() {
        let mut j = Jail::new(Path::new("/nowhere"), 64).unwrap();
        assert!(j.dir("relative", 0o755, 0, 0).is_err());
        assert!(j.dir("/a/../b", 0o755, 0, 0).is_err());
        j.dirs("/a/b/c").unwrap();
        j.dirs("/a/b/d").unwrap();
        assert_eq!(j.steps.iter().filter(|s| matches!(s, Step::Dir { .. })).count(), 4);
    }

    /// The jail itself, as root on Linux (skipped otherwise): run by hand
    /// with `sudo -E cargo test -p neochumby-sys jail -- --include-ignored`.
    #[test]
    #[ignore]
    fn a_jail_holds_only_what_was_put_there() {
        let mp = std::env::temp_dir().join(format!("neochumby-jail-test-{}", std::process::id()));
        std::fs::create_dir_all(&mp).unwrap();
        let mut j = Jail::new(&mp, 256).unwrap();
        j.tmpfs("/tmp", 64, 0o700, 20000, 20000).unwrap();
        j.device("/dev/null", Path::new("/dev/null"), 0o666, 0, 0).unwrap();
        j.bind(Path::new("/bin"), "/bin").unwrap();
        j.bind(Path::new("/lib"), "/lib").unwrap();
        j.file("/hello", b"hi\n".to_vec(), 0o444, 0, 0).unwrap();
        j.limit(Limit::Core(0)).limit(Limit::Files(64));
        let user = User { uid: 20000, gid: 20000, groups: vec![] };
        let script = "test \"$(cat /hello)\" = hi && echo x > /tmp/x && ! (echo x > /x) 2>/dev/null \
                      && test ! -e /etc && test \"$(id -u)\" = 20000 && test \"$(ulimit -n)\" = 64";
        let mut p = j.spawn("/bin/sh", &[OsStr::new("-c"), OsStr::new(script)], &[], "/", &user, &[]).unwrap();
        assert!(p.wait().unwrap().success());
        assert_eq!(std::fs::read_dir(&mp).unwrap().count(), 0, "the jail's mounts were its own");
        std::fs::remove_dir(&mp).unwrap();
    }
}
