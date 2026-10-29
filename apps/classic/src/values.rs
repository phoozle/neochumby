//! Live device values: what the Chumby can tell about itself, read here and
//! drawn without waiting for the server to render them (`live` boxes and
//! panes keyed on `wifi`). The readers take a root directory so tests can
//! point them at copies of /proc; on the Chumby it is `/`. Each is a few
//! small reads and stops at the first error: a value it can't read is
//! missing, drawn as `?`.
//!
//! The formats are part of the manifest's contract with the feeder (its
//! glyph strips must cover them): see the live-values spec.

use std::path::Path;

/// The glyphs of a `livefont` strip, in strip order.
pub const CHARSET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz .:%/-?_()+,'#&@!";
pub const NGLYPHS: usize = 79;
/// The glyph drawn for a character outside the charset.
pub const UNKNOWN: usize = 68;
/// Values are cut to this many characters.
pub const MAXLEN: usize = 32;

/// The index of a character's glyph.
pub fn glyph(c: char) -> usize {
    CHARSET.find(c).unwrap_or(UNKNOWN)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Wifi,
    WifiPct,
    Ip,
    Ssid,
    Uptime,
    MemFree,
    StickFree,
    Firmware,
    Name,
}

pub const KEYS: [Key; 9] =
    [Key::Wifi, Key::WifiPct, Key::Ip, Key::Ssid, Key::Uptime, Key::MemFree, Key::StickFree, Key::Firmware, Key::Name];

impl Key {
    pub fn parse(s: &str) -> Option<Key> {
        KEYS.into_iter().find(|k| k.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Key::Wifi => "wifi",
            Key::WifiPct => "wifi_pct",
            Key::Ip => "ip",
            Key::Ssid => "ssid",
            Key::Uptime => "uptime",
            Key::MemFree => "mem_free",
            Key::StickFree => "stick_free",
            Key::Firmware => "firmware",
            Key::Name => "name",
        }
    }

    pub fn index(self) -> usize {
        KEYS.iter().position(|&k| k == self).unwrap_or(0)
    }

    /// The keys a pane can take as its variable: they have a number.
    pub fn numeric(self) -> bool {
        matches!(self, Key::Wifi | Key::WifiPct)
    }
}

/// A numeric value from its text: the digits it starts with; 0 if none.
pub fn number(text: Option<&str>) -> i32 {
    text.map_or(0, |t| {
        t.bytes().take_while(u8::is_ascii_digit).fold(0i32, |v, d| v.saturating_mul(10) + (d - b'0') as i32)
    })
}

/// Reads one value; None if it is missing.
pub fn read(root: &Path, key: Key) -> Option<String> {
    let v = match key {
        Key::Wifi => wireless(root).map(|(_, pct)| bars(pct).to_string()),
        Key::WifiPct => wireless(root).map(|(_, pct)| format!("{pct}%")),
        Key::Ip => {
            let iface = default_iface(root).or_else(|| wireless(root).map(|(i, _)| i))?;
            ipv4(&iface).map(|a| format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]))
        }
        Key::Ssid => essid(&wireless(root)?.0),
        Key::Uptime => {
            let s = std::fs::read_to_string(root.join("proc/uptime")).ok()?;
            let secs = s.split(['.', ' ']).next()?.trim().parse::<u64>().ok()?;
            Some(fmt_uptime(secs))
        }
        Key::MemFree => {
            let s = std::fs::read_to_string(root.join("proc/meminfo")).ok()?;
            let field = |k: &str| {
                s.lines().find_map(|l| l.strip_prefix(k)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok())
            };
            Some(fmt_mem(field("MemFree:")? + field("Buffers:")? + field("Cached:")?))
        }
        Key::StickFree => {
            let m = std::fs::read_to_string(root.join("proc/mounts")).ok()?;
            if !m.lines().any(|l| l.split_whitespace().nth(1) == Some("/mnt/usb")) {
                return None;
            }
            Some(fmt_stick(free_bytes(&root.join("mnt/usb"))?))
        }
        Key::Firmware => {
            let s = std::fs::read_to_string(root.join("etc/software_version")).ok()?;
            Some(s.lines().next()?.trim().to_string()).filter(|s| !s.is_empty())
        }
        Key::Name => Some(name(root)),
    }?;
    Some(v.chars().take(MAXLEN).collect())
}

