//! The apps' services: each runnable app's `service`, run in the background
//! for as long as the session runs, as child.rs describes, with a request
//! pipe of its own.
//!
//! A service that exits is started again: after 1 s, then 2, 4 and so on up
//! to a minute. A run of a minute or more starts the wait again from 1 s.
//! Each service is told which app is in front (`front <app>`) as it starts
//! and whenever that changes; if its pipe is full, only the latest waits.
//! When the session ends, each gets SIGTERM, with its process group, and
//! SIGKILL if it hasn't exited after 5 s.
//!
//! Which services run is decided as the session starts, from whether this
//! Chumby is paired (an app that needs a neochumbyd). Pairing asks for a
//! restart anyway, so that's when a newly paired Chumby starts them.
//!
//! The session's own services (core: neochumby-sync, for delivery=sync)
//! are run, restarted and stopped the same way, but open no app: they are
//! never told what is in front, and what they ask for is dropped, so the
//! interruption rules never come into it.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use neochumby::apps::App;
use neochumby::session::{Notice, Pipe, Request};

use neochumby_sys::jail::Process;
use neochumby_sys::process::{Signal, exited, kill_group};

use crate::child;
use crate::jail::{self, Role};

/// The waits between a service's runs.
#[derive(Clone, Copy)]
pub struct Backoff {
    first: Duration,
    longest: Duration,
    wait: Duration,
}

impl Backoff {
    pub fn new(first: Duration, longest: Duration) -> Backoff {
        Backoff { first, longest, wait: first }
    }

    /// The wait after a run that lasted `ran`.
    pub fn after(&mut self, ran: Duration) -> Duration {
        if ran >= self.longest {
            self.wait = self.first;
        }
        let w = self.wait;
        self.wait = (w * 2).min(self.longest);
        w
    }
}

struct Running {
    child: Process,
    pipe: Pipe,
    since: Instant,
    /// The app in front, if this service hasn't been told yet: its pipe
    /// was full. Only the latest is kept, and sent once there's room.
    untold: Option<String>,
}

struct Service<'a> {
    app: &'a App,
    exe: &'a Path,
    running: Option<Running>,
    backoff: Backoff,
    /// When to start it, while it isn't running.
    due: Instant,
    /// The session's own, not an app's.
    core: bool,
}

/// What happened to the services since the last look.
#[derive(Default)]
pub struct Events<'a> {
    /// The apps whose services exited, with each one's pid.
    pub died: Vec<(&'a App, u32)>,
    /// What each service asked for, in order.
    pub requests: Vec<(&'a App, Request)>,
}

pub struct Services<'a> {
    list: Vec<Service<'a>>,
    root: &'a Path,
}

impl<'a> Services<'a> {
    /// The service of each app in `apps` that can run here, and each of
    /// `core`, the session's own; none started yet.
    pub fn new(apps: &'a [App], core: &'a [App], root: &'a Path, backoff: Backoff) -> Services<'a> {
        let now = Instant::now();
        let ours = |a: &'a App, core| {
            Some(Service { app: a, exe: a.service.as_deref()?, running: None, backoff, due: now, core })
        };
        let list = apps
            .iter()
            .filter(|a| a.runnable())
            .filter_map(|a| ours(a, false))
            .chain(core.iter().filter_map(|a| ours(a, true)))
            .collect();
        Services { list, root }
    }

