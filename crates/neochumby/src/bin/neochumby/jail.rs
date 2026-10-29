//! Where the session runs apps: each in a jail of its own
//! (neochumby_sys::jail), holding what its manifest's `access` lists and
//! nothing else, as a user of its own.
//!
//! The jail of an app, whether its `exec` (in front) or its service:
//!
//!   /mnt/neochumby/apps/<name>/   its directory, bound from the tree, read-
//!                                 only (the tree is); NEOCHUMBY_ROOT and its
//!                                 working directory
//!   /tmp                          its own tmpfs (nosuid, nodev, noexec), HOME
//!   /dev/null zero random urandom
//!
//! and, as its `access` lists:
//!
//!   screen    /dev/fb0                     (its exec only)
//!   touch     /dev/input/event0, and the calibration, /psp/ts_settings,
//!             read-only                    (its exec only)
//!   speaker   /dev/dsp, and /tmp/neochumby, where processes hand the
//!             speaker over (neochumby::audio), shared by every app that
//!             lists it (a nosuid, nodev, noexec tmpfs); the mute and the
//!             volume are the session's
//!   accel     /dev/accel
//!   system    /proc, /sys and /etc
//!   network   copies of /etc/resolv.conf and /etc/hosts (with `system`,
//!             /etc has them); and the session lets it keep internet
//!             sockets (network.rs)
//!   terminal  /dev/ptmx and the Chumby's /dev/pts, for pseudo-terminals
//!             (neochumby::term)
//!   logs      the Chumby's logs, each file bound at its own path: the
//!             session's (/tmp/neochumby.log), /tmp's other *.log and
//!             /var/log's files, as they are when the app starts; only
//!             plain files, none setuid or setgid, readable as their
//!             modes say (the session's log is root's, 0644)
//!
//! Each app runs as its own uid (from FIRST_UID, in the tree's name order:
//! an app's exec and its service share one), with a group of the same id
//! and, if it lists `speaker`, SPEAKER_GID beside it. Its devices are made
//! anew in its jail, its own; the host's /dev is left as it is. It may have
//! PROCESSES processes, FILES descriptors and no core dumps.
//!
//! `access = ["root"]` (home) and the session's own services
//! (neochumby-sync) run as before: root, unjailed.
//!
//! Jails need root and the mounts it can make. dev/emulate (user-mode
//! QEMU) allows neither, so it sets NEOCHUMBY_JAIL=off, and the session
//! then runs every app as root, saying so once.

use std::ffi::OsStr;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::Path;
use std::sync::OnceLock;

use neochumby::apps::App;
use neochumby::input;
use neochumby_sys::jail::{Jail, Limit, Process, User};

/// Where each jail's root is mounted, in its own mount namespace.
const MOUNT_POINT: &str = "/tmp/neochumby-jail";
/// The first app's uid.
const FIRST_UID: u32 = 20000;
/// The group that may hand the speaker over.
const SPEAKER_GID: u32 = 19999;
/// The variable that turns jails off, for dev/emulate.
pub const OFF_ENV: &str = "NEOCHUMBY_JAIL";

/// What looking hosts up needs, for `network`: copies, as the app starts.
const NAMES: [&str; 2] = ["/etc/resolv.conf", "/etc/hosts"];

const PROCESSES: u32 = 64;
const FILES: u32 = 256;
/// The jail's root and the app's /tmp, in KiB.
const ROOT_KIB: u32 = 256;
const TMP_KIB: u32 = 4096;

/// What an app is started as: its exec, in front, or its service.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Front,
    Service,
}

/// The apps' names, in the tree's order, which gives their uids.
static JAILS: OnceLock<Vec<String>> = OnceLock::new();
/// Whether [`prepare`] succeeded. When it hasn't, [`start`] refuses every
/// app rather than running one unjailed.
static READY: OnceLock<bool> = OnceLock::new();

/// Turns jails on for `apps` (the tree's, in name order), unless they
/// can't be: not root, or NEOCHUMBY_JAIL=off. Once, as the session starts;
/// until then, and in tests, nothing is jailed.
pub fn init(apps: &[App]) {
    if std::env::var(OFF_ENV).as_deref() == Ok("off") {
        eprintln!("neochumby: {OFF_ENV}=off: every app runs as root, unjailed");
        return;
    }
    if !neochumby_sys::process::is_root() {
        eprintln!("neochumby: not root: apps run as this user, unjailed");
        return;
    }
    // Jails stay on even so: an app's own start fails, and says why.
    let ready = match prepare() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("neochumby: preparing jails: {e}");
            false
        }
    };
    let _ = READY.set(ready);
    let _ = JAILS.set(apps.iter().map(|a| a.name.clone()).collect());
}

/// The mount point, and /tmp/neochumby for the speaker's handover: the
/// session's, and SPEAKER_GID's. The handover is a tmpfs of its own,
/// `nosuid,nodev,noexec`: it is the one host path a speaker app may write,
/// so nothing executable may be put there.
fn prepare() -> io::Result<()> {
    std::fs::create_dir_all(MOUNT_POINT)?;
    std::fs::set_permissions(MOUNT_POINT, std::fs::Permissions::from_mode(0o700))?;
    let speaker = neochumby_sys::speaker::DIR;
    std::fs::create_dir_all(speaker)?;
    // A fresh tmpfs on /tmp/neochumby. If the session is restarted, the
    // directory is already one of ours: its st_dev differs from /tmp's.
    let tmp = std::fs::metadata("/tmp")?;
    let handover = std::fs::metadata(speaker)?;
    if tmp.dev() == handover.dev() {
        neochumby_sys::jail::mount_tmpfs(Path::new(speaker), 64, 0o770, 0, SPEAKER_GID, true)?;
    }
    chown(speaker, Some(0), Some(SPEAKER_GID))?;
    std::fs::set_permissions(speaker, std::fs::Permissions::from_mode(0o770))
}