/// The wireless interface and its link quality, 0..100, from
/// /proc/net/wireless: two header lines, then "  wlan0: 0000   54.  -45. ...".
fn wireless(root: &Path) -> Option<(String, i32)> {
    let s = std::fs::read_to_string(root.join("proc/net/wireless")).ok()?;
    let l = s.lines().nth(2)?;
    let (iface, rest) = l.split_once(':')?;
    let link = rest.split_whitespace().nth(1)?;
    let n: i32 = link.trim_end_matches('.').split('.').next()?.parse().ok()?;
    Some((iface.trim().to_string(), n.clamp(0, 100)))
}

pub fn bars(pct: i32) -> i32 {
    match pct {
        75.. => 4,
        50.. => 3,
        25.. => 2,
        1.. => 1,
        _ => 0,
    }
}

/// The default route's interface: /proc/net/route's first line with
/// destination 0 and the up flag.
fn default_iface(root: &Path) -> Option<String> {
    let s = std::fs::read_to_string(root.join("proc/net/route")).ok()?;
    s.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let up = f.get(3).and_then(|x| u32::from_str_radix(x, 16).ok()).is_some_and(|x| x & 1 != 0);
        (f.len() > 3 && f[1] == "00000000" && up).then(|| f[0].to_string())
    })
}

/// The interface's IPv4 address.
#[cfg(target_os = "linux")]
fn ipv4(iface: &str) -> Option<[u8; 4]> {
    neochumby_sys::net::ipv4_address(iface)
}

#[cfg(not(target_os = "linux"))]
fn ipv4(_iface: &str) -> Option<[u8; 4]> {
    None
}

/// The interface's ESSID.
#[cfg(target_os = "linux")]
fn essid(iface: &str) -> Option<String> {
    neochumby_sys::net::wireless_essid(iface).map(|e| String::from_utf8_lossy(&e).into_owned())
}

#[cfg(not(target_os = "linux"))]
fn essid(_iface: &str) -> Option<String> {
    None
}

/// Free bytes for an unprivileged user on the filesystem at `path`.
fn free_bytes(path: &Path) -> Option<u64> {
    neochumby_sys::fs::available_bytes(path).ok()
}