    /// Starts each service that is due, with `front` as the app in front;
    /// takes in what the running ones asked for, and notes those that
    /// exited.
    pub fn poll(&mut self, front: &str) -> Events<'a> {
        let mut ev = Events::default();
        for s in &mut self.list {
            let Some(r) = s.running.as_mut() else {
                if Instant::now() >= s.due {
                    s.start(self.root, front);
                }
                continue;
            };
            // The rest of a line its pipe had no room for, then a notice
            // that waited.
            r.pipe.flush();
            if let Some(front) = r.untold.take() {
                r.tell(s.app, &front);
            }
            // Exited, but not reaped yet, so its group can still be ended
            // safely (child.rs).
            let gone = exited(r.child.id());
            for line in r.pipe.receive().lines {
                match Request::parse(&line) {
                    Some(req) if !s.core => ev.requests.push((s.app, req)),
                    _ => eprintln!("neochumby: service {}: ignoring {line:?}", s.app.name),
                }
            }
            if let Ok(false) = gone {
                continue;
            }
            let mut r = s.running.take().expect("running");
            let pid = r.child.id();
            // What it started goes with it.
            let status = child::end(&mut r.child, Duration::ZERO);
            let wait = s.backoff.after(r.since.elapsed());
            eprintln!("neochumby: service {} exited ({status:?}); again in {} ms", s.app.name, wait.as_millis());
            s.due = Instant::now() + wait;
            ev.died.push((s.app, pid));
        }
        ev
    }

    /// Ends each running service whose app lists no `network` but whose
    /// uid is among `owners` (network.rs). The next poll finds it exited,
    /// and starts it again later, as any service that exits.
    pub fn end_offline(&mut self, owners: &HashSet<u32>) {
        for s in &self.list {
            if let Some(r) = &s.running
                && jail::offline(s.app).is_some_and(|n| owners.contains(&n))
            {
                eprintln!(
                    "neochumby: service {} has an internet socket open, without network access; ending it",
                    s.app.name
                );
                let _ = kill_group(r.child.id(), Signal::Kill);
            }
        }
    }

    /// Tells each running service that `app` is now in front.
    pub fn front(&mut self, app: &str) {
        for s in self.list.iter_mut().filter(|s| !s.core) {
            if let Some(r) = s.running.as_mut() {
                r.tell(s.app, app);
            }
        }
    }

    /// Stops every service: SIGTERM to each one's process group, then
    /// SIGKILL to those still running after `patience`.
    pub fn stop(&mut self, patience: Duration) {
        let mut running: Vec<Running> = self.list.iter_mut().filter_map(|s| s.running.take()).collect();
        for r in &running {
            let _ = kill_group(r.child.id(), Signal::Term);
        }
        let deadline = Instant::now() + patience;
        while running.iter().any(|r| matches!(exited(r.child.id()), Ok(false))) && Instant::now() < deadline {
            neochumby::sleep_ms(50);
        }
        for r in &mut running {
            let _ = child::end(&mut r.child, Duration::ZERO);
        }
    }
}

impl Service<'_> {
    fn start(&mut self, root: &Path, front: &str) {
        match child::launch(self.app, self.exe, Role::Service, root).spawn() {
            Ok((child, pipe)) => {
                eprintln!("neochumby: service {} started", self.app.name);
                let mut r = Running { child, pipe, since: Instant::now(), untold: None };
                if !self.core {
                    r.tell(self.app, front);
                }
                self.running = Some(r);
            }
            Err(e) => {
                let wait = self.backoff.after(Duration::ZERO);
                eprintln!(
                    "neochumby: service {}: {}: {e}; again in {} ms",
                    self.app.name,
                    self.exe.display(),
                    wait.as_millis()
                );
                self.due = Instant::now() + wait;
            }
        }
    }
}

impl Running {
    /// Tells it `front` is in front, or keeps that for later if its pipe
    /// is full: the session never waits on a service.
    fn tell(&mut self, service: &App, front: &str) {
        if self.pipe.try_send(&Notice::Front(front.to_string()).line()) {
            self.untold = None;
        } else {
            if self.untold.is_none() {
                eprintln!("neochumby: service {} isn't reading its pipe; its notices wait", service.name);
            }
            self.untold = Some(front.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff() {
        let s = Duration::from_secs;
        let mut b = Backoff::new(s(1), s(60));
        let waits: Vec<u64> = (0..8).map(|_| b.after(s(3)).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        // A run of a minute or more starts again from the first wait.
        assert_eq!(b.after(s(60)).as_secs(), 1);
        assert_eq!(b.after(s(0)).as_secs(), 2);
    }
}
