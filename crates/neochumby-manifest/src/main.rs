//! neochumby-manifest: an app crate's `[package.metadata.neochumby]`, as
//! the `manifest` of key=value lines a Chumby's tree holds for it (read by
//! the session, the apps and neochumbyd). Run on the host by
//! tools/tree.
//!
//!   neochumby-manifest path/to/Cargo.toml
//!
//! Prints the manifest, or fails with what is wrong: a crate not named
//! neochumby-<name>, no metadata, an unknown key, or a value of the wrong
//! kind. Values are strings, except `start`, `button` and `interrupt`,
//! which are true or false, and `access`, a list of what the app reaches
//! beyond its jail ([`ACCESS`]), written as one comma-separated value.
//! `neochumbyd` is `required` or `optional`
//! (absent: the app never needs one), and an app with a `companion` must
//! set it: only a neochumbyd runs companions. It is stated, not inferred
//! from `companion`.

use std::process::ExitCode;

use toml::Value;

/// The keys an app may set, as apps.rs reads them on the Chumby.
const KEYS: &[&str] =
    &["title", "exec", "colour", "service", "companion", "neochumbyd", "start", "button", "interrupt", "access"];
/// The keys among them that are true or false:
/// - `start`: the app may be the start app;
/// - `button`: while in front, it gets the top button;
/// - `interrupt`: false if no service may replace it in front.
const FLAGS: &[&str] = &["start", "button", "interrupt"];

/// What an app may list in `access`, as the session grants it (jail.rs);
/// without it, an app has only its own directory and a /tmp of its own:
/// - `screen`: /dev/fb0, for its `exec` (in front), never its service;
/// - `touch`: the touchscreen and its calibration, likewise;
/// - `speaker`: /dev/dsp and the speaker's handover; the mute and the
///   volume are the session's, asked for over the request pipe;
/// - `accel`: the accelerometer;
/// - `system`: /proc, /sys and /etc, as the app's own user sees them;
/// - `network`: the names it needs to look hosts up (/etc/resolv.conf and
///   /etc/hosts); without it, the session ends an app it finds with an
///   internet socket open;
/// - `terminal`: /dev/ptmx and /dev/pts, for a pseudo-terminal
///   (neochumby::term);
/// - `logs`: the Chumby's logs, read-only: the session's, /tmp's *.log
///   and /var/log's files;
/// - `root`: no jail: it runs as root, as the session does. Listed alone.
const ACCESS: &[&str] = &["screen", "touch", "speaker", "accel", "system", "network", "terminal", "logs", "root"];

/// What a value may hold: a manifest line is `key=value`, one per line.
fn plain(s: &str) -> bool {
    !s.is_empty() && !s.contains(['\n', '\r', '='])
}

/// `access`'s list, checked, as one value.
fn access(list: &[Value]) -> Result<String, String> {
    let mut words: Vec<&str> = Vec::new();
    for v in list {
        match v.as_str() {
            Some(w) if words.contains(&w) => return Err(format!("access: {w} twice")),
            Some(w) if ACCESS.contains(&w) => words.push(w),
            _ => return Err(format!("access: {v} is not one of {}", ACCESS.join(", "))),
        }
    }
    if words.contains(&"root") && words.len() > 1 {
        return Err("access: root is everything already; list it alone".into());
    }
    if words.is_empty() {
        return Err("access: an empty list; leave it out".into());
    }
    Ok(words.join(","))
}