/// Whether `app` runs jailed.
pub fn jailed(app: &App) -> bool {
    JAILS.get().is_some() && !app.access.root
}

/// The number `app` runs as, jailed: its uid, and its group.
fn number(app: &App) -> io::Result<u32> {
    let names = JAILS.get().ok_or_else(|| io::Error::other("jails are off"))?;
    let i = names.iter().position(|n| *n == app.name).ok_or_else(|| io::Error::other("not in the tree"))?;
    Ok(FIRST_UID + i as u32)
}

/// The uid of `app`, if it runs jailed without `network`: one the session
/// ends if it finds it with an internet socket.
pub fn offline(app: &App) -> Option<u32> {
    (jailed(app) && !app.access.network).then(|| number(app).ok()).flatten()
}

/// The Chumby's logs, for `logs`: plain files only, none setuid or setgid
/// (a bind mount keeps its source's flags, and /var/log may not be
/// nosuid), so no other kind of file comes into the jail with them.
fn logs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for (dir, only_logs) in [("/tmp", true), ("/var/log", false)] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            // symlink_metadata: a link isn't followed out of the directory.
            let plain = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file() && m.mode() & 0o6000 == 0);
            if plain && (!only_logs || p.extension().is_some_and(|x| x == "log")) {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

/// Starts `exe` of `app` jailed, as `role`, with the tree at `root`, only
/// `env` (and HOME, NEOCHUMBY_ROOT, PATH) in its environment and the
/// descriptors in `keep`.
pub fn start(
    app: &App,
    exe: &Path,
    role: Role,
    root: &Path,
    mut env: Vec<(String, String)>,
    keep: &[RawFd],
) -> io::Result<Process> {
    if !READY.get().copied().unwrap_or(false) {
        return Err(io::Error::other("the jail couldn't be prepared"));
    }
    // The ELF loader maps a PT_LOAD at vaddr 0 even though the ARM mmap
    // path refuses MAP_FIXED below PAGE_SIZE. Refuse such a file before
    // exec, wherever it hides in the app's directory.
    neochumby_sys::elf::check(exe)?;
    neochumby_sys::elf::check_app_dir(&app.dir)?;
    let uid = number(app)?;
    let a = app.access;
    let front = role == Role::Front;
    let path = |p: &Path| {
        p.to_str().map(str::to_string).ok_or_else(|| io::Error::other(format!("{}: not UTF-8", p.display())))
    };
    let dir = path(&app.dir)?;
    let exe = path(exe)?;

    let mut j = Jail::new(Path::new(MOUNT_POINT), ROOT_KIB)?;
    j.bind(&app.dir, &dir)?;
    j.tmpfs("/tmp", TMP_KIB, 0o700, uid, uid)?;
    for d in ["null", "zero", "random", "urandom"] {
        let p = format!("/dev/{d}");
        j.device(&p, Path::new(&p), 0o666, 0, 0)?;
    }
    if a.screen && front {
        j.device("/dev/fb0", Path::new("/dev/fb0"), 0o600, uid, uid)?;
    }
    if a.touch && front {
        j.device(input::TOUCHSCREEN, Path::new(input::TOUCHSCREEN), 0o600, uid, uid)?;
        if let Ok(c) = std::fs::read(input::CALIBRATION) {
            j.file(input::CALIBRATION, c, 0o444, 0, 0)?;
        }
    }
    let mut groups = Vec::new();
    if a.speaker {
        j.device("/dev/dsp", Path::new("/dev/dsp"), 0o600, uid, uid)?;
        let handover = neochumby_sys::speaker::DIR;
        j.bind(Path::new(handover), handover)?;
        groups.push(SPEAKER_GID);
    }
    if a.accel {
        j.device("/dev/accel", Path::new("/dev/accel"), 0o600, uid, uid)?;
    }
    if a.system {
        j.kernel_fs("proc", "/proc")?;
        j.kernel_fs("sysfs", "/sys")?;
        j.bind(Path::new("/etc"), "/etc")?;
    }
    if a.terminal {
        // The Chumby's own devpts (the only one this kernel has): another
        // uid's terminals are listed there but closed to this one.
        j.device("/dev/ptmx", Path::new("/dev/ptmx"), 0o666, 0, 0)?;
        j.bind(Path::new("/dev/pts"), "/dev/pts")?;
    }
    if a.logs {
        for f in logs() {
            if let Some(p) = f.to_str() {
                j.bind(&f, p)?;
            }
        }
    }
    if a.network && !a.system {
        for f in NAMES {
            if let Ok(c) = std::fs::read(f) {
                j.file(f, c, 0o444, 0, 0)?;
            }
        }
    }
    j.limit(Limit::Processes(PROCESSES)).limit(Limit::Files(FILES)).limit(Limit::Core(0));

    env.push(("NEOCHUMBY_ROOT".into(), path(root)?));
    env.push(("HOME".into(), "/tmp".into()));
    env.push(("PATH".into(), dir.clone()));
    // What the boot hook gave the session for its apps (the server, say),
    // and the time zone.
    for (k, v) in std::env::vars() {
        if (k.starts_with("NEOCHUMBY_") && k != OFF_ENV && !env.iter().any(|(e, _)| *e == k)) || k == "TZ" {
            env.push((k, v));
        }
    }
    let user = User { uid, gid: uid, groups };
    let args: [&OsStr; 0] = [];
    j.spawn(&exe, &args, &env, &dir, &user, keep)
}
