//! Apps without `network` in their `access`, kept off the network as well
//! as the stock kernel allows.
//!
//! It has no network namespaces and no netfilter, so a jailed app can open
//! a socket whatever its manifest says. What it can't do is hide it: the
//! kernel lists every internet socket, with its owner's uid, in /proc/net.
//! The session looks there every CHECK and ends an app that lists no
//! `network` but whose uid owns one (its exec in front, or its service).
//!
//! This finds a socket; it doesn't stop one. An app that sends a packet and
//! closes its socket between two looks goes unseen. It does mean that an
//! app can't quietly keep a connection, or a port, it never declared.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How often the session looks.
pub const CHECK: Duration = Duration::from_secs(1);

/// Where the kernel lists internet sockets; those it doesn't have (IPv6
/// on the Chumby) are skipped.
const TABLES: [&str; 6] =
    ["/proc/net/tcp", "/proc/net/udp", "/proc/net/raw", "/proc/net/tcp6", "/proc/net/udp6", "/proc/net/raw6"];

/// The uids owning an internet socket, as the kernel lists them now.
pub fn owners() -> HashSet<u32> {
    TABLES.iter().filter_map(|t| std::fs::read_to_string(t).ok()).flat_map(|s| parse(&s)).collect()
}

/// The uids in one of /proc/net's tables: after the heading, a line a
/// socket, its owner's uid the eighth field.
fn parse(table: &str) -> Vec<u32> {
    table.lines().skip(1).filter_map(|l| l.split_whitespace().nth(7)?.parse().ok()).collect()
}

/// When the session looks next.
pub struct Watch {
    due: Instant,
}

impl Watch {
    pub fn new() -> Watch {
        Watch { due: Instant::now() }
    }

    /// The uids owning an internet socket, if it's time to look again.
    pub fn due(&mut self) -> Option<HashSet<u32>> {
        if Instant::now() < self.due {
            return None;
        }
        self.due = Instant::now() + CHECK;
        Some(owners())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_are_read_from_the_eighth_field() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1234 1 c3a1e000 3000 0 0 2 -1
   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 20003        0 5678 1 c3a1e400 3000 0 0 2 -1
";
        assert_eq!(parse(tcp), [0, 20003]);
        assert_eq!(parse("heading only\n"), Vec::<u32>::new());
    }
}
