//! Remote control of the sender (DACP): how AirPlay receivers ask the
//! phone or Mac playing to them to pause, skip or go back, as shairport-sync
//! does in its own dacp.c. As a stream starts, the sender tells the
//! receiver its DACP-ID and an Active-Remote token, which shairport-sync
//! passes on as metadata (`daid`, `acre`), with the sender's address
//! (`clip`). The sender serves DACP over HTTP on a port it announces over
//! mDNS as `iTunes_Ctrl_<DACP-ID>._dacp._tcp`. A command is then
//!
//!   GET /ctrl-int/1/playpause HTTP/1.1
//!   Active-Remote: <token>
//!
//! The port: shairport-sync built with a remote-control interface (D-Bus,
//! MPRIS or MQTT) looks it up through avahi itself and passes it on as
//! `dapo`. Otherwise it is looked up here, with avahi's own avahi-browse
//! (avahi-utils), as shairport-sync's avahi backend does.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use neochumby_nowplaying::frames::Control;

/// How long connecting, sending and the answer may each take.
const TIMEOUT: Duration = Duration::from_secs(2);

/// What it takes to control the sender of the current stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remote {
    /// The sender's address (`clip`).
    pub ip: String,
    /// Its DACP-ID (`daid`).
    pub id: String,
    /// The Active-Remote token (`acre`).
    pub token: String,
    /// Its DACP port, once known (`dapo`, or looked up).
    pub port: Option<u16>,
}

impl Remote {
    pub fn ready(&self) -> bool {
        !self.ip.is_empty() && !self.id.is_empty() && !self.token.is_empty()
    }
}

/// The DACP command for a button.
pub fn command(c: Control) -> &'static str {
    match c {
        Control::PlayPause => "playpause",
        Control::Next => "nextitem",
        Control::Previous => "previtem",
        Control::Pause => "pause",
        Control::Play => "play",
    }
}

/// Sends `c` to the sender, looking its port up first if need be; the port
/// it used, to keep.
pub fn send(r: &Remote, c: Control) -> io::Result<u16> {
    let port = match r.port {
        Some(p) => p,
        None => browse(&r.id)?,
    };
    let ip: IpAddr = r.ip.parse().map_err(|_| io::Error::other(format!("not an address: {}", r.ip)))?;
    let status = request(SocketAddr::new(ip, port), command(c), &r.token)?;
    // 204 No Content is the usual answer; anything 2xx is taken.
    if !(200..300).contains(&status) {
        return Err(io::Error::other(format!("the sender answered {status}")));
    }
    Ok(port)
}

/// One DACP request; the answer's status code.
pub fn request(to: SocketAddr, command: &str, token: &str) -> io::Result<u16> {
    let mut s = TcpStream::connect_timeout(&to, TIMEOUT)?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    let host = match to {
        SocketAddr::V4(a) => format!("{}:{}", a.ip(), a.port()),
        SocketAddr::V6(a) => format!("[{}]:{}", a.ip(), a.port()),
    };
    write!(s, "GET /ctrl-int/1/{command} HTTP/1.1\r\nHost: {host}\r\nActive-Remote: {token}\r\n\r\n")?;
    s.flush()?;
    // The status line is all that's wanted.
    let mut head = Vec::new();
    let mut b = [0u8; 256];
    while !head.contains(&b'\n') && head.len() < 1024 {
        match s.read(&mut b)? {
            0 => break,
            n => head.extend_from_slice(&b[..n]),
        }
    }
    let line = String::from_utf8_lossy(&head);
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .filter(|_| line.starts_with("HTTP/"))
        .ok_or_else(|| io::Error::other("not an HTTP answer"))
}

/// The sender's DACP port, from avahi-browse.
fn browse(id: &str) -> io::Result<u16> {
    let out = Command::new("avahi-browse")
        .args(["--parsable", "--resolve", "--terminate", "--no-db-lookup", "_dacp._tcp"])
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("avahi-browse (avahi-utils), to find the sender's port: {e}")))?;
    port_in(&String::from_utf8_lossy(&out.stdout), id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no iTunes_Ctrl_{id} announced")))
}

