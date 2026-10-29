//! The host's logs, as the companion serves them. Its owner lists them in
//! the companion's state directory (neochumbyd's <state>/apps/logs):
//! `sources.<chumby>` for one Chumby, else `sources` for all, a log a line:
//!
//!   # name=file, or name=|command (run with sh; its output is followed)
//!   syslog=/var/log/syslog
//!   neochumbyd=|journalctl -f -n 500 -u neochumbyd
//!
//! Without either, the system's own logs, if it has them: /var/log/syslog
//! or /var/log/messages, and the journal where journalctl runs.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feed {
    File(PathBuf),
    Command(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub feed: Feed,
}

/// The longest name shown.
pub const NAME: usize = 24;

/// The sources a `sources` file lists; lines it can't read are skipped.
pub fn parse(s: &str) -> Vec<Source> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter_map(|(name, what)| {
            let (name, what) = (name.trim(), what.trim());
            if name.is_empty() || name.chars().count() > NAME || what.is_empty() {
                return None;
            }
            let feed = match what.strip_prefix('|') {
                Some(cmd) => Feed::Command(cmd.trim().to_string()),
                None => Feed::File(PathBuf::from(what)),
            };
            Some(Source { name: name.to_string(), feed })
        })
        // At most 255: a log is a byte in the frames.
        .take(255)
        .collect()
}

/// `chumby`'s sources: its own file, else the shared one, else the
/// system's logs (`exists` and `runs` say what this host has).
pub fn sources(state: &Path, chumby: &str, exists: impl Fn(&Path) -> bool, runs: impl Fn(&str) -> bool) -> Vec<Source> {
    for f in [format!("sources.{chumby}"), "sources".to_string()] {
        if let Ok(s) = std::fs::read_to_string(state.join(f)) {
            return parse(&s);
        }
    }
    let mut v: Vec<Source> = ["syslog", "messages"]
        .into_iter()
        .map(|n| (n, PathBuf::from("/var/log").join(n)))
        .filter(|(_, p)| exists(p))
        .map(|(n, p)| Source { name: n.into(), feed: Feed::File(p) })
        .collect();
    if runs("journalctl") {
        v.push(Source { name: "journal".into(), feed: Feed::Command("journalctl -f -n 1000 --no-pager".into()) });
    }
    v
}

/// What the Chumby shows for a host with nothing to serve.
pub fn help(state: &Path) -> String {
    format!(
        "No logs are set up for this Chumby on the server.\n\
         List them in {}/sources (or sources.<chumby>), one a line:\n\
         \n  name=/path/to/file\n  name=|command that prints a log\n",
        state.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_files_and_commands() {
        let s = parse(
            "# a comment\n\
             syslog = /var/log/syslog\n\
             web=|tail -f /srv/web.log\n\
             \n\
             no equals sign\n\
             =/nameless\n\
             empty=\n\
             a-name-that-is-far-too-long-to-show=/x\n",
        );
        assert_eq!(
            s,
            [
                Source { name: "syslog".into(), feed: Feed::File("/var/log/syslog".into()) },
                Source { name: "web".into(), feed: Feed::Command("tail -f /srv/web.log".into()) },
            ]
        );
    }

    #[test]
    fn a_chumby_s_own_file_comes_first() {
        let dir = std::env::temp_dir().join(format!("neochumby-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let none = |_: &Path| false;
        let never = |_: &str| false;
        assert_eq!(sources(&dir, "den", none, never), []);
        std::fs::write(dir.join("sources"), "all=/a\n").unwrap();
        assert_eq!(sources(&dir, "den", none, never)[0].name, "all");
        std::fs::write(dir.join("sources.den"), "den=/d\n").unwrap();
        assert_eq!(sources(&dir, "den", none, never)[0].name, "den");
        assert_eq!(sources(&dir, "hall", none, never)[0].name, "all");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_file_the_system_s_logs() {
        let dir = Path::new("/nonexistent");
        let s = sources(dir, "den", |p| p == Path::new("/var/log/messages"), |c| c == "journalctl");
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["messages", "journal"]);
    }
}
