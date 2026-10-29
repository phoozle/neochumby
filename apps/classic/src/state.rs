//! What the app and its service share: where the pictures are, the
//! settings kept in /psp (wx_music, wx_night...), and the night schedule.
//! The app decides night for what it draws; the service, for the backlight
//! while another app is in front. Both decide from the same files.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::scan::{atoi, lines, scan};

/// Opens a file without following a symbolic link at the end of its path.
pub fn open_nofollow(o: &mut OpenOptions) -> &mut OpenOptions {
    o.custom_flags(libc::O_NOFOLLOW)
}

pub fn read_nofollow(path: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = open_nofollow(OpenOptions::new().read(true)).open(path).ok()?;
    let mut b = Vec::new();
    f.take(cap + 1).read_to_end(&mut b).ok()?;
    (b.len() as u64 <= cap).then_some(b)
}

pub fn read_int(path: &Path, dflt: i32) -> i32 {
    match read_nofollow(path, 64) {
        Some(b) if !b.is_empty() => atoi(&b[..b.len().min(11)]),
        _ => dflt,
    }
}

/// Writes a setting whole or not at all: `name.tmp`, made new (never
/// through a link), then renamed over the setting. Whoever reads it (the
/// service, every half second) sees the old value or the new one, never an
/// empty file, and a power cut keeps one of them.
pub fn write_setting(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover from a run that died mid-write (removing a link removes
    // the link, not what it points at).
    let _ = std::fs::remove_file(&tmp);
    let mut f = open_nofollow(OpenOptions::new().write(true).create_new(true).mode(0o644)).open(&tmp)?;
    let r = f.write_all(data).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

pub fn save_int(path: &Path, v: i32) {
    let _ = write_setting(path, format!("{v}\n").as_bytes());
}

pub fn mtime(m: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    m.mtime()
}

/// Where the current set lives: base/<word in base/current>, else base.
pub fn resolve_dir(base: &Path) -> PathBuf {
    if let Ok(cur) = std::fs::read(base.join("current"))
        && cur.len() <= 64
    {
        let w: String = cur
            .iter()
            .map(|&c| c as char)
            .skip_while(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_'))
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
            .take(16)
            .collect();
        if !w.is_empty() && base.join(&w).join("manifest.txt").exists() {
            return base.join(w);
        }
    }
    if base.join("manifest.txt").exists() {
        return base.to_path_buf();
    }
    // `current` missing or broken (a power cut mid-switch): any complete set.
    for s in ["a", "b"] {
        if base.join(s).join("manifest.txt").exists() {
            return base.join(s);
        }
    }
    base.to_path_buf()
}

/// The private directory for the control and stats files, and the
/// service's record of what it plays.
pub const RUN_DIR: &str = "/tmp/neochumby-classic";

/// [`RUN_DIR`], made (or kept) a real directory of ours, mode 0700; None if
/// it can't be (another user's, or a link).
pub fn run_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let d = Path::new(RUN_DIR);
    let _ = std::fs::DirBuilder::new().mode(0o700).create(d);
    let ok = std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir() && m.uid() == neochumby_sys::process::uid());
    if !ok || std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).is_err() {
        return None;
    }
    Some(d.to_path_buf())
}

/// A setting that is a time (seconds since the epoch); 0 if unset.
pub fn read_time(path: &Path) -> i64 {
    read_nofollow(path, 64).and_then(|b| std::str::from_utf8(&b).ok()?.trim().parse().ok()).unwrap_or(0)
}

pub fn save_time(path: &Path, t: i64) {
    let _ = write_setting(path, format!("{t}\n").as_bytes());
}

/// Whether it is dark (in the feeder's town): 1 or 0; -1 without sun data.
pub fn sun_dark(sun: &[(i64, i64)], now: i64) -> i32 {
    if sun.is_empty() {
        return -1;
    }
    if sun.iter().any(|&(r, s)| now >= r && now < s) { 0 } else { 1 }
}