/// The Chumby's name, as neochumby::conf::name gives it.
fn name(root: &Path) -> String {
    ["psp/hostname", "proc/sys/kernel/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(root.join(p)).ok())
        .map(|raw| {
            raw.trim()
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .take(32)
                .collect::<String>()
        })
        .find(|n| !n.is_empty() && n != "_none_")
        .unwrap_or_else(|| "chumby".into())
}

pub fn fmt_uptime(s: u64) -> String {
    let (d, h, m) = (s / 86400, s / 3600 % 24, s / 60 % 60);
    if s < 3600 {
        format!("{m}m")
    } else if s < 86400 {
        format!("{h}h {m}m")
    } else {
        format!("{d}d {h}h")
    }
}

pub fn fmt_mem(kb: u64) -> String {
    let t = kb * 10 / 1024;
    format!("{}.{} MB", t / 10, t % 10)
}

pub fn fmt_stick(b: u64) -> String {
    if b >= 1 << 30 {
        let t = b * 10 / (1 << 30);
        format!("{}.{} GB", t / 10, t % 10)
    } else {
        format!("{} MB", b >> 20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("classic-values-{}-{}", std::process::id(), files.len()));
        let _ = std::fs::remove_dir_all(&d);
        for (p, c) in files {
            let f = d.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn charset() {
        assert_eq!(CHARSET.chars().count(), NGLYPHS);
        assert_eq!(glyph('0'), 0);
        assert_eq!(glyph('A'), 10);
        assert_eq!(glyph('a'), 36);
        assert_eq!(glyph(' '), 62);
        assert_eq!(glyph('?'), UNKNOWN);
        assert_eq!(glyph('!'), 78);
        assert_eq!(glyph('é'), UNKNOWN);
        assert_eq!(Key::parse("stick_free"), Some(Key::StickFree));
        assert_eq!(Key::parse("wx"), None);
        assert!(KEYS.iter().all(|k| Key::parse(k.name()) == Some(*k)));
    }

    #[test]
    fn from_canned_files() {
        let d = root(&[
            (
                "proc/net/wireless",
                "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
                 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 19\n  \
                 rausb0: 0000   54.  -45.  -95.       0      0      0      0      0        0\n",
            ),
            ("proc/uptime", "273654.21 270011.80\n"),
            (
                "proc/meminfo",
                "MemTotal:        61848 kB\nMemFree:         20480 kB\nBuffers:          1024 kB\nCached:          12390 kB\n",
            ),
            ("proc/mounts", "rootfs / rootfs rw 0 0\n/dev/sda1 /mnt/usb vfat rw 0 0\n"),
            ("etc/software_version", "1.7.2\nextra\n"),
            ("psp/hostname", "Kitchen Chumby!\n"),
            ("mnt/usb/x", ""),
        ]);
        assert_eq!(read(&d, Key::Wifi).as_deref(), Some("3"));
        assert_eq!(read(&d, Key::WifiPct).as_deref(), Some("54%"));
        assert_eq!(read(&d, Key::Uptime).as_deref(), Some("3d 4h"));
        assert_eq!(read(&d, Key::MemFree).as_deref(), Some("33.0 MB"));
        assert_eq!(read(&d, Key::Firmware).as_deref(), Some("1.7.2"));
        assert_eq!(read(&d, Key::Name).as_deref(), Some("kitchen_chumby_"));
        assert!(read(&d, Key::StickFree).is_some_and(|s| s.ends_with("GB") || s.ends_with("MB")));
        let _ = std::fs::remove_dir_all(&d);

        let e = root(&[("proc/mounts", "rootfs / rootfs rw 0 0\n"), ("proc/sys/kernel/hostname", "(none)\n")]);
        for k in [Key::Wifi, Key::WifiPct, Key::Ip, Key::Ssid, Key::Uptime, Key::MemFree, Key::StickFree, Key::Firmware]
        {
            assert_eq!(read(&e, k), None, "{} should be missing", k.name());
        }
        assert_eq!(read(&e, Key::Name).as_deref(), Some("chumby"));
        let _ = std::fs::remove_dir_all(&e);
    }

    #[test]
    fn routes() {
        let d = root(&[(
            "proc/net/route",
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
             rausb0\t0032A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\n\
             rausb0\t00000000\t0132A8C0\t0003\t0\t0\t0\t00000000\n",
        )]);
        assert_eq!(default_iface(&d).as_deref(), Some("rausb0"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_uptime(0), "0m");
        assert_eq!(fmt_uptime(42 * 60 + 59), "42m");
        assert_eq!(fmt_uptime(5 * 3600 + 7 * 60), "5h 7m");
        assert_eq!(fmt_uptime(3 * 86400 + 4 * 3600 + 59 * 60), "3d 4h");
        assert_eq!(fmt_mem(33894), "33.0 MB");
        assert_eq!(fmt_mem(33900), "33.1 MB");
        assert_eq!(fmt_mem(0), "0.0 MB");
        assert_eq!(fmt_stick(830 << 20), "830 MB");
        assert_eq!(fmt_stick((1 << 30) * 14 / 10), "1.3 GB");
        assert_eq!(fmt_stick(1 << 30), "1.0 GB");
        assert_eq!(
            [bars(0), bars(1), bars(24), bars(25), bars(50), bars(74), bars(75), bars(100)],
            [0, 1, 1, 2, 3, 3, 4, 4]
        );
        assert_eq!(number(Some("54%")), 54);
        assert_eq!(number(Some("?")), 0);
        assert_eq!(number(None), 0);
    }
}
