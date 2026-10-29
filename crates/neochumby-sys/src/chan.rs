//! A channel between threads that never waits on a futex.
//!
//! std's channels, `Mutex`, `Condvar` and thread parking block with futex
//! flags (FUTEX_PRIVATE_FLAG, Linux 2.6.22; FUTEX_WAIT_BITSET, 2.6.25) that
//! the Chumby's 2.6.16 kernel doesn't have. This channel hands each value
//! over as a boxed pointer written to a pipe, and a pipe blocks and poll()s
//! on any kernel. Nothing is copied but the pointer.

use std::fs::File;
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd};

/// The receiving end has gone, or the sending end has.
#[derive(Debug, PartialEq, Eq)]
pub struct Disconnected;

/// Sends values to the [`Receiver`] made with it.
pub struct Sender<T> {
    w: File,
    _t: PhantomData<T>,
}

/// Receives what the [`Sender`] made with it sends, in order.
pub struct Receiver<T> {
    r: File,
    _t: PhantomData<T>,
}

/// A channel for values of type `T`, which crosses between threads (so
/// `T: Send`, and each end is Send when T is).
///
/// Its pipe's descriptors stay inside the two ends, marked close-on-exec,
/// so only the Sender writes to it, and only whole pointers it made from a
/// `Box<T>` and gave up. Values still in the pipe when the Receiver is
/// dropped are freed then. A value sent after that is handed back, once
/// the pipe has no reader left: a child that another thread is starting
/// holds a copy of every descriptor from its fork until its exec, so for
/// that moment a send can still succeed (and its value is never freed).
pub fn channel<T: Send>() -> io::Result<(Sender<T>, Receiver<T>)> {
    let mut fd: [libc::c_int; 2] = [0; 2];
    // SAFETY: pipe(2) writes two descriptors into the two-int array.
    if unsafe { libc::pipe(fd.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just made by pipe(2) and nothing else
    // owns them; each goes to exactly one File, which closes it.
    let (r, w) = unsafe { (File::from_raw_fd(fd[0]), File::from_raw_fd(fd[1])) };
    for f in [&r, &w] {
        // SAFETY: F_SETFD with an int flag on a descriptor we own; it
        // touches no memory. (pipe2's O_CLOEXEC needs Linux 2.6.27.)
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((Sender { w, _t: PhantomData }, Receiver { r, _t: PhantomData }))
}

const PTR: usize = size_of::<usize>();

impl<T: Send> Sender<T> {
    /// Sends `v`; it is handed back if the Receiver has gone.
    pub fn send(&self, v: T) -> Result<(), T> {
        let p = Box::into_raw(Box::new(v)) as usize;
        // A pipe write of at most PIPE_BUF bytes is atomic: the pointer
        // arrives whole, never interleaved with another.
        match (&self.w).write_all(&p.to_ne_bytes()) {
            Ok(()) => Ok(()),
            // SAFETY: the write failed, so the Receiver never got the
            // pointer: it is still the one Box::into_raw just gave us, and
            // this is the only owner taking it back.
            Err(_) => Err(*unsafe { Box::from_raw(p as *mut T) }),
        }
    }
}

impl<T: Send> Receiver<T> {
    /// Waits up to `timeout_ms` (negative: for ever) for the next value.
    /// None if nothing came in time; Disconnected once the Sender has gone
    /// and everything it sent has been received.
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<T>, Disconnected> {
        let mut pf = libc::pollfd { fd: self.r.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            // SAFETY: one pollfd, a live local, for a descriptor we own.
            let n = unsafe { libc::poll(&mut pf, 1, timeout_ms) };
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 {
                return Ok(None);
            }
            break;
        }
        // Readable: a whole pointer is waiting (sends are atomic and we
        // always take whole ones), or the Sender has closed its end.
        let mut b = [0u8; PTR];
        match (&self.r).read_exact(&mut b) {
            // SAFETY: only our Sender writes to this pipe, and each pointer
            // it writes came from Box::into_raw::<T> and is read here once.
            Ok(()) => Ok(Some(*unsafe { Box::from_raw(usize::from_ne_bytes(b) as *mut T) })),
            Err(_) => Err(Disconnected),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Free what is still in the pipe. Non-blocking, so it stops when
        // the pipe is empty; a Sender's later writes then fail.
        // SAFETY: F_SETFL with an int flag on a descriptor we own.
        unsafe { libc::fcntl(self.r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let mut b = [0u8; PTR];
        while (&self.r).read_exact(&mut b).is_ok() {
            // SAFETY: as in recv: a pointer from our Sender, read once.
            drop(unsafe { Box::from_raw(usize::from_ne_bytes(b) as *mut T) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn across_threads_in_order() {
        let (tx, rx) = channel::<Vec<u8>>().unwrap();
        let t = std::thread::spawn(move || {
            for i in 0..100u8 {
                tx.send(vec![i; 1000]).unwrap();
            }
        });
        for i in 0..100u8 {
            assert_eq!(rx.recv(-1).unwrap().unwrap(), vec![i; 1000]);
        }
        t.join().unwrap();
        assert_eq!(rx.recv(-1), Err(Disconnected), "the sender has gone");
    }

    #[test]
    fn timeouts_and_a_gone_receiver() {
        let (tx, rx) = channel::<String>().unwrap();
        assert_eq!(rx.recv(0), Ok(None));
        assert_eq!(rx.recv(10), Ok(None));
        tx.send("a".into()).unwrap();
        tx.send("b".into()).unwrap();
        drop(rx); // frees "b" and "a"
        // Handed back once nothing reads the pipe. Other tests start
        // processes, and a child between its fork and its exec still holds
        // the read end, so the first sends may succeed: try for a while.
        let gone = (0..1000).any(|_| {
            let r = tx.send("c".into());
            if r.is_ok() {
                crate::time::sleep_ms(10);
            }
            r == Err("c".to_string())
        });
        assert!(gone, "a send to a dropped receiver was never handed back");
    }
}
