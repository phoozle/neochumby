//! The start app: what the session opens first, and goes back to when any
//! other app exits.
//!
//! It's named by `start=<app>` in this Chumby's settings from its
//! neochumbyd (neochumby::settings, kept in /psp for boots without it),
//! else in /psp/neochumby.conf. Only an app whose manifest says
//! `start = true` may be the start app: it promises a way to other apps and
//! to CHUMBY MODE, and leaves the top button to the session, so a Chumby
//! can't be left stuck in a game. A name that isn't one is refused, with
//! the reason logged, and the start app is `home` (neochumby-home), as it
//! is when nothing names one.

use neochumby::apps::App;

/// The built-in app list, apps/<this>: the start app when no other may be,
/// and what the escape hatch opens.
pub const HOME: &str = "home";

/// `name`, if it may be the start app here; else why not.
pub fn eligible<'a>(apps: &'a [App], name: &str) -> Result<&'a App, &'static str> {
    let app = apps.iter().find(|a| a.name == name).ok_or("no such app here")?;
    if !app.start {
        Err("its manifest doesn't say start = true")
    } else if app.button {
        Err("it says button = true, so the button couldn't bring it back")
    } else if app.exec.is_none() {
        Err("it has no exec")
    } else if !app.runnable() {
        Err("it needs a paired neochumbyd")
    } else {
        Ok(app)
    }
}

/// The start app: `choice` (where the name came from, and the name) if it
/// may be one, else `home`. None if there's no `home` either.
pub fn choose<'a>(apps: &'a [App], choice: Option<(&str, &str)>) -> Option<&'a App> {
    if let Some((from, name)) = choice {
        match eligible(apps, name) {
            Ok(app) => return Some(app),
            Err(why) => eprintln!("neochumby: start={name} ({from}) can't be the start app: {why}; {HOME} instead"),
        }
    }
    home(apps)
}

/// neochumby-home, if it's in the tree and can run.
pub fn home(apps: &[App]) -> Option<&App> {
    apps.iter().find(|a| a.name == HOME && a.exec.is_some() && a.runnable())
}

#[cfg(test)]
pub mod tests {
    use std::path::PathBuf;

    use neochumby::apps::{App, Needs};

    use super::*;

    /// An app for tests: `exec` is a binary named after it.
    pub fn app(name: &str, start: bool) -> App {
        App {
            dir: PathBuf::from("/nowhere").join(name),
            name: name.into(),
            title: Some(name.to_uppercase()),
            exec: Some(PathBuf::from("/nowhere").join(name).join(name)),
            colour: 0,
            service: None,
            needs: Needs::Nothing,
            start,
            interrupt: true,
            button: false,
            companion: false,
            access: Default::default(),
        }
    }

    #[test]
    fn only_a_start_app_starts() {
        let mut apps = vec![app("home", true), app("wx", true), app("doom", false), app("paired", true)];
        apps[3].needs = Needs::Required;
        let name = |a: Option<&App>| a.map(|a| a.name.clone());

        assert_eq!(name(choose(&apps, Some(("settings", "wx")))), Some("wx".into()));
        assert_eq!(name(choose(&apps, None)), Some("home".into()));
        // Refused: no start = true, not in the tree, or not runnable here
        // (no neochumbyd is paired on a host).
        assert_eq!(eligible(&apps, "doom").err(), Some("its manifest doesn't say start = true"));
        assert_eq!(eligible(&apps, "gone").err(), Some("no such app here"));
        assert_eq!(eligible(&apps, "paired").err(), Some("it needs a paired neochumbyd"));
        apps[1].button = true;
        assert!(eligible(&apps, "wx").is_err());
        apps[1].button = false;
        assert_eq!(name(choose(&apps, Some(("settings", "doom")))), Some("home".into()));
        assert_eq!(name(choose(&apps, Some(("settings", "gone")))), Some("home".into()));
        // No exec: not even home.
        apps[0].exec = None;
        assert_eq!(name(choose(&apps, Some(("settings", "doom")))), None);
    }
}