/// The next sunrise or sunset after `now`; 12 h on if the data has run out.
pub fn next_sun_change(sun: &[(i64, i64)], now: i64) -> i64 {
    let mut best = now + 12 * 3600;
    for &(r, s) in sun {
        if r > now && r < best {
            best = r;
        }
        if s > now && s < best {
            best = s;
        }
    }
    best
}

/// What night mode (0 auto, 1 on, 2 off) wants now, ignoring a NIGHT tap's
/// override: 1 or 0; -1 for no opinion (auto, without sun data).
pub fn night_wanted(mode: i32, sun: &[(i64, i64)], now: i64) -> i32 {
    match mode {
        1 => 1,
        2 => 0,
        _ => sun_dark(sun, now),
    }
}

/// Whether it is night, as the app decides it: a NIGHT tap's state
/// (`last`, wx_night) holds until `until` (wx_night_until); then the mode;
/// with no opinion, the last state.
pub fn night_now(mode: i32, sun: &[(i64, i64)], now: i64, until: i64, last: bool) -> bool {
    if now < until {
        return last;
    }
    match night_wanted(mode, sun, now) {
        -1 => last,
        w => w == 1,
    }
}

/// The manifest's sun and station lines (all the service needs of it), as
/// the manifest parser reads them.
pub fn feed(m: &[u8]) -> (Vec<(i64, i64)>, Vec<String>) {
    let mut sun = Vec::new();
    let mut stations: Vec<String> = Vec::new();
    for l in lines(m) {
        let v = scan(l, "sun %ld %ld");
        if v.len() == 2 && sun.len() < crate::manifest::MAXSUN {
            sun.push((v[0].l(), v[1].l()));
            continue;
        }
        let v = scan(l, "station %d %48s %255s");
        if v.len() == 3 && (0..crate::manifest::MAXST as i32).contains(&v[0].i()) {
            let k = v[0].i() as usize;
            if stations.len() <= k {
                stations.resize(k + 1, String::new());
            }
            stations[k] = v[2].s().to_string();
        }
    }
    (sun, stations)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUN: [(i64, i64); 2] = [(100, 200), (300, 400)];

    #[test]
    fn the_night_decision() {
        // Auto follows the sun.
        assert!(!night_now(0, &SUN, 150, 0, true));
        assert!(night_now(0, &SUN, 250, 0, false));
        // On and off hold whatever the sun does.
        assert!(night_now(1, &SUN, 150, 0, false));
        assert!(!night_now(2, &SUN, 250, 0, true));
        // A NIGHT tap's state holds until its end, then the mode decides.
        assert!(night_now(0, &SUN, 150, 180, true));
        assert!(!night_now(0, &SUN, 180, 180, true));
        assert!(!night_now(1, &SUN, 150, 300, false));
        // No sun data and auto: as last time.
        assert!(night_now(0, &[], 150, 0, true));
        assert!(!night_now(0, &[], 150, 0, false));
        assert_eq!(next_sun_change(&SUN, 150), 200);
        assert_eq!(next_sun_change(&SUN, 250), 300);
        assert_eq!(next_sun_change(&SUN, 450), 450 + 12 * 3600);
    }

    #[test]
    fn what_the_service_reads() {
        let (sun, st) =
            feed(b"page 0 p.raw 5\nsun 1 2\nstation 2 lush http://example.org/lush\nstation 99 x y\nsun 3 4\n");
        assert_eq!(sun, vec![(1, 2), (3, 4)]);
        assert_eq!(st, vec![String::new(), String::new(), "http://example.org/lush".into()]);
        let d = std::env::temp_dir().join(format!("classic-state-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        save_time(&d.join("t"), 1793178706);
        // Written whole, by a rename: nothing is left beside it.
        assert!(!d.join("t.tmp").exists());
        // A stale temporary file, even a link, is replaced, not followed.
        std::os::unix::fs::symlink(d.join("elsewhere"), d.join("t.tmp")).unwrap();
        save_time(&d.join("t"), 5);
        assert_eq!(read_time(&d.join("t")), 5);
        assert!(!d.join("elsewhere").exists());
        save_time(&d.join("t"), 1793178706);
        assert_eq!(read_time(&d.join("t")), 1793178706);
        assert_eq!(read_time(&d.join("missing")), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