fn manifest(cargo_toml: &str) -> Result<String, String> {
    let doc: Value = cargo_toml.parse().map_err(|e| format!("not TOML: {e}"))?;
    let package = doc.get("package").ok_or("no [package]")?;
    let name = package.get("name").and_then(Value::as_str).ok_or("no package name")?;
    if !name.starts_with("neochumby-") {
        return Err(format!("{name}: an app's crate is named neochumby-<name>"));
    }
    let meta = package
        .get("metadata")
        .and_then(|m| m.get("neochumby"))
        .and_then(Value::as_table)
        .ok_or("no [package.metadata.neochumby]")?;

    let mut out = String::new();
    for key in KEYS {
        let Some(v) = meta.get(*key) else { continue };
        let value = match (FLAGS.contains(key), v) {
            (false, Value::Array(a)) if *key == "access" => access(a)?,
            (false, _) if *key == "access" => return Err("access: not a list of strings".into()),
            (false, Value::String(s)) => s.clone(),
            (false, _) => return Err(format!("{key}: not a string")),
            (true, Value::Boolean(b)) => b.to_string(),
            (true, _) => return Err(format!("{key}: not true or false")),
        };
        if !plain(&value) {
            return Err(format!("{key}: {value:?} is not a plain value"));
        }
        out.push_str(&format!("{key}={value}\n"));
    }
    if let Some(unknown) = meta.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown key {unknown} (known: {})", KEYS.join(", ")));
    }
    match meta.get("neochumbyd").and_then(Value::as_str) {
        None | Some("required" | "optional") => {}
        Some(v) => return Err(format!(r#"neochumbyd: {v:?} is not "required" or "optional" (or leave it out)"#)),
    }
    if meta.contains_key("companion") && !meta.contains_key("neochumbyd") {
        return Err(r#"companion: only a neochumbyd runs it; set neochumbyd = "required" or "optional""#.into());
    }
    if out.is_empty() {
        return Err("[package.metadata.neochumby] sets nothing".into());
    }
    Ok(out)
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: neochumby-manifest path/to/Cargo.toml");
        return ExitCode::from(2);
    };
    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| manifest(&s)) {
        Ok(m) => {
            print!("{m}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("neochumby-manifest: {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::manifest;

    #[test]
    fn writes_the_keys_in_order() {
        let m = manifest(
            r#"
            [package]
            name = "neochumby-quake"
            [package.metadata.neochumby]
            interrupt = false
            colour = "8c5a28"
            exec = "neochumby-quake"
            button = true
            title = "QUAKE"
            "#,
        )
        .unwrap();
        assert_eq!(m, "title=QUAKE\nexec=neochumby-quake\ncolour=8c5a28\nbutton=true\ninterrupt=false\n");
        let home = "[package]\nname = \"neochumby-home\"\n[package.metadata.neochumby]\nexec = \"neochumby-home\"\nstart = true";
        assert_eq!(manifest(home).unwrap(), "exec=neochumby-home\nstart=true\n");
    }

    #[test]
    fn refuses_what_a_chumby_could_not_read() {
        assert!(manifest("[package]\nname = \"doom\"\n[package.metadata.neochumby]\ntitle = \"D\"").is_err());
        assert!(manifest("[package]\nname = \"neochumby-x\"").is_err());
        assert!(manifest(&wrap("titel = \"X\"")).is_err());
        assert!(manifest(&wrap("title = \"two\\nlines\"")).is_err());
        assert!(manifest(&wrap("title = 3")).is_err());
        assert!(manifest(&wrap("start = \"true\"")).is_err());
        assert!(manifest(&wrap("title = true")).is_err());
        // Gone with proposal 0002: every app is a plain exec app.
        assert!(manifest(&wrap("home = \"neochumby-x\"")).is_err());
        assert!(manifest(&wrap("shells = [\"wx\"]")).is_err());
    }

    fn wrap(meta: &str) -> String {
        format!("[package]\nname = \"neochumby-x\"\n[package.metadata.neochumby]\n{meta}\n")
    }

    #[test]
    fn neochumbyd_is_required_optional_or_absent() {
        assert_eq!(manifest(&wrap(r#"neochumbyd = "required""#)).unwrap(), "neochumbyd=required\n");
        assert_eq!(manifest(&wrap(r#"neochumbyd = "optional""#)).unwrap(), "neochumbyd=optional\n");
        let e = manifest(&wrap(r#"neochumbyd = "yes""#)).unwrap_err();
        assert!(e.contains(r#"not "required" or "optional""#), "{e}");
    }

    #[test]
    fn access_is_a_list_of_what_the_session_grants() {
        assert_eq!(manifest(&wrap(r#"access = ["screen", "touch"]"#)).unwrap(), "access=screen,touch\n");
        assert_eq!(manifest(&wrap(r#"access = ["root"]"#)).unwrap(), "access=root\n");
        for bad in [r#"access = "screen""#, r#"access = ["disk"]"#, r#"access = ["screen", "screen"]"#, "access = []"] {
            assert!(manifest(&wrap(bad)).unwrap_err().starts_with("access:"), "{bad}");
        }
        assert!(manifest(&wrap(r#"access = ["root", "screen"]"#)).unwrap_err().contains("alone"));
    }

    #[test]
    fn a_companion_needs_neochumbyd() {
        let c = "service = \"neochumby-x\"\ncompanion = \"neochumby-x-companion\"";
        let e = manifest(&wrap(c)).unwrap_err();
        assert!(e.starts_with("companion:"), "{e}");
        for needs in ["required", "optional"] {
            let m = manifest(&wrap(&format!("{c}\nneochumbyd = \"{needs}\""))).unwrap();
            assert!(m.ends_with(&format!("companion=neochumby-x-companion\nneochumbyd={needs}\n")), "{m}");
        }
    }
}
