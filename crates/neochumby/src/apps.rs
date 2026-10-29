//! The apps in the tree: each a directory apps/<name>/ holding a `manifest`
//! of key=value lines, file names relative to the app's directory:
//!
//!   title=DOOM                 listed in app lists; opens `exec`
//!   exec=neochumby-doom        runs full-screen, in front, when opened
//!   colour=dc281e              (its colour, hex RGB; optional)
//!   service=neochumby-airplay  runs in the background while neochumby does
//!   companion=...              its server half, which neochumbyd runs
//!   neochumbyd=required        needs a paired neochumbyd (or `optional`:
//!                              uses one when there is one; absent: never)
//!   start=true                 may be the start app (see the session)
//!   interrupt=false            no service may replace it in front (games)
//!   button=true                gets the top button while in front (games)
//!   access=screen,touch        what it reaches beyond its jail (absent:
//!                              nothing; neochumby-manifest lists the words)
//!
//! An app may have any of these. The session (neochumby) and the apps
//! agree on the tree through NEOCHUMBY_ROOT.

use std::path::{Path, PathBuf};

use crate::conf;
use crate::fb::rgb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Needs {
    Nothing,
    /// Uses a neochumbyd when the Chumby is paired with one.
    Optional,
    /// Only on a Chumby paired with a neochumbyd.
    Required,
}

#[derive(Debug)]
pub struct App {
    /// apps/<name>/ in the tree; `name` is how neochumbyd knows it.
    pub dir: PathBuf,
    pub name: String,
    pub title: Option<String>,
    pub exec: Option<PathBuf>,
    pub colour: u16,
    pub service: Option<PathBuf>,
    /// It has a companion: the session opens it for the app
    /// (neochumby::companion).
    pub companion: bool,
    pub needs: Needs,
    /// It may be the start app: it promises to open other apps and CHUMBY
    /// MODE through the session, to keep running, and to handle the top
    /// button (a `button` line on its pipe, or NEOCHUMBY_REASON=button).
    pub start: bool,
    /// While it's in front, the start app's service may replace it (unless
    /// its manifest says `interrupt=false`).
    pub interrupt: bool,
    /// While it's in front, it has the top button (neochumby::input::Squeeze),
    /// and a press doesn't return to the start app.
    pub button: bool,
    /// What it reaches beyond its jail.
    pub access: Access,
}

/// An app's `access`: what the session gives it beyond its own directory
/// and a /tmp of its own. Each is false unless the manifest lists it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Access {
    /// /dev/fb0, for its exec only.
    pub screen: bool,
    /// The touchscreen and its calibration, for its exec only.
    pub touch: bool,
    /// /dev/dsp and the speaker's handover.
    pub speaker: bool,
    /// The accelerometer.
    pub accel: bool,
    /// /proc, /sys and /etc.
    pub system: bool,
    /// Internet sockets, and /etc/resolv.conf and /etc/hosts.
    pub network: bool,
    /// /dev/ptmx and /dev/pts.
    pub terminal: bool,
    /// The Chumby's logs, read-only.
    pub logs: bool,
    /// No jail: root, as the session.
    pub root: bool,
}

impl Access {
    /// From the manifest's comma-separated value; a word it doesn't know
    /// grants nothing.
    pub fn parse(s: &str) -> Access {
        let mut a = Access::default();
        for w in s.split(',').map(str::trim) {
            match w {
                "screen" => a.screen = true,
                "touch" => a.touch = true,
                "speaker" => a.speaker = true,
                "accel" => a.accel = true,
                "system" => a.system = true,
                "network" => a.network = true,
                "terminal" => a.terminal = true,
                "logs" => a.logs = true,
                "root" => a.root = true,
                _ => {}
            }
        }
        a
    }
}

impl App {
    /// Whether it can run here: what it needs of a neochumbyd it has.
    pub fn runnable(&self) -> bool {
        self.needs != Needs::Required || conf::paired().is_some()
    }
}

fn parse_colour(s: &str) -> Option<u16> {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
    Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn load(dir: &Path) -> Option<App> {
    let manifest = std::fs::read_to_string(dir.join("manifest")).ok()?;
    let field = |k: &str| {
        manifest
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(key, _)| key.trim() == k)
            .map(|(_, v)| v.trim().to_string())
    };
    let file = |k: &str| field(k).filter(|f| !f.is_empty()).map(|f| dir.join(f));
    Some(App {
        dir: dir.to_path_buf(),
        name: dir.file_name()?.to_string_lossy().into_owned(),
        title: field("title"),
        exec: file("exec"),
        colour: field("colour").and_then(|c| parse_colour(&c)).unwrap_or(rgb(220, 160, 40)),
        service: file("service"),
        companion: field("companion").is_some_and(|c| !c.is_empty()),
        needs: match field("neochumbyd").as_deref() {
            Some("required") => Needs::Required,
            Some("optional") => Needs::Optional,
            _ => Needs::Nothing,
        },
        start: field("start").as_deref() == Some("true"),
        interrupt: field("interrupt").as_deref() != Some("false"),
        button: field("button").as_deref() == Some("true"),
        access: field("access").map(|a| Access::parse(&a)).unwrap_or_default(),
    })
}

/// The tree: NEOCHUMBY_ROOT, which the session sets for what it runs, or
/// the directory of the running binary (the session's own).
pub fn root() -> PathBuf {
    std::env::var_os("NEOCHUMBY_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| "/mnt/neochumby".into())
}

/// This program's own binary, to run it again (on a terminal, say): /proc's
/// link to it, else its absolute argv[0], as the session starts apps. A
/// jail without `system` has no /proc.
pub fn exe() -> Option<PathBuf> {
    std::env::current_exe().ok().or_else(|| {
        let p = PathBuf::from(std::env::args_os().next()?);
        p.is_absolute().then_some(p)
    })
}

/// Every app in the tree, in name order.
pub fn all(root: &Path) -> Vec<App> {
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(root.join("apps")).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    dirs.sort();
    dirs.iter().filter_map(|d| load(d)).collect()
}
