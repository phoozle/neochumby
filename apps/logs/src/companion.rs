//! The logs app's companion: its server half, which neochumbyd runs for
//! each Chumby that opens it (the Chumby's half is main.rs). It serves the
//! host's logs as its owner lists them (neochumby_logs::config): it sends
//! their names, then follows whichever the Chumby asks for, a file as
//! `tail -f` would (from its last 64 KB, starting again if it is truncated
//! or replaced) or a command's output, stopping the last. The frames are
//! in lib.rs.
//!
//! From the environment: NEOCHUMBY_CHUMBY and NEOCHUMBY_STATE
//! (neochumbyd's). Exits when its stdin closes: the Chumby has gone.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use neochumby_logs::config::{self, Feed, Source};
use neochumby_logs::frame as tag;
use neochumby_logs::tail::Tail;
use neochumby_proto::{frame, valid_name};

/// How often a file is looked at for more.
const POLL: Duration = Duration::from_millis(500);

enum Event {
    /// The Chumby asks for log i.
    Show(u8),
    /// More of the log that this follower (its number) follows.
    Text(u64, Vec<u8>),
    /// The Chumby has gone.
    Gone,
}

/// A follower, told to stop when dropped.
struct Following(Arc<AtomicBool>);

impl Drop for Following {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Follows a file until `stop`.
fn follow_file(path: &Path, stop: &AtomicBool, send: &dyn Fn(Vec<u8>) -> bool) {
    let mut tail = Tail::new(path);
    while !stop.load(Ordering::Relaxed) {
        let more = tail.poll();
        if more.is_empty() {
            std::thread::sleep(POLL);
        } else if !send(more) {
            return;
        }
    }
}

/// Runs `cmd` with sh until `stop` or until it exits, sending its output.
fn follow_command(cmd: &str, stop: &AtomicBool, send: &(dyn Fn(Vec<u8>) -> bool + Sync)) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1; {cmd}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let mut child: Child = match child {
        Ok(c) => c,
        Err(e) => {
            send(format!("logs: {cmd}: {e}\n").into_bytes());
            return;
        }
    };
    let mut out = child.stdout.take().expect("piped");
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut buf = vec![0u8; 16 * 1024];
            while let Ok(n @ 1..) = out.read(&mut buf) {
                if !send(buf[..n].to_vec()) {
                    break;
                }
            }
        });
        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                if !status.success() {
                    send(format!("logs: {cmd}: {status}\n").into_bytes());
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.wait();
    });
}

/// Starts following `source` as follower `n`.
fn follow(source: &Source, n: u64, tx: &Sender<Event>) -> Following {
    let stop = Arc::new(AtomicBool::new(false));
    let (feed, stopping, tx) = (source.feed.clone(), stop.clone(), tx.clone());
    std::thread::spawn(move || {
        let send = move |b: Vec<u8>| tx.send(Event::Text(n, b)).is_ok();
        match feed {
            Feed::File(p) => follow_file(&p, &stopping, &send),
            Feed::Command(c) => follow_command(&c, &stopping, &send),
        }
    });
    Following(stop)
}

fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

fn run() -> io::Result<()> {
    let name = std::env::var("NEOCHUMBY_CHUMBY")
        .ok()
        .filter(|n| valid_name(n))
        .ok_or_else(|| io::Error::other("no NEOCHUMBY_CHUMBY"))?;
    let state =
        PathBuf::from(std::env::var_os("NEOCHUMBY_STATE").ok_or_else(|| io::Error::other("no NEOCHUMBY_STATE"))?);
    let sources = config::sources(&state, &name, Path::exists, on_path);
    let help = sources.is_empty().then(|| config::help(&state));
    let names: Vec<&str> =
        if help.is_some() { vec!["setup"] } else { sources.iter().map(|s| s.name.as_str()).collect() };
    eprintln!("neochumby-logs: {} for {name}", names.join(", "));

    let mut out = io::stdout().lock();
    let mut f = vec![tag::SOURCES];
    f.extend_from_slice(names.join("\n").as_bytes());
    frame::write(&mut out, &f)?;
    out.flush()?;

    let (tx, rx) = mpsc::channel();
    let from_chumby = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(f)) = frame::read(&mut stdin) {
            if let [tag::SHOW, i] = f[..] {
                let _ = from_chumby.send(Event::Show(i));
            }
        }
        let _ = from_chumby.send(Event::Gone);
    });

    let mut following: Option<Following> = None;
    let mut n = 0u64;
    loop {
        match rx.recv() {
            Ok(Event::Show(i)) => {
                following = None;
                n += 1;
                frame::write(&mut out, &[tag::START, i])?;
                if let Some(h) = &help {
                    let mut f = vec![tag::TEXT];
                    f.extend_from_slice(h.as_bytes());
                    frame::write(&mut out, &f)?;
                } else if let Some(s) = sources.get(i as usize) {
                    following = Some(follow(s, n, &tx));
                }
            }
            Ok(Event::Text(of, text)) if of == n && following.is_some() => {
                for chunk in text.chunks(frame::MAX_FRAME - 1) {
                    let mut f = Vec::with_capacity(1 + chunk.len());
                    f.push(tag::TEXT);
                    f.extend_from_slice(chunk);
                    frame::write(&mut out, &f)?;
                }
            }
            Ok(Event::Text(..)) => {}
            Ok(Event::Gone) | Err(_) => return Ok(()),
        }
        out.flush()?;
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("neochumby-logs companion: {e}");
        std::process::exit(1);
    }
}
