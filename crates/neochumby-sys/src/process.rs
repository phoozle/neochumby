//! Signals to other processes, by pid or by process group, whether a
//! process is alive, the descriptors a child is given, and this process's
//! priority.

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A signal [`kill`] and [`kill_group`] can send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM: asks the process to stop.
    Term,
    /// SIGKILL: ends it.
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Sends `sig` to the process `pid` (as from `Child::id`). A pid of 0, or
/// one too large for a pid_t, is refused without a system call: kill(2)
/// would read it as a process group or every process.
pub fn kill(pid: u32, sig: Signal) -> io::Result<()> {
    send(positive(pid)?, sig)
}

/// Sends `sig` to every process in the group `pgid` (the group a child
/// leads after `process_group(0)` has its pid as the group's id). 0 or a
/// value too large for a pid_t is refused, as for [`kill`].
pub fn kill_group(pgid: u32, sig: Signal) -> io::Result<()> {
    send(-positive(pgid)?, sig)
}

/// Whether this process runs as root (its effective uid is 0).
pub fn is_root() -> bool {
    // SAFETY: geteuid(2) takes nothing and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Lowers this process's priority by `by` (nice(2)); 19 is the lowest. A
/// process may always lower its own, so nothing is reported.
pub fn lower_priority(by: u8) {
    // SAFETY: nice(2) takes an integer and touches no memory of ours.
    unsafe { libc::nice(libc::c_int::from(by.min(19))) };
}

/// Whether any process is left in the group `pgid` (kill(2) with signal
/// 0, which sends nothing). False for an id [`kill_group`] would refuse.
pub fn group_alive(pgid: u32) -> bool {
    let Ok(p) = positive(pgid) else { return false };
    // SAFETY: as in `send`; signal 0 only checks that the group exists and
    // may be signalled.
    unsafe { libc::kill(-p, 0) == 0 }
}

/// Whether the process `pid` exists (a zombie, not yet reaped, counts), as
/// kill(2) with no signal says: it does unless the kernel finds no such
/// process. 0, or a value too large for a pid_t, is not a process.
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = positive(pid) else { return false };
    // SAFETY: kill(2) takes plain integers and touches no memory of ours;
    // signal 0 sends nothing, and `pid` is a single process.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM: it exists, but is another user's.
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Whether our child `pid` has exited, without reaping it (waitid(2) with
/// WNOWAIT). Until it's reaped (`Child::wait`), its pid, and the id of the
/// process group it leads, can't be given to another process, so its group
/// can still be signalled safely.
pub fn exited(pid: u32) -> io::Result<bool> {
    let pid = positive(pid)?;
    // SAFETY: siginfo_t is plain data, for which all zeroes is valid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: `info` is a live siginfo_t the kernel writes into; the other
    // arguments are integers.
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid filled in `info`; with WNOHANG and nothing to report,
    // it left si_pid as the zero it was given.
    Ok(unsafe { info.si_pid() } != 0)
}

fn positive(id: u32) -> io::Result<libc::pid_t> {
    match libc::pid_t::try_from(id) {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a process id")),
    }
}

fn send(target: libc::pid_t, sig: Signal) -> io::Result<()> {
    // SAFETY: kill(2) takes plain integers and touches no memory of ours;
    // `target` is a single process or a single group, never 0 or -1.
    if unsafe { libc::kill(target, sig.raw()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// This process's real user id (getuid(2)).
pub fn uid() -> u32 {
    // SAFETY: getuid(2) takes nothing, touches no memory and can't fail.
    unsafe { libc::getuid() }
}

/// Passes the descriptors `fds` on to the program `cmd` runs, though they
/// are close-on-exec here: in the child, between fork and exec, each is
/// marked not close-on-exec. Starting the program fails if one isn't open.
pub fn pass_fds(cmd: &mut Command, fds: Vec<RawFd>) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: it calls fcntl(2), which is,
    // reads a Vec allocated before the fork, and allocates nothing.
    unsafe {
        cmd.pre_exec(move || {
            for &fd in &fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_what_is_not_one_process_or_group() {
        for id in [0, u32::MAX, 1 << 31] {
            assert_eq!(kill(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(kill_group(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert!(!group_alive(id));
            assert!(!alive(id));
        }
    }

    #[test]
    fn alive_until_reaped() {
        assert!(alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn exited_leaves_the_zombie_to_be_reaped() {
        let mut child = std::process::Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !exited(child.id()).unwrap() {
            assert!(std::time::Instant::now() < deadline, "never exited");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Not reaped: its pid is still taken, and its status still there.
        assert!(alive(child.id()));
        assert_eq!(child.wait().unwrap().code(), Some(3));
        assert!(exited(0).is_err());
    }
}
