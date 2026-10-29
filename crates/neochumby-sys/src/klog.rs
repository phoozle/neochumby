//! The kernel's log ring, as dmesg prints it: syslog(2)'s READ_ALL, which
//! the Chumby's 2.6.16 kernel allows any user (only clearing the ring, or
//! reading it destructively, takes root), so a jailed app can read it
//! without /proc or a dmesg binary.

use std::io;

/// SYSLOG_ACTION_READ_ALL and SYSLOG_ACTION_SIZE_BUFFER.
const READ_ALL: libc::c_int = 3;
const SIZE_BUFFER: libc::c_int = 10;

/// The ring's whole contents, oldest first, without taking anything out.
pub fn read_all() -> io::Result<Vec<u8>> {
    // SAFETY: SIZE_BUFFER reads no buffer: a null pointer and length 0.
    let size = unsafe { libc::klogctl(SIZE_BUFFER, std::ptr::null_mut(), 0) };
    // Kernels before 2.6.27 don't answer SIZE_BUFFER: the largest ring a
    // Chumby's kernel can be built with is 128 KB.
    let size = if size > 0 { size as usize } else { 128 * 1024 };
    let mut buf = vec![0u8; size];
    // SAFETY: klogctl writes at most `len` bytes into `buf`, which is that
    // long and ours for the call.
    let n = unsafe { libc::klogctl(READ_ALL, buf.as_mut_ptr().cast(), buf.len() as libc::c_int) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_or_is_refused() {
        // Containers often refuse it (dmesg_restrict); either way, no crash.
        match super::read_all() {
            Ok(b) => assert!(b.len() <= 1 << 24),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}"),
        }
    }
}
