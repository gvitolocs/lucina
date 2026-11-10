//! An app's manifest: a few `key = value` lines in `/usr/share/lucina/apps/<id>.app`, read by the
//! home screen. `#` starts a comment line.
//!
//! ```text
//! id = hello
//! title = Hello
//! exec = /usr/bin/lucina-app-hello
//! icon = H
//! needs_network = false
//! terminal = false
//! ```
//!
//! `exec` defaults to `/usr/bin/lucina-app-<id>`. `icon` is the name of an icon in the pack
//! (`assets/icons/juno`, `crate::icons`) when it is a word of lower-case letters, digits and -, at
//! least three long; a PNG, JPEG or WebP file when it starts with `/`; otherwise one or two characters
//! drawn on the tile. The default is the icon named after the id, then the title's first letter.
//! `terminal = true` is a program for the terminal, not the screen: the shell types it into its terminal.
//! `intents = notes.create, notes.open` names the app's intents (`crate::intent`), so that lucina-devd knows them without
//! starting it; each is `<id>.<verb>`. Their schemas come from `<exec> --intents`.

use std::path::Path;

/// Where the manifests live on the device.
pub const MANIFEST_DIR: &str = "/usr/share/lucina/apps";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub title: String,
    pub exec: String,
    pub icon: String,
    /// The app is of no use without a network: the tile says so while offline.
    pub needs_network: bool,
    /// A program for the terminal, which runs there instead of taking the screen.
    pub terminal: bool,
    /// Its intents' names, `<id>.<verb>`.
    pub intents: Vec<String>,
}

impl Manifest {
    pub fn parse(s: &str) -> Result<Manifest, String> {
        let (mut id, mut title, mut exec, mut icon, mut needs_network, mut terminal) = (None, None, None, None, false, false);
        let mut intents: Vec<String> = Vec::new();
        for (n, line) in s.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: no '='", n + 1))?;
            let v = v.trim().to_string();
            match k.trim() {
                "id" => id = Some(v),
                "title" => title = Some(v),
                "exec" => exec = Some(v),
                "icon" => icon = Some(v),
                "needs_network" | "network" => {
                    needs_network = match v.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("line {}: needs_network is true or false", n + 1)),
                    }
                }
                "terminal" => {
                    terminal = match v.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("line {}: terminal is true or false", n + 1)),
                    }
                }
                "intents" => {
                    intents = v.split(',').map(|i| i.trim().to_string()).filter(|i| !i.is_empty()).collect();
                }
                other => return Err(format!("line {}: unknown key {other}", n + 1)),
            }
        }
        let id: String = id.ok_or("no id")?;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!("bad id {id:?}: lower-case letters, digits and -"));
        }
        for i in &intents {
            let verb = i.strip_prefix(id.as_str()).and_then(|r| r.strip_prefix('.')).unwrap_or("");
            if verb.is_empty() || !verb.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(format!("bad intent {i:?}: {id}.<verb>, the verb in lower-case letters and _"));
            }
        }
        let title: String = title.ok_or("no title")?;
        if title.is_empty() {
            return Err("empty title".into());
        }
        let first = title.chars().next().map(String::from).unwrap_or_default();
        let default_icon = if crate::icons::is_name(&id) {
            id.clone()
        } else {
            first
        };
        Ok(Manifest {
            exec: exec.unwrap_or_else(|| format!("/usr/bin/lucina-app-{id}")),
            icon: icon.unwrap_or(default_icon),
            id,
            title,
            needs_network,
            terminal,
            intents,
        })
    }
}

/// Every readable `*.app` in `dir`, sorted by title. A bad manifest is reported through `bad` and
/// skipped, so one broken app cannot take the home screen with it.
pub fn scan(dir: &Path, bad: &mut dyn FnMut(String)) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut apps: Vec<Manifest> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "app") {
            continue;
        }
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| Manifest::parse(&s))
        {
            Ok(mut m) => {
                // A picture beside the manifest, named after the app, is its icon.
                let picture = dir.join(format!("{}.png", m.id));
                if picture.exists() {
                    m.icon = picture.to_string_lossy().into_owned();
                }
                apps.push(m)
            }
            Err(e) => bad(format!("{}: {e}", path.display())),
        }
    }
    apps.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_defaults() {
        let m =
            Manifest::parse("# c\nid = hello\n\n title=Hello world \nnetwork = true\n").unwrap();
        assert_eq!(m.id, "hello");
        assert_eq!(m.title, "Hello world");
        assert_eq!(m.exec, "/usr/bin/lucina-app-hello");
        assert_eq!(m.icon, "hello");
        assert!(m.needs_network);
        // An id too short to be an icon's name: the title's first letter.
        let m = Manifest::parse("id = x\ntitle = Hello\n").unwrap();
        assert_eq!(m.icon, "H");
    }

    #[test]
    fn intents_are_named_after_the_app() {
        let m = Manifest::parse("id = notes\ntitle = Notes\nintents = notes.create, notes.open ,\n").unwrap();
        assert_eq!(m.intents, vec!["notes.create", "notes.open"]);
        assert!(Manifest::parse("id = notes\ntitle = Notes\n").unwrap().intents.is_empty());
        for bad in ["player.play", "notes.", "notes.Open", "notes", "notes.open.now"] {
            assert!(Manifest::parse(&format!("id = notes\ntitle = Notes\nintents = {bad}\n")).is_err(), "{bad}");
        }
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        assert!(Manifest::parse("title = x").is_err());
        assert!(Manifest::parse("id = x").is_err());
        assert!(Manifest::parse("id = X/y\ntitle = t").is_err());
        assert!(Manifest::parse("id = x\ntitle = t\nnetwork = maybe").is_err());
        assert!(Manifest::parse("id = x\ntitle = t\ncolour = red").is_err());
        assert!(Manifest::parse("id = x\ngarbage").is_err());
    }

    #[test]
    fn scan_skips_bad_files_and_sorts() {
        let d = std::env::temp_dir().join(format!("lucina-app-scan-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("b.app"), "id = b\ntitle = Alpha\n").unwrap();
        std::fs::write(d.join("a.app"), "id = a\ntitle = Beta\n").unwrap();
        std::fs::write(d.join("bad.app"), "nonsense\n").unwrap();
        std::fs::write(d.join("ignored.txt"), "nonsense\n").unwrap();
        let mut errors = Vec::new();
        let apps = scan(&d, &mut |e| errors.push(e));
        std::fs::remove_dir_all(&d).ok();
        assert_eq!(
            apps.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(errors.len(), 1);
        assert!(scan(Path::new("/nonexistent-dir"), &mut |_| {}).is_empty());
    }
}