/// The DACP-IDs `a` and `b` are the same: shairport-sync ignores case and
/// leading zeros, which some senders drop from one or the other.
fn same_id(a: &str, b: &str) -> bool {
    a.trim_start_matches('0').eq_ignore_ascii_case(b.trim_start_matches('0'))
}

/// The port avahi-browse's parsable output gives for `iTunes_Ctrl_<id>`:
/// resolved services are lines
/// `=;IFACE;PROTO;NAME;TYPE;DOMAIN;HOST;ADDRESS;PORT;TXT`.
pub fn port_in(browse: &str, id: &str) -> Option<u16> {
    browse.lines().find_map(|l| {
        let f: Vec<&str> = l.split(';').collect();
        let name = f.get(3)?.strip_prefix("iTunes_Ctrl_")?;
        (f[0] == "=" && same_id(name, id)).then(|| f.get(8)?.parse().ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn ports_from_avahi_browse() {
        let out = "+;eth0;IPv4;iTunes_Ctrl_1A2B3C4D5E6F7081;_dacp._tcp;local\n\
                   =;eth0;IPv4;iTunes_Ctrl_FFFF000011112222;_dacp._tcp;local;other.local;192.0.2.11;3690;\"txcv=1\"\n\
                   =;eth0;IPv4;iTunes_Ctrl_01A2B3C4D5E6F7081;_dacp._tcp;local;phone.local;192.0.2.10;3689;\"txtvers=1\"\n";
        assert_eq!(port_in(out, "1a2b3c4d5e6f7081"), Some(3689));
        assert_eq!(port_in(out, "FFFF000011112222"), Some(3690));
        assert_eq!(port_in(out, "1234"), None);
        assert_eq!(port_in("", "1234"), None);
    }

    /// A sender's DACP server that takes one request: what it was asked,
    /// answered with `status`.
    fn sender(status: &str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = l.local_addr().unwrap();
        let answer = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut b = [0u8; 512];
            while !req.ends_with(b"\r\n\r\n") {
                let n = s.read(&mut b).unwrap();
                req.extend_from_slice(&b[..n]);
            }
            s.write_all(answer.as_bytes()).unwrap();
            String::from_utf8(req).unwrap()
        });
        (at, t)
    }

    #[test]
    fn a_command_goes_to_the_sender() {
        let (at, t) = sender("204 No Content");
        let r = Remote { ip: "127.0.0.1".into(), id: "1A2B".into(), token: "1234567890".into(), port: Some(at.port()) };
        assert_eq!(send(&r, Control::Next).unwrap(), at.port());
        let req = t.join().unwrap();
        assert!(req.starts_with("GET /ctrl-int/1/nextitem HTTP/1.1\r\n"), "{req}");
        assert!(req.contains(&format!("\r\nHost: 127.0.0.1:{}\r\n", at.port())), "{req}");
        assert!(req.contains("\r\nActive-Remote: 1234567890\r\n"), "{req}");
    }

    #[test]
    fn a_refusal_is_an_error() {
        let (at, t) = sender("403 Forbidden");
        let r = Remote { ip: "127.0.0.1".into(), id: "1A2B".into(), token: "0".into(), port: Some(at.port()) };
        assert!(send(&r, Control::PlayPause).is_err());
        t.join().unwrap();
    }

    #[test]
    fn commands() {
        assert_eq!(command(Control::PlayPause), "playpause");
        assert_eq!(command(Control::Next), "nextitem");
        assert_eq!(command(Control::Previous), "previtem");
        assert_eq!(command(Control::Pause), "pause");
        assert_eq!(command(Control::Play), "play");
        assert!(!Remote::default().ready());
    }
}
