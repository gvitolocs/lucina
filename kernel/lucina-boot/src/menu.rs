//! The decisions LucinaLoader makes (`boot/lucina-loader/`, the UEFI boot menu that takes GRUB's place in LucinaOS
//! on the Galaxy Book Go), kept apart from its firmware calls so they run and are tested on the host.
//!
//! * [`Config`]: the menu file `/lucinaos/boot.toml` (a small TOML subset, below), plus the optional files it
//!   includes and the GRUB environment block it reads ([`env_vars`]).
//! * [`Config::resolve`]: which entries show, which is the default and the countdown, from which files exist
//!   (GRUB's `if [ -f ... ]` in the old `grub.cfg`).
//! * [`expand`], [`kernel_cmdline`]: `$name` variables and the command line GRUB's `linux` command built
//!   (`BOOT_IMAGE=<path> <args>`, words separated by one space).
//! * [`Menu`]: the keys and the countdown.
//! * [`Layout`] and the colours: where GRUB 2.06 drew the LucinaOS theme (`boards/gbgo/theme/theme.txt` in
//!   lucinaos), measured on its render.
//!
//! The menu file:
//!
//! ```toml
//! title = "LucinaOS"                       # the two lines above the menu
//! subtitle = "Samsung Galaxy Book Go"
//! default = "gbgo"                         # an entry id; a hidden or unknown one means the first entry shown
//! timeout = 8                              # seconds; 0 boots the default without showing the menu
//! modes = ["1920x1080", "1366x768"]        # GOP modes to try, in order; else the firmware's mode stays
//! include = ["/lucinaos/test.toml"]        # optional files read after this one, when they exist
//! env = "/lucinaos/grubenv"                # optional GRUB environment block: its variables join [vars]
//!
//! [vars]
//! gbgo_args = "console=tty0 clk_ignore_unused"
//!
//! [[when]]                                 # in order; a later match wins
//! exists = "/lucinaos/install-pending"
//! default = "install"
//! timeout = 10
//!
//! [[entry]]
//! id = "gbgo"
//! title = "LucinaOS (Galaxy Book Go)"
//! if = "/lucinaos/install-pending"         # optional: shown only when this file exists
//! message = "Loading Linux ..."            # optional: shown while it loads
//! linux = "/lucinaos/vmlinuz"              # an EFI-stub kernel
//! cmdline = "$gbgo_args"
//! devicetree = "/lucinaos/gbgo.dtb"        # optional
//! initrd = ["/lucinaos/initrd.gz"]         # optional; several are concatenated, each padded to 4 bytes
//!
//! [[entry]]
//! id = "fwsetup"
//! title = "Firmware setup (F2)"
//! action = "firmware-setup"                # restart into the firmware's setup screen
//! ```
//!
//! What GRUB's scripted `grub.cfg` did with `save_env` and functions is spelt with a few more keys:
//!
//! ```toml
//! save = ["lucina_entry=menu"]             # written to the `env` block when the menu shows (GRUB's save_env)
//!
//! [vars.v1]                                # a group of variables; an entry's `with` names the group to use
//! v_args = ""
//! v_next = "v2"
//!
//! [[when]]
//! exists = "/lucinaos/test-overlay.cpio"
//! unless = "/lucinaos/install-pending"     # optional: not when this file exists
//!
//! [[entry]]
//! if_env = ["lucina_step!=done", "lucina_ok"]  # all must hold: name=value, name!=value, name (set), !name (unset)
//! with = "$lucina_step"                    # the group whose variables this entry sees on top of [vars]
//! save = ["lucina_entry=$lucina_step", "lucina_step=$v_next"]  # written to `env` before it starts, values
//!                                          # expanded with the variables as they were before
//! message = ["line one", "line two"]       # a string or up to four lines
//! action = "message"                       # only show the message: for `wait` seconds (0: until a key)
//! wait = 5
//! ```
//!
//! `slots = "/lucinaos"` turns on GRUB's A/B update slots ([`slot_boot`]): when `<slots>/a/vmlinuz` or
//! `<slots>/b/vmlinuz` exists, the loader chooses a slot from `env` and sets `lucina_dir`, `lucina_boot`,
//! `lucina_kind`, `lucina_other`, `lucina_esp`, `lucina_title` and `slot_args`. An entry's `title` and `if` may hold
//! variables.
//!
//! A `devicetree` or `initrd` path that expands to nothing is left out (a test file can clear `$dtb`). Values in the
//! GRUB environment block replace the menu file's variables, except empty ones, which keep the menu file's value
//! (GRUB scripts the same with `if [ -z "$x" ]; then set x=...`).
//!
//! Strings are `"..."` or `'...'` on one line, without escapes; arrays hold strings on one line; integers are
//! decimal. An included file may set anything the menu file sets: its keys and variables replace earlier ones, its
//! `[[when]]` and `[[entry]]` tables are added after the earlier ones.

/// Most entries in a menu.
pub const MAX_ENTRIES: usize = 16;
/// Most variables (`[vars]`, the groups and the GRUB environment block together).
pub const MAX_VARS: usize = 128;
/// Most conditions in an `if_env`.
pub const MAX_CONDS: usize = 4;
/// Most assignments in a `save`.
pub const MAX_SAVES: usize = 8;
/// Most lines in a `message`.
pub const MAX_LINES: usize = 4;
/// Most `[[when]]` tables.
pub const MAX_WHENS: usize = 8;
/// Most files in an entry's `initrd`.
pub const MAX_INITRDS: usize = 4;
/// Most GOP modes in `modes`.
pub const MAX_MODES: usize = 8;
/// Most files in `include`.
pub const MAX_INCLUDES: usize = 4;
/// Linux's `COMMAND_LINE_SIZE` on arm64.
pub const CMDLINE_MAX: usize = 2048;
/// The size of a GRUB environment block (`grub-editenv create`).
pub const ENV_BLOCK_SIZE: usize = 1024;
/// The first line of a GRUB environment block.
pub const ENV_HEADER: &str = "# GRUB Environment Block\n";

// ---------------------------------------------------------------------------------------------------------------
// The menu file
// ---------------------------------------------------------------------------------------------------------------

/// A fixed-capacity list of borrowed strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct List<'a, const N: usize> {
    items: [&'a str; N],
    len: usize,
}

impl<'a, const N: usize> List<'a, N> {
    pub const fn new() -> Self {
        List {
            items: [""; N],
            len: 0,
        }
    }

    pub fn as_slice(&self) -> &[&'a str] {
        &self.items[..self.len]
    }

    fn push(&mut self, s: &'a str) -> Result<(), &'static str> {
        let slot = self.items.get_mut(self.len).ok_or("too many items")?;
        *slot = s;
        self.len += 1;
        Ok(())
    }
}

impl<const N: usize> Default for List<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// What an entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    /// Starts `linux` through the firmware (LoadImage/StartImage) with the command line, device tree and initrd.
    #[default]
    Linux,
    /// Asks the firmware to open its setup screen at the next start, then restarts (GRUB's `fwsetup`).
    FirmwareSetup,
    /// Shows `message` (after `save`) and goes back to the menu: GRUB entries that only `echo`, `save_env`, `sleep`.
    Message,
}

/// One `[[entry]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Entry<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub if_exists: Option<&'a str>,
    pub if_env: List<'a, MAX_CONDS>,
    pub with: Option<&'a str>,
    pub save: List<'a, MAX_SAVES>,
    pub message: List<'a, MAX_LINES>,
    pub wait: u32,
    pub action: Action,
    pub linux: &'a str,
    pub cmdline: &'a str,
    pub devicetree: Option<&'a str>,
    pub initrd: List<'a, MAX_INITRDS>,
}

/// One `[[when]]`: when `exists` is there (and `unless` is not), `default` and `timeout` replace the menu's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct When<'a> {
    pub exists: &'a str,
    pub unless: Option<&'a str>,
    pub default: Option<&'a str>,
    pub timeout: Option<u32>,
}

/// Variables by name, each in a group (`""` for `[vars]` and the environment block, `v1` for `[vars.v1]`); setting
/// a name again in the same group replaces its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vars<'a> {
    groups: [&'a str; MAX_VARS],
    names: [&'a str; MAX_VARS],
    values: [&'a str; MAX_VARS],
    len: usize,
}

impl<'a> Vars<'a> {
    pub const fn new() -> Self {
        Vars {
            groups: [""; MAX_VARS],
            names: [""; MAX_VARS],
            values: [""; MAX_VARS],
            len: 0,
        }
    }

    fn find(&self, group: &str, name: &str) -> Option<usize> {
        (0..self.len).find(|&i| self.groups[i] == group && self.names[i] == name)
    }

    pub fn get(&self, name: &str) -> Option<&'a str> {
        self.find("", name).map(|i| self.values[i])
    }

    pub fn set(&mut self, name: &'a str, value: &'a str) -> Result<(), &'static str> {
        self.set_in("", name, value)
    }

    pub fn set_in(
        &mut self,
        group: &'a str,
        name: &'a str,
        value: &'a str,
    ) -> Result<(), &'static str> {
        if let Some(i) = self.find(group, name) {
            self.values[i] = value;
            return Ok(());
        }
        if self.len == MAX_VARS {
            return Err("too many variables");
        }
        self.groups[self.len] = group;
        self.names[self.len] = name;
        self.values[self.len] = value;
        self.len += 1;
        Ok(())
    }

    /// The variables outside groups.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        (0..self.len)
            .filter(|&i| self.groups[i].is_empty())
            .map(|i| (self.names[i], self.values[i]))
    }

    /// These variables with group `group`'s on top (an entry's `with`).
    pub fn scoped(&self, group: &str) -> Result<Vars<'a>, &'static str> {
        let mut v = *self;
        for i in 0..self.len {
            if !group.is_empty() && self.groups[i] == group {
                v.set(self.names[i], self.values[i])?;
            }
        }
        Ok(v)
    }

    /// One `if_env` condition: `name=value`, `name!=value`, `name` (set and not empty), `!name` (unset or empty).
    pub fn holds(&self, cond: &str) -> bool {
        let value = |n: &str| self.get(n).unwrap_or("");
        if let Some((n, v)) = cond.split_once("!=") {
            value(n) != v
        } else if let Some((n, v)) = cond.split_once('=') {
            value(n) == v
        } else if let Some(n) = cond.strip_prefix('!') {
            value(n).is_empty()
        } else {
            !value(cond).is_empty()
        }
    }
}

impl Default for Vars<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// The menu file and what it includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub default: &'a str,
    pub timeout: u32,
    pub modes: List<'a, MAX_MODES>,
    pub include: List<'a, MAX_INCLUDES>,
    pub env: Option<&'a str>,
    /// Written to `env` when the menu shows.
    pub save: List<'a, MAX_SAVES>,
    /// Where the update slots `a/` and `b/` are; see [`slot_boot`].
    pub slots: Option<&'a str>,
    pub vars: Vars<'a>,
    whens: [When<'a>; MAX_WHENS],
    nwhens: usize,
    entries: [Entry<'a>; MAX_ENTRIES],
    nentries: usize,
}

/// A mistake in a menu file: its line (1-based; 0 for the file as a whole) and what is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError {
    pub line: u32,
    pub what: &'static str,
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.what)
        } else {
            write!(f, "line {}: {}", self.line, self.what)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section<'a> {
    Top,
    /// `[vars]` (group `""`) or `[vars.NAME]`.
    Vars(&'a str),
    When,
    Entry,
}

enum Value<'a> {
    Str(&'a str),
    Int(u32),
    Array(&'a str),
}

impl<'a> Config<'a> {
    pub const fn new() -> Self {
        Config {
            title: "",
            subtitle: "",
            default: "",
            timeout: 5,
            modes: List::new(),
            include: List::new(),
            env: None,
            save: List::new(),
            slots: None,
            vars: Vars::new(),
            whens: [When {
                exists: "",
                unless: None,
                default: None,
                timeout: None,
            }; MAX_WHENS],
            nwhens: 0,
            entries: [Entry {
                id: "",
                title: "",
                if_exists: None,
                if_env: List::new(),
                with: None,
                save: List::new(),
                message: List::new(),
                wait: 0,
                action: Action::Linux,
                linux: "",
                cmdline: "",
                devicetree: None,
                initrd: List::new(),
            }; MAX_ENTRIES],
            nentries: 0,
        }
    }

    /// Parses a menu file; [`Config::check`] it once every included file is read.
    pub fn parse(text: &'a str) -> Result<Config<'a>, ConfigError> {
        let mut c = Config::new();
        c.read(text)?;
        Ok(c)
    }

    pub fn entries(&self) -> &[Entry<'a>] {
        &self.entries[..self.nentries]
    }

    pub fn whens(&self) -> &[When<'a>] {
        &self.whens[..self.nwhens]
    }

    /// Replaces entry `i`'s title (with its variables expanded).
    pub fn set_title(&mut self, i: usize, title: &'a str) {
        self.entries[i].title = title;
    }

    /// Reads one more file (the menu file, then each include) on top of what is there.
    pub fn read(&mut self, text: &'a str) -> Result<(), ConfigError> {
        let mut section = Section::Top;
        for (n, raw) in text.split('\n').enumerate() {
            let line = n as u32 + 1;
            let err = |what| ConfigError { line, what };
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(rest) = l.strip_prefix('[') {
                let (name, array) = match rest.strip_prefix('[') {
                    Some(r) => (r.split_once("]]"), true),
                    None => (rest.split_once(']'), false),
                };
                let (name, tail) = name.ok_or(err("unterminated table header"))?;
                if !is_blank_or_comment(tail) {
                    return Err(err("text after the table header"));
                }
                section = match (name.trim(), array) {
                    ("vars", false) => Section::Vars(""),
                    (n, false) if n.starts_with("vars.") => {
                        let group = &n["vars.".len()..];
                        if group.is_empty() || !group.bytes().all(is_name_byte) {
                            return Err(err("bad group name in [vars.NAME]"));
                        }
                        Section::Vars(group)
                    }
                    ("when", true) => {
                        if self.nwhens == MAX_WHENS {
                            return Err(err("too many [[when]] tables"));
                        }
                        self.nwhens += 1;
                        Section::When
                    }
                    ("entry", true) => {
                        if self.nentries == MAX_ENTRIES {
                            return Err(err("too many [[entry]] tables"));
                        }
                        self.entries[self.nentries] = Entry::default();
                        self.nentries += 1;
                        Section::Entry
                    }
                    _ => {
                        return Err(err(
                            "unknown table (vars, vars.NAME, [[when]] and [[entry]] exist)",
                        ));
                    }
                };
                continue;
            }
            let (key, rest) = l.split_once('=').ok_or(err("expected key = value"))?;
            let key = key.trim();
            if key.is_empty() || !key.bytes().all(is_name_byte) {
                return Err(err("bad key"));
            }
            let value = parse_value(rest.trim()).map_err(err)?;
            match section {
                Section::Top => self.set_top(key, value),
                Section::Vars(group) => match value {
                    Value::Str(s) => self.vars.set_in(group, key, s),
                    _ => Err("variables are strings"),
                },
                Section::When => set_when(&mut self.whens[self.nwhens - 1], key, value),
                Section::Entry => set_entry(&mut self.entries[self.nentries - 1], key, value),
            }
            .map_err(err)?;
        }
        Ok(())
    }

    fn set_top(&mut self, key: &str, v: Value<'a>) -> Result<(), &'static str> {
        match (key, v) {
            ("title", Value::Str(s)) => self.title = s,
            ("subtitle", Value::Str(s)) => self.subtitle = s,
            ("default", Value::Str(s)) => self.default = s,
            ("timeout", Value::Int(t)) => self.timeout = t,
            ("env", Value::Str(s)) => self.env = Some(s),
            ("slots", Value::Str(s)) => self.slots = Some(s),
            ("modes", Value::Array(a)) => {
                self.modes = List::new();
                for m in strings(a) {
                    let m = m?;
                    parse_mode(m).ok_or("a mode is WIDTHxHEIGHT")?;
                    self.modes.push(m).map_err(|_| "too many modes")?;
                }
            }
            ("include", Value::Array(a)) => {
                self.include = List::new();
                for f in strings(a) {
                    self.include.push(f?).map_err(|_| "too many includes")?;
                }
            }
            ("save", Value::Array(a)) => self.save = assignments(a)?,
            ("title" | "subtitle" | "default" | "env" | "slots", _) => {
                return Err("expected a string");
            }
            ("timeout", _) => return Err("expected a number"),
            ("modes" | "include" | "save", _) => return Err("expected an array of strings"),
            _ => return Err("unknown key"),
        }
        Ok(())
    }

    /// Checks what [`Config::read`] cannot see line by line: every entry has an id, a title and, to boot Linux, a
    /// kernel; ids are unique; `[[when]]` tables name a file.
    pub fn check(&self) -> Result<(), ConfigError> {
        let err = |what| Err(ConfigError { line: 0, what });
        if self.nentries == 0 {
            return err("no [[entry]]");
        }
        for (i, e) in self.entries().iter().enumerate() {
            if e.id.is_empty() || e.title.is_empty() {
                return err("an [[entry]] has no id or no title");
            }
            if e.action == Action::Linux && e.linux.is_empty() {
                return err("an [[entry]] has no linux = kernel");
            }
            if e.action == Action::Message && e.message.as_slice().is_empty() {
                return err("an action = \"message\" [[entry]] has no message");
            }
            if self.entries()[..i].iter().any(|o| o.id == e.id) {
                return err("two entries have the same id");
            }
        }
        if self.whens().iter().any(|w| w.exists.is_empty()) {
            return err("a [[when]] has no exists = file");
        }
        Ok(())
    }

    /// The GOP modes to try, as (width, height).
    pub fn modes(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.modes.as_slice().iter().filter_map(|m| parse_mode(m))
    }

    /// Which entries show, the default and the countdown, given which files exist and the variables (for
    /// `if_env`). An entry with the default's id that does not show leaves the default to the first entry shown.
    pub fn resolve(&self, mut exists: impl FnMut(&str) -> bool) -> Shown {
        let mut default = self.default;
        let mut timeout = self.timeout;
        for w in self.whens() {
            if exists(w.exists) && !w.unless.is_some_and(&mut exists) {
                if let Some(d) = w.default {
                    default = d;
                }
                if let Some(t) = w.timeout {
                    timeout = t;
                }
            }
        }
        let mut s = Shown {
            index: [0; MAX_ENTRIES],
            len: 0,
            default: 0,
            timeout,
        };
        for (i, e) in self.entries().iter().enumerate() {
            let env_ok = e.if_env.as_slice().iter().all(|c| self.vars.holds(c));
            if env_ok && e.if_exists.is_none_or(&mut exists) {
                if e.id == default {
                    s.default = s.len;
                }
                s.index[s.len] = i as u8;
                s.len += 1;
            }
        }
        s
    }
}

impl Default for Config<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of [`Config::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    index: [u8; MAX_ENTRIES],
    len: usize,
    /// Position in [`Shown::entries`], not an index into [`Config::entries`].
    pub default: usize,
    pub timeout: u32,
}

impl Shown {
    /// Indices into [`Config::entries`] of the entries that show, in menu order.
    pub fn entries(&self) -> impl Iterator<Item = usize> + '_ {
        self.index[..self.len].iter().map(|&i| usize::from(i))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The index into [`Config::entries`] of the `n`th entry shown.
    pub fn entry(&self, n: usize) -> usize {
        usize::from(self.index[n])
    }
}

fn set_when<'a>(w: &mut When<'a>, key: &str, v: Value<'a>) -> Result<(), &'static str> {
    match (key, v) {
        ("exists", Value::Str(s)) => w.exists = s,
        ("unless", Value::Str(s)) => w.unless = Some(s),
        ("default", Value::Str(s)) => w.default = Some(s),
        ("timeout", Value::Int(t)) => w.timeout = Some(t),
        ("exists" | "unless" | "default", _) => return Err("expected a string"),
        ("timeout", _) => return Err("expected a number"),
        _ => return Err("unknown key in [[when]]"),
    }
    Ok(())
}

fn set_entry<'a>(e: &mut Entry<'a>, key: &str, v: Value<'a>) -> Result<(), &'static str> {
    match (key, v) {
        ("id", Value::Str(s)) => e.id = s,
        ("title", Value::Str(s)) => e.title = s,
        ("if", Value::Str(s)) => e.if_exists = Some(s),
        ("with", Value::Str(s)) => e.with = Some(s),
        ("message", Value::Str(s)) => {
            e.message = List::new();
            e.message.push(s)?;
        }
        ("message", Value::Array(a)) => {
            e.message = List::new();
            for l in strings(a) {
                e.message
                    .push(l?)
                    .map_err(|_| "a message has at most 4 lines")?;
            }
        }
        ("if_env", Value::Array(a)) => {
            e.if_env = List::new();
            for c in strings(a) {
                let c = c?;
                let body = c.strip_prefix('!').unwrap_or(c);
                let name = body
                    .split_once("!=")
                    .or_else(|| body.split_once('='))
                    .map_or(body, |(n, _)| n);
                if name.is_empty() || !name.bytes().all(is_name_byte) {
                    return Err("a condition is name=value, name!=value, name or !name");
                }
                e.if_env.push(c).map_err(|_| "too many conditions")?;
            }
        }
        ("save", Value::Array(a)) => e.save = assignments(a)?,
        ("wait", Value::Int(t)) => e.wait = t,
        ("linux", Value::Str(s)) => e.linux = s,
        ("cmdline", Value::Str(s)) => e.cmdline = s,
        ("devicetree", Value::Str(s)) => e.devicetree = Some(s),
        ("action", Value::Str("linux")) => e.action = Action::Linux,
        ("action", Value::Str("firmware-setup")) => e.action = Action::FirmwareSetup,
        ("action", Value::Str("message")) => e.action = Action::Message,
        ("action", _) => return Err("action is \"linux\", \"firmware-setup\" or \"message\""),
        ("initrd", Value::Array(a)) => {
            e.initrd = List::new();
            for f in strings(a) {
                e.initrd.push(f?).map_err(|_| "too many initrd files")?;
            }
        }
        ("initrd" | "if_env" | "save", _) => return Err("expected an array of strings"),
        ("message", _) => return Err("expected a string or an array of strings"),
        ("wait", _) => return Err("expected a number"),
        ("id" | "title" | "if" | "with" | "linux" | "cmdline" | "devicetree", _) => {
            return Err("expected a string");
        }
        _ => return Err("unknown key in [[entry]]"),
    }
    Ok(())
}

/// A `save` array: `name=value` items (the value may hold `$variables`).
fn assignments(a: &str) -> Result<List<'_, MAX_SAVES>, &'static str> {
    let mut l = List::new();
    for s in strings(a) {
        let s = s?;
        let (name, _) = s.split_once('=').ok_or("a save item is name=value")?;
        if name.is_empty() || !name.bytes().all(is_name_byte) {
            return Err("a save item is name=value");
        }
        l.push(s).map_err(|_| "too many save items")?;
    }
    Ok(l)
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_blank_or_comment(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || s.starts_with('#')
}

/// A one-line string starting at `s`: the string and what follows its closing quote.
fn parse_string(s: &str) -> Result<(&str, &str), &'static str> {
    let quote = s.as_bytes()[0];
    let body = &s[1..];
    let end = body
        .find(quote as char)
        .ok_or("unterminated string (strings end on their line)")?;
    let value = &body[..end];
    if quote == b'"' && value.contains('\\') {
        return Err("escapes are not supported (use '...' for a backslash)");
    }
    Ok((value, &body[end + 1..]))
}

fn parse_value(s: &str) -> Result<Value<'_>, &'static str> {
    let (v, tail) = match s.as_bytes().first() {
        Some(b'"' | b'\'') => {
            let (v, tail) = parse_string(s)?;
            (Value::Str(v), tail)
        }
        Some(b'[') => {
            let end = array_end(s).ok_or("unterminated array (arrays end on their line)")?;
            let inner = &s[1..end];
            for item in strings(inner) {
                item?;
            }
            (Value::Array(inner), &s[end + 1..])
        }
        Some(b'0'..=b'9') => {
            let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let n = s[..end].parse().map_err(|_| "number too large")?;
            (Value::Int(n), &s[end..])
        }
        _ => return Err("expected \"string\", [array] or number"),
    };
    if !is_blank_or_comment(tail) {
        return Err("text after the value");
    }
    Ok(v)
}

/// The position of the `]` that closes the array `s` starts with (one outside quotes).
fn array_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (None, ']') => return Some(i),
            (Some(q), _) if c == q => quote = None,
            _ => {}
        }
    }
    None
}

/// The strings of an array's inside (`"a", 'b',`), checked one by one.
fn strings(inner: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
    let mut rest = inner;
    core::iter::from_fn(move || {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        if !rest.starts_with(['"', '\'']) {
            rest = "";
            return Some(Err("arrays hold strings"));
        }
        let (v, tail) = match parse_string(rest) {
            Ok(x) => x,
            Err(e) => {
                rest = "";
                return Some(Err(e));
            }
        };
        let tail = tail.trim_start();
        rest = match tail.strip_prefix(',') {
            Some(t) => t,
            None if tail.is_empty() => "",
            None => {
                rest = "";
                return Some(Err("expected , between array items"));
            }
        };
        Some(Ok(v))
    })
}

/// `1366x768` as (1366, 768).
pub fn parse_mode(m: &str) -> Option<(u32, u32)> {
    let (w, h) = m.split_once('x')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

// ---------------------------------------------------------------------------------------------------------------
// The GRUB environment block (shared with GRUB and the A/B update)
// ---------------------------------------------------------------------------------------------------------------

/// The `name=value` lines of a GRUB environment block (`grub-editenv`'s format: the header line, one variable per
/// line, `#` padding up to 1024 bytes). Values GRUB had to escape (a backslash or a newline in them) are skipped.
/// `None` when `block` is not an environment block.
pub fn env_vars(block: &str) -> Option<impl Iterator<Item = (&str, &str)>> {
    let body = block.strip_prefix(ENV_HEADER)?;
    Some(body.split('\n').filter_map(|l| {
        let (k, v) = l.split_once('=')?;
        let ok = !k.is_empty() && k.bytes().all(is_name_byte) && !v.contains('\\');
        ok.then_some((k, v))
    }))
}

/// The value of `name` in a GRUB environment block, empty values included; `None` when it is not there.
pub fn env_get<'b>(block: &'b str, name: &str) -> Option<&'b str> {
    env_vars(block)?.find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// `block` (a GRUB environment block; anything else counts as an empty one) with `updates` applied, written into
/// `out` as grub-editenv and GRUB's `save_env` write it: the header, one `name=value` line per variable (the
/// block's own in their order, new names after them), then `#` up to 1024 bytes. The result has the block's size,
/// so it can be written over the file in place, as GRUB does. Values with a backslash or a newline are refused
/// (GRUB would escape them), and so is a result over 1024 bytes.
pub fn env_update(
    block: &str,
    updates: &[(&str, &str)],
    out: &mut [u8; ENV_BLOCK_SIZE],
) -> Result<(), TooLong> {
    const MAX: usize = 2 * MAX_SAVES;
    if updates.len() > MAX || updates.iter().any(|(_, v)| v.contains(['\\', '\n'])) {
        return Err(TooLong);
    }
    let mut done = [false; MAX];
    let mut at = 0;
    let mut put = |s: &str| -> Result<(), TooLong> {
        let end = at + s.len();
        out.get_mut(at..end)
            .ok_or(TooLong)?
            .copy_from_slice(s.as_bytes());
        at = end;
        Ok(())
    };
    put(ENV_HEADER)?;
    if let Some(body) = block.strip_prefix(ENV_HEADER) {
        for line in body.split('\n') {
            let Some((k, _)) = line.split_once('=') else {
                continue; // the '#' padding
            };
            match updates.iter().position(|(n, _)| *n == k) {
                Some(i) => {
                    done[i] = true;
                    put(k)?;
                    put("=")?;
                    put(updates[i].1)?;
                }
                None => put(line)?,
            }
            put("\n")?;
        }
    }
    for (i, (k, v)) in updates.iter().enumerate() {
        if !done[i] && !updates[..i].iter().any(|(n, _)| n == k) {
            put(k)?;
            put("=")?;
            put(v)?;
            put("\n")?;
        }
    }
    out[at..].fill(b'#');
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// The update slots
// ---------------------------------------------------------------------------------------------------------------

/// How a slot was chosen (`lucinaos.boot=` on the command line, read by lucina-update).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Home,
    Trial,
    Manual,
}

impl SlotKind {
    pub fn word(self) -> &'static str {
        match self {
            SlotKind::Home => "home",
            SlotKind::Trial => "trial",
            SlotKind::Manual => "manual",
        }
    }
}

/// The result of [`slot_boot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBoot {
    pub slot: &'static str,
    pub other: &'static str,
    pub kind: SlotKind,
}

/// `lucina_trial` as a slot: only `a` and `b` ask for a trial.
pub fn trial_slot(trial: Option<&str>) -> Option<&'static str> {
    match trial {
        Some("a") => Some("a"),
        Some("b") => Some("b"),
        _ => None,
    }
}

/// grub.cfg's choice of update slot. `home` is `lucina_slot` (anything but `b` is `a`); `trial` is the slot a trial
/// asked for, given only when clearing `lucina_trial` in the environment block read back, so a reset of the trial
/// kernel always goes home. A slot without a kernel (one whose trial failed) never boots: the other one does, as
/// `manual`.
pub fn slot_boot(
    home: Option<&str>,
    trial: Option<&'static str>,
    has_kernel: impl Fn(&str) -> bool,
) -> SlotBoot {
    let (mut slot, mut kind) = match trial {
        Some(t) => (t, SlotKind::Trial),
        None if home == Some("b") => ("b", SlotKind::Home),
        None => ("a", SlotKind::Home),
    };
    if !has_kernel(slot) {
        slot = if slot == "a" { "b" } else { "a" };
        kind = SlotKind::Manual;
    }
    let other = if slot == "a" { "b" } else { "a" };
    SlotBoot { slot, other, kind }
}

/// A FAT boot sector's volume id as GRUB's `probe --fs-uuid` prints it (`1a2b-3c4d`): at byte 67 on FAT32 (no 16-bit
/// FAT size at byte 22), at byte 39 on FAT12 and FAT16.
pub fn fat_volume_id<'b>(sector: &[u8], out: &'b mut [u8; 9]) -> Option<&'b str> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let fat16_size = u16::from_le_bytes([*sector.get(22)?, *sector.get(23)?]);
    let at = if fat16_size == 0 { 67 } else { 39 };
    let id = sector.get(at..at + 4)?;
    let id = u32::from_le_bytes([id[0], id[1], id[2], id[3]]);
    for (i, o) in out.iter_mut().enumerate() {
        *o = match i {
            4 => b'-',
            _ => {
                let nibble = if i < 4 { 7 - i } else { 8 - i };
                HEX[((id >> (nibble * 4)) & 0xf) as usize]
            }
        };
    }
    core::str::from_utf8(&out[..]).ok()
}

// ---------------------------------------------------------------------------------------------------------------
// Variables and the kernel command line
// ---------------------------------------------------------------------------------------------------------------

/// How deep variables inside variables are replaced.
pub const EXPAND_DEPTH: usize = 4;

/// The command line or path did not fit its buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong;

/// Writes words separated by single spaces, as GRUB passes the words of its `linux` command on.
struct Words<'b> {
    out: &'b mut [u8],
    len: usize,
    space: bool,
}

impl<'b> Words<'b> {
    fn push(&mut self, s: &str) -> Result<(), TooLong> {
        for &b in s.as_bytes() {
            if b.is_ascii_whitespace() {
                self.space = self.len > 0;
                continue;
            }
            if self.space {
                self.byte(b' ')?;
                self.space = false;
            }
            self.byte(b)?;
        }
        Ok(())
    }

    fn byte(&mut self, b: u8) -> Result<(), TooLong> {
        *self.out.get_mut(self.len).ok_or(TooLong)? = b;
        self.len += 1;
        Ok(())
    }

    /// Copies `template`, replacing `$name` and `${name}` with their values (unknown names are empty, as in GRUB).
    fn expand(&mut self, template: &str, vars: &Vars<'_>) -> Result<(), TooLong> {
        self.expand_in(template, vars, 0)
    }

    /// Variables inside a value are replaced too (GRUB expands them when `set` runs), at most [`EXPAND_DEPTH`]
    /// levels deep.
    fn expand_in(&mut self, template: &str, vars: &Vars<'_>, depth: usize) -> Result<(), TooLong> {
        let mut rest = template;
        while let Some(at) = rest.find('$') {
            self.push(&rest[..at])?;
            let after = &rest[at + 1..];
            let (name, tail) = match after.strip_prefix('{') {
                Some(b) => match b.find('}') {
                    Some(end) => (&b[..end], &b[end + 1..]),
                    None => ("", after),
                },
                None => {
                    let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
                    let end = end.unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            if name.is_empty() {
                self.push("$")?;
            } else if depth < EXPAND_DEPTH {
                self.expand_in(vars.get(name).unwrap_or(""), vars, depth + 1)?;
            } else {
                self.push(vars.get(name).unwrap_or(""))?;
            }
            rest = tail;
        }
        self.push(rest)
    }

    fn finish(self) -> &'b str {
        let out: &'b mut [u8] = self.out;
        // Only whole UTF-8 sequences from `&str`s and ASCII spaces were copied.
        core::str::from_utf8(&out[..self.len]).unwrap_or("")
    }
}

/// `template` with its variables replaced and its words separated by single spaces.
pub fn expand<'b>(template: &str, vars: &Vars<'_>, out: &'b mut [u8]) -> Result<&'b str, TooLong> {
    let mut w = Words {
        out,
        len: 0,
        space: false,
    };
    w.expand(template, vars)?;
    Ok(w.finish())
}

/// The command line GRUB's `linux /lucinaos/vmlinuz args...` gave the kernel: `BOOT_IMAGE=<path> args...`.
pub fn kernel_cmdline<'b>(
    e: &Entry<'_>,
    vars: &Vars<'_>,
    out: &'b mut [u8],
) -> Result<&'b str, TooLong> {
    let mut w = Words {
        out,
        len: 0,
        space: false,
    };
    w.push("BOOT_IMAGE=")?;
    w.expand(e.linux, vars)?;
    w.push(" ")?;
    w.expand(e.cmdline, vars)?;
    Ok(w.finish())
}

/// The initrd files concatenated: each starts on a 4-byte boundary (GRUB pads them the same way).
pub fn initrd_layout(sizes: &[u64], offsets: &mut [u64]) -> Option<u64> {
    let mut at = 0u64;
    for (i, (s, o)) in sizes.iter().zip(offsets.iter_mut()).enumerate() {
        if i > 0 {
            at = at.checked_add(3)? & !3;
        }
        *o = at;
        at = at.checked_add(*s)?;
    }
    Some(at)
}

// ---------------------------------------------------------------------------------------------------------------
// Keys and the countdown
// ---------------------------------------------------------------------------------------------------------------

/// The keys the menu reacts to (GRUB 2.06's normal menu); anything else only stops the countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Other,
}

/// What the loader does after a key or a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing changed on screen.
    Wait,
    /// Draw the menu again.
    Redraw,
    /// Start the `n`th entry shown.
    Boot(usize),
}

/// The selection and the countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Menu {
    pub selected: usize,
    len: usize,
    total_ms: u64,
    left_ms: Option<u64>,
}

impl Menu {
    /// A menu of `len` entries (at least one) with `default` selected and `timeout_s` to count down.
    pub fn new(len: usize, default: usize, timeout_s: u32) -> Menu {
        let total_ms = u64::from(timeout_s) * 1000;
        Menu {
            selected: default.min(len.saturating_sub(1)),
            len,
            total_ms,
            left_ms: Some(total_ms),
        }
    }

    /// True when the default boots at once: a timeout of 0 never shows the menu (GRUB's `timeout=0`).
    pub fn immediate(&self) -> bool {
        self.total_ms == 0
    }

    /// A key: moves the selection or starts it; every key stops the countdown, as in GRUB.
    pub fn key(&mut self, k: Key) -> Step {
        let counting = self.left_ms.take().is_some();
        let last = self.len.saturating_sub(1);
        let before = self.selected;
        self.selected = match k {
            Key::Enter => return Step::Boot(self.selected),
            Key::Up => self.selected.saturating_sub(1),
            Key::Down => (self.selected + 1).min(last),
            Key::Home | Key::PageUp => 0,
            Key::End | Key::PageDown => last,
            Key::Other => self.selected,
        };
        if counting || before != self.selected {
            Step::Redraw
        } else {
            Step::Wait
        }
    }

    /// `ms` milliseconds passed: the selection starts when the countdown runs out; the screen changes once a second.
    pub fn tick(&mut self, ms: u64) -> Step {
        let Some(left) = self.left_ms else {
            return Step::Wait;
        };
        let now = left.saturating_sub(ms);
        if now == 0 {
            self.left_ms = None;
            return Step::Boot(self.selected);
        }
        self.left_ms = Some(now);
        if now.div_ceil(1000) != left.div_ceil(1000) {
            Step::Redraw
        } else {
            Step::Wait
        }
    }

    /// The seconds shown ("Starting in N s."), while counting down.
    pub fn seconds_left(&self) -> Option<u64> {
        self.left_ms.map(|l| l.div_ceil(1000))
    }

    /// The part of the countdown bar that is filled, as `width * elapsed seconds / timeout` (GRUB fills it one
    /// whole second at a time).
    pub fn bar_fill(&self, width: u32) -> Option<u32> {
        let left = self.seconds_left()?;
        let total = self.total_ms / 1000;
        if total == 0 {
            return Some(0);
        }
        Some((u64::from(width) * (total - left.min(total)) / total) as u32)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The theme
// ---------------------------------------------------------------------------------------------------------------

/// The colours of `theme.txt` (0xRRGGBB).
pub mod colour {
    /// `desktop-color`: the background.
    pub const DESKTOP: u32 = 0x070b18;
    pub const TITLE: u32 = 0xffffff;
    pub const SUBTITLE: u32 = 0x7f8fe0;
    pub const ITEM: u32 = 0xd8d8e0;
    pub const ITEM_SELECTED: u32 = 0xffffff;
    /// `select_c.png`: the selected entry's bar.
    pub const SELECT: u32 = 0x2c2c2e;
    /// `select_w.png`: its left edge.
    pub const SELECT_EDGE: u32 = 0x7f8fe0;
    pub const BAR: u32 = 0x7f8fe0;
    pub const BAR_BACK: u32 = 0x2c2c2e;
    pub const TIMEOUT: u32 = 0x8e8e96;
    /// `message-color`: what an entry prints while it loads, and errors.
    pub const MESSAGE: u32 = 0xf2f2f7;
}

/// The font sizes of `theme.txt` (DejaVu Sans).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// "DejaVu Sans Bold 48": the title.
    Bold48,
    /// "DejaVu Sans Regular 20": the subtitle and the entries.
    Regular20,
    /// "DejaVu Sans Bold 20": the selected entry.
    Bold20,
    /// "DejaVu Sans Regular 16": the countdown line and messages.
    Regular16,
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Where a line of text goes: `x` is the pen position, `top` the font's ascender line (the top of `l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Text {
    pub x: u32,
    pub top: u32,
    pub face: Face,
}

/// Where everything is on a `width` x `height` screen. GRUB places the theme's components by percentages of the
/// screen (rounded down) plus pixels; the offsets below come from GRUB 2.06's own render of this theme at 1024x768
/// (the selection bar 12 px inside the menu box and 40 px high every 46 px, text 40 px right of the bar's edge, the
/// countdown bar 28 px high).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub title: Text,
    pub subtitle: Text,
    /// The first entry's selection bar; entry `n` is `ITEM_STEP * n` lower.
    pub item: Rect,
    /// Text of the first entry.
    pub item_text_x: u32,
    pub item_text_dy: u32,
    /// How many entries fit in the menu box.
    pub items_fit: usize,
    pub timeout: Text,
    pub bar: Rect,
}

/// The selected entry's left edge (`select_w.png`).
pub const EDGE_W: u32 = 4;
/// From one entry to the next (`item_height` 40 + `item_spacing` 6).
pub const ITEM_STEP: u32 = 46;

impl Layout {
    pub fn new(width: u32, height: u32) -> Layout {
        let pct = |v: u32, p: u32| (u64::from(v) * u64::from(p) / 100) as u32;
        let left = pct(width, 8);
        let wide = pct(width, 84);
        let title_top = pct(height, 10);
        let menu_top = pct(height, 30);
        let menu_h = pct(height, 50);
        let bar_top = pct(height, 84);
        let item = Rect {
            x: left + 12,
            y: menu_top + 12,
            w: wide.saturating_sub(12 + 14),
            h: 40,
        };
        Layout {
            title: Text {
                x: left,
                top: title_top + 2,
                face: Face::Bold48,
            },
            subtitle: Text {
                x: left,
                top: title_top + 64 + 1,
                face: Face::Regular20,
            },
            item,
            item_text_x: item.x + EDGE_W + 36,
            item_text_dy: 10,
            items_fit: (menu_h.saturating_sub(12) / ITEM_STEP).max(1) as usize,
            timeout: Text {
                x: left,
                top: bar_top.saturating_sub(32) + 1,
                face: Face::Regular16,
            },
            bar: Rect {
                x: left,
                y: bar_top,
                w: wide,
                h: 28,
            },
        }
    }

    /// The selection bar of the `n`th entry on screen.
    pub fn item_rect(&self, n: usize) -> Rect {
        Rect {
            y: self.item.y + ITEM_STEP * n as u32,
            ..self.item
        }
    }
}

/// The first entry on screen when `selected` must be visible and `fit` entries fit (GRUB scrolls the same way).
pub fn first_visible(selected: usize, fit: usize) -> usize {
    (selected + 1).saturating_sub(fit.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real menu, `boot/gbgo/boot.toml`.
    const GBGO: &str = include_str!("../../../boot/gbgo/boot.toml");

    /// The menu with `env` variables already read (as LucinaLoader does: empty values keep the default).
    fn gbgo_with(
        env: &[(&'static str, &'static str)],
        test_toml: Option<&'static str>,
    ) -> Config<'static> {
        let mut c = Config::parse(GBGO).unwrap();
        if let Some(t) = test_toml {
            c.read(t).unwrap();
        }
        for &(k, v) in env {
            if !v.is_empty() {
                c.vars.set(k, v).unwrap();
            }
        }
        c.check().unwrap();
        c
    }

    fn gbgo() -> Config<'static> {
        gbgo_with(&[], None)
    }

    fn ids<'a>(c: &Config<'a>, s: &Shown) -> ([&'a str; MAX_ENTRIES], usize) {
        let mut out = [""; MAX_ENTRIES];
        for (o, i) in out.iter_mut().zip(s.entries()) {
            *o = c.entries()[i].id;
        }
        (out, s.len())
    }

    fn entry<'a>(c: &Config<'a>, id: &str) -> Entry<'a> {
        *c.entries().iter().find(|e| e.id == id).unwrap()
    }

    /// The command line of entry `id` with its `with` group.
    fn cmdline(c: &Config<'_>, id: &str) -> String {
        let e = entry(c, id);
        let mut g = [0u8; 64];
        let group = expand(e.with.unwrap_or(""), &c.vars, &mut g).unwrap();
        let scope = c.vars.scoped(group).unwrap();
        let mut buf = [0u8; CMDLINE_MAX];
        kernel_cmdline(&e, &scope, &mut buf).unwrap().to_string()
    }

    const PENDING: &str = "/lucinaos/install-pending";
    const OVERLAY: &str = "/lucinaos/test-overlay.cpio";
    /// What qemu-install-test.sh writes (its test.cfg for GRUB).
    const TEST_TOML: &str = "[vars]\nqemu_args = \"lucinaos.disk=vdb lucinaos.diagwait=3\"\n\
        kargs = \"console=ttyAMA0 rdinit=/init lucinaos.board=galaxy-book-go\"\ndtb = \"\"\n\
        overlay = \"/lucinaos/test-overlay.cpio\"\n[[when]]\nexists = \"/lucinaos/test.toml\"\ntimeout = 1\n";

    #[test]
    fn gbgo_menus_match_grub_cfg() {
        let c = gbgo();
        assert_eq!(c.title, "Juno");
        let modes: [(u32, u32); 4] = [(1920, 1080), (1366, 768), (1280, 800), (1024, 768)];
        assert!(c.modes().eq(modes.iter().copied()));
        assert_eq!(c.save.as_slice(), ["lucina_entry=menu"]);

        // The installed laptop.
        let s = c.resolve(|_| false);
        let (ids_, n) = ids(&c, &s);
        assert_eq!(&ids_[..n], ["gbgo", "gbgo-safe", "fwsetup"]);
        assert_eq!((s.default, s.timeout), (0, 8));

        // The stick before the diagnosis: variant 1 by default, 5 s.
        let s = c.resolve(|f| f == PENDING);
        let (ids_, n) = ids(&c, &s);
        assert_eq!(
            &ids_[..n],
            ["auto", "rediag", "gbgo", "gbgo-safe", "fwsetup"]
        );
        assert_eq!((s.default, s.timeout), (0, 5));

        // qemu-test.sh: the overlay without install-pending boots the qemu entry after 1 s.
        let s = c.resolve(|f| f == OVERLAY);
        let (ids_, n) = ids(&c, &s);
        assert_eq!(&ids_[..n], ["gbgo", "gbgo-safe", "qemu", "fwsetup"]);
        assert_eq!((s.default, s.timeout), (2, 1));

        // qemu-test.sh --install: both, plus test.toml; the diagnosis still wins, after 1 s.
        let c = gbgo_with(&[], Some(TEST_TOML));
        let s = c.resolve(|f| f == PENDING || f == OVERLAY || f == "/lucinaos/test.toml");
        assert_eq!(c.entries()[s.entry(s.default)].id, "auto");
        assert_eq!(s.timeout, 1);
    }

    #[test]
    fn diagnosis_walks_the_variants_like_grub() {
        let c = gbgo();
        assert_eq!(
            cmdline(&c, "auto"),
            "BOOT_IMAGE=/lucinaos/vmlinuz console=tty0 clk_ignore_unused pd_ignore_unused rdinit=/init \
             lucinaos.board=galaxy-book-go modprobe.blacklist=msm,qcom_q6v5_pas,qcom_q6v5_mss panic=20 \
             lucinaos.diag=v1"
        );
        let c3 = gbgo_with(&[("lucina_step", "v3")], None);
        assert!(cmdline(&c3, "auto").ends_with(
            "qcom_q6v5_mss efi=novamap,noruntime deferred_probe_timeout=30 panic=20 lucinaos.diag=v3"
        ));
        let e = entry(&c3, "auto");
        let scope = c3.vars.scoped("v3").unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(
            expand(e.message.as_slice()[0], &scope, &mut buf).unwrap(),
            "LucinaOS diagnosis, variant 3 of 5: with efi=novamap,noruntime"
        );
        // `save` values are expanded with the variables before any of them is written.
        let mut saved = [("", ""); 2];
        let mut bufs = [[0u8; 64]; 2];
        for ((s, b), item) in saved.iter_mut().zip(bufs.iter_mut()).zip(e.save.as_slice()) {
            let (k, v) = item.split_once('=').unwrap();
            *s = (k, expand(v, &scope, b).unwrap());
        }
        assert_eq!(saved, [("lucina_entry", "v3"), ("lucina_step", "v4")]);
        let c5 = gbgo_with(&[("lucina_step", "v5")], None);
        let scope = c5.vars.scoped("v5").unwrap();
        assert_eq!(scope.get("v_initrd"), Some("/lucinaos/initrd-mini.gz"));
        assert_eq!(scope.get("v_next"), Some("done"));

        // Done, v1 worked: install with it.
        let ok = gbgo_with(&[("lucina_step", "done"), ("lucina_ok", "v1")], None);
        let s = ok.resolve(|f| f == PENDING);
        let (ids_, n) = ids(&ok, &s);
        assert_eq!(
            &ids_[..n],
            ["auto-install", "rediag", "gbgo", "gbgo-safe", "fwsetup"]
        );
        assert_eq!(s.default, 0);
        assert!(cmdline(&ok, "auto-install").ends_with("qcom_q6v5_mss lucinaos.install=internal"));
        // Done, none worked; and an emptied lucina_ok (rediag) is "none".
        let none = gbgo_with(&[("lucina_step", "done"), ("lucina_ok", "")], None);
        let s = none.resolve(|f| f == PENDING);
        assert_eq!(none.entries()[s.entry(0)].id, "auto-none");
        assert_eq!(entry(&none, "auto-none").action, Action::Message);
        assert_eq!(entry(&none, "auto-none").wait, 600);

        // The installed menu boots with the arguments the diagnosis chose.
        let inst = gbgo_with(&[("lucina_args", "earlycon=efifb ignore_loglevel")], None);
        assert!(cmdline(&inst, "gbgo").ends_with("galaxy-book-go earlycon=efifb ignore_loglevel"));
    }

    #[test]
    fn qemu_install_test_uses_qemus_tree_and_overlay() {
        let c = gbgo_with(&[], Some(TEST_TOML));
        assert_eq!(
            cmdline(&c, "auto"),
            "BOOT_IMAGE=/lucinaos/vmlinuz console=ttyAMA0 rdinit=/init lucinaos.board=galaxy-book-go \
             modprobe.blacklist=msm,qcom_q6v5_pas,qcom_q6v5_mss panic=20 lucinaos.diag=v1 lucinaos.disk=vdb \
             lucinaos.diagwait=3"
        );
        let mut buf = [0u8; 64];
        assert_eq!(
            expand(entry(&c, "auto").devicetree.unwrap(), &c.vars, &mut buf).unwrap(),
            ""
        );
        let e = entry(&c, "auto");
        let mut b2 = [0u8; 64];
        assert_eq!(
            expand(e.initrd.as_slice()[1], &c.vars, &mut b2).unwrap(),
            OVERLAY
        );
        // Without test.toml: the Book Go tree, no overlay.
        let real = gbgo();
        let e = entry(&real, "auto");
        assert_eq!(
            expand(e.devicetree.unwrap(), &real.vars, &mut buf).unwrap(),
            "/lucinaos/gbgo.dtb"
        );
        assert_eq!(
            expand(e.initrd.as_slice()[1], &real.vars, &mut b2).unwrap(),
            ""
        );
        assert_eq!(
            cmdline(&real, "qemu"),
            "BOOT_IMAGE=/lucinaos/vmlinuz console=ttyAMA0 rdinit=/init lucinaos.board=galaxy-book-go"
        );
    }

    #[test]
    fn slots_choose_like_grub_cfg() {
        let both = |_: &str| true;
        let home = |h, t| slot_boot(h, t, both);
        // Home; anything but b is a.
        assert_eq!(
            home(Some("a"), None),
            SlotBoot {
                slot: "a",
                other: "b",
                kind: SlotKind::Home
            }
        );
        assert_eq!(
            home(None, None),
            SlotBoot {
                slot: "a",
                other: "b",
                kind: SlotKind::Home
            }
        );
        assert_eq!(home(Some("x"), None).slot, "a");
        assert_eq!(
            home(Some("b"), None),
            SlotBoot {
                slot: "b",
                other: "a",
                kind: SlotKind::Home
            }
        );
        // A trial whose clearing read back.
        assert_eq!(
            home(Some("a"), Some("b")),
            SlotBoot {
                slot: "b",
                other: "a",
                kind: SlotKind::Trial
            }
        );
        // Home without a kernel: the other slot, manual. Same for a trial slot without one.
        let only_b = |s: &str| s == "b";
        assert_eq!(
            slot_boot(Some("a"), None, only_b),
            SlotBoot {
                slot: "b",
                other: "a",
                kind: SlotKind::Manual
            }
        );
        let only_a = |s: &str| s == "a";
        assert_eq!(
            slot_boot(Some("a"), Some("b"), only_a),
            SlotBoot {
                slot: "a",
                other: "b",
                kind: SlotKind::Manual
            }
        );
        assert_eq!(trial_slot(Some("a")), Some("a"));
        assert_eq!(trial_slot(Some("b")), Some("b"));
        assert_eq!(trial_slot(Some("")), None);
        assert_eq!(trial_slot(Some("c")), None);
        assert_eq!(trial_slot(None), None);
    }

    #[test]
    fn slots_in_the_gbgo_menu() {
        let mut c = gbgo();
        assert_eq!(c.slots, Some("/lucinaos"));
        for (k, v) in [
            ("lucina_dir", "/lucinaos/b"),
            ("lucina_other", "a"),
            ("lucina_esp", "1a2b-3c4d"),
            (
                "slot_args",
                "lucinaos.slot=b lucinaos.boot=trial lucinaos.esp=1a2b-3c4d panic=10",
            ),
        ] {
            c.vars.set(k, v).unwrap();
        }
        let s = c.resolve(|_| true);
        let (ids_, n) = ids(&c, &s);
        assert!(ids_[..n].contains(&"gbgo-other"));
        assert_eq!(
            cmdline(&c, "gbgo"),
            "BOOT_IMAGE=/lucinaos/b/vmlinuz console=tty0 clk_ignore_unused pd_ignore_unused rdinit=/init \
             lucinaos.board=galaxy-book-go lucinaos.slot=b lucinaos.boot=trial lucinaos.esp=1a2b-3c4d panic=10"
        );
        assert!(
            cmdline(&c, "gbgo-other").starts_with("BOOT_IMAGE=/lucinaos/a/vmlinuz console=tty0")
        );
        assert!(
            cmdline(&c, "gbgo-other")
                .ends_with("lucinaos.slot=a lucinaos.boot=manual lucinaos.esp=1a2b-3c4d")
        );
        assert!(cmdline(&c, "qemu").ends_with(
            "galaxy-book-go lucinaos.slot=b lucinaos.boot=trial lucinaos.esp=1a2b-3c4d panic=10"
        ));
        let mut b = [0u8; 64];
        assert_eq!(
            expand(entry(&c, "gbgo").devicetree.unwrap(), &c.vars, &mut b).unwrap(),
            "/lucinaos/b/gbgo.dtb"
        );
        // The stick has no slots: the other-slot entry is hidden and paths stay in lucinaos/.
        let stick = gbgo();
        assert!(
            !ids(&stick, &stick.resolve(|_| true))
                .0
                .contains(&"gbgo-other")
        );
        assert_eq!(
            cmdline(&stick, "gbgo").split(' ').next(),
            Some("BOOT_IMAGE=/lucinaos/vmlinuz")
        );
    }

    #[test]
    fn fat_volume_ids_like_grub_probe() {
        let mut s = [0u8; 512];
        let mut out = [0u8; 9];
        s[67..71].copy_from_slice(&0x1a2b_3c4du32.to_le_bytes());
        assert_eq!(fat_volume_id(&s, &mut out), Some("1a2b-3c4d"));
        s[22] = 8; // FAT12/16: the id is at byte 39
        s[39..43].copy_from_slice(&0x0000_f00du32.to_le_bytes());
        assert_eq!(fat_volume_id(&s, &mut out), Some("0000-f00d"));
        assert_eq!(fat_volume_id(&s[..30], &mut out), None);
    }

    #[test]
    fn conditions() {
        let mut v = Vars::new();
        v.set("a", "x").unwrap();
        v.set("e", "").unwrap();
        assert!(
            v.holds("a=x") && v.holds("a!=y") && v.holds("a") && v.holds("!e") && v.holds("!nope")
        );
        assert!(!v.holds("a=y") && !v.holds("a!=x") && !v.holds("e") && !v.holds("!a"));
        assert!(Config::parse("[[entry]]\nif_env = ['=x']").is_err());
    }

    #[test]
    fn grub_environment_block_is_rewritten_in_place() {
        let mut block = [b'#'; ENV_BLOCK_SIZE];
        let text = "# GRUB Environment Block\nlucina_entry=menu\nkeep=a\\\\b\nlucina_step=v2\n";
        block[..text.len()].copy_from_slice(text.as_bytes());
        let s = core::str::from_utf8(&block).unwrap();
        let mut out = [0u8; ENV_BLOCK_SIZE];
        env_update(
            s,
            &[
                ("lucina_step", "v3"),
                ("lucina_entry", "v2"),
                ("lucina_ok", ""),
            ],
            &mut out,
        )
        .unwrap();
        let want =
            "# GRUB Environment Block\nlucina_entry=v2\nkeep=a\\\\b\nlucina_step=v3\nlucina_ok=\n";
        assert_eq!(&out[..want.len()], want.as_bytes());
        assert!(out[want.len()..].iter().all(|&b| b == b'#'));
        // From nothing (a missing or broken block) it starts empty.
        env_update("junk", &[("a", "1")], &mut out).unwrap();
        assert!(out.starts_with(b"# GRUB Environment Block\na=1\n#"));
        assert_eq!(env_update(s, &[("a", "x\\y")], &mut out), Err(TooLong));
        let done = core::str::from_utf8(&block).unwrap();
        assert_eq!(env_get(done, "lucina_step"), Some("v2"));
        assert_eq!(env_get(done, "nope"), None);
        let mut cleared = [0u8; ENV_BLOCK_SIZE];
        env_update(done, &[("lucina_trial", "")], &mut cleared).unwrap();
        assert_eq!(
            env_get(core::str::from_utf8(&cleared).unwrap(), "lucina_trial"),
            Some("")
        );
        let long = "x".repeat(1100);
        assert_eq!(env_update(s, &[("a", &long)], &mut out), Err(TooLong));
    }

    #[test]
    fn variables_inside_variables() {
        let mut v = Vars::new();
        v.set("a", "$b-1").unwrap();
        v.set("b", "${c}2").unwrap();
        v.set("c", "3").unwrap();
        v.set("loop", "$loop").unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(expand("$a", &v, &mut buf).unwrap(), "32-1");
        assert_eq!(expand("[$loop]", &v, &mut buf).unwrap(), "[$loop]");
    }

    #[test]
    fn groups_stay_out_of_plain_lookups() {
        let c = Config::parse("[vars]\nx = '1'\n[vars.g]\nx = '2'\ny = '3'\n[vars.h-1]\nz = '4'\n")
            .unwrap();
        assert_eq!(c.vars.get("x"), Some("1"));
        assert_eq!(c.vars.get("y"), None);
        let g = c.vars.scoped("g").unwrap();
        assert_eq!(
            (g.get("x"), g.get("y"), g.get("z")),
            (Some("2"), Some("3"), None)
        );
        assert_eq!(c.vars.iter().count(), 1);
        assert!(Config::parse("[vars.]\n").is_err());
    }

    #[test]
    fn expand_forms() {
        let mut v = Vars::new();
        v.set("a", "one").unwrap();
        v.set("b_2", "  two   words ").unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(
            expand("x=$a,${b_2}!", &v, &mut buf).unwrap(),
            "x=one, two words !"
        );
        assert_eq!(expand("$ $nope ${a", &v, &mut buf).unwrap(), "$ ${a");
        assert_eq!(expand("a-$a-b", &v, &mut buf).unwrap(), "a-one-b");
        let mut small = [0u8; 4];
        assert_eq!(expand("$a $a", &v, &mut small), Err(TooLong));
    }

    #[test]
    fn errors_name_the_line() {
        let e = |t: &str| Config::parse(t).unwrap_err();
        assert_eq!(e("title = \"x").line, 1);
        assert_eq!(e("\n\nfoo = 1").what, "unknown key");
        assert_eq!(e("[[entry]]\nid = 5").what, "expected a string");
        assert_eq!(e("[table]").line, 1);
        assert_eq!(e("timeout = 5 x").what, "text after the value");
        assert_eq!(e("cmd = \"a\\b\"").line, 1);
        assert_eq!(e("modes = [\"10x\"]").what, "a mode is WIDTHxHEIGHT");
        assert_eq!(e("[[entry]]\ninitrd = [\"a\" \"b\"]").line, 2);
        assert!(
            Config::parse("[[entry]]\nid = 'a'\ntitle = 'A'")
                .unwrap()
                .check()
                .is_err()
        );
        assert!(
            Config::parse(
                "[[entry]]\nid='a'\ntitle='A'\nlinux='/k'\n[[entry]]\nid='a'\ntitle='B'\nlinux='/k'"
            )
            .unwrap()
            .check()
            .is_err()
        );
        let ok =
            Config::parse("[[entry]]\nid='f'\ntitle='F'\naction='firmware-setup'\r\n").unwrap();
        assert!(ok.check().is_ok());
        assert_eq!(ok.entries()[0].action, Action::FirmwareSetup);
    }

    #[test]
    fn unknown_default_is_the_first_shown() {
        let mut c = gbgo();
        c.default = "nope";
        assert_eq!(c.resolve(|_| false).default, 0);
    }

    #[test]
    fn grub_environment_block() {
        let mut block = [b'#'; ENV_BLOCK_SIZE];
        let text = "# GRUB Environment Block\nlucina_slot=b\nbad\\=x\nesc=a\\nb\nlucina_tries=2\n";
        block[..text.len()].copy_from_slice(text.as_bytes());
        let s = core::str::from_utf8(&block).unwrap();
        let mut it = env_vars(s).unwrap();
        assert_eq!(it.next(), Some(("lucina_slot", "b")));
        assert_eq!(it.next(), Some(("lucina_tries", "2")));
        assert_eq!(it.next(), None);
        assert!(env_vars("not an env block").is_none());
    }

    #[test]
    fn initrds_are_padded_to_four_bytes() {
        let mut off = [0u64; 3];
        assert_eq!(initrd_layout(&[5, 8, 3], &mut off), Some(19));
        assert_eq!(off, [0, 8, 16]);
        assert_eq!(initrd_layout(&[], &mut off), Some(0));
    }

    #[test]
    fn keys_and_countdown_follow_grub() {
        let mut m = Menu::new(4, 1, 10);
        assert_eq!(m.seconds_left(), Some(10));
        assert_eq!(m.bar_fill(100), Some(0));
        // 9001 ms left still shows 10 s; 9000 ms shows 9.
        assert_eq!(m.tick(999), Step::Wait);
        assert_eq!(m.seconds_left(), Some(10));
        assert_eq!(m.tick(1), Step::Redraw);
        assert_eq!(m.seconds_left(), Some(9));
        assert_eq!(m.bar_fill(100), Some(10));
        assert_eq!(m.tick(5_000), Step::Redraw);
        assert_eq!(m.bar_fill(1000), Some(600)); // "Starting in 4 s": GRUB's render shows 60 % filled
        assert_eq!(m.tick(4_000), Step::Boot(1));

        let mut m = Menu::new(3, 0, 8);
        assert_eq!(m.key(Key::Up), Step::Redraw); // stops the countdown
        assert_eq!(m.seconds_left(), None);
        assert_eq!(m.tick(60_000), Step::Wait);
        assert_eq!(m.key(Key::Up), Step::Wait); // no wrap at the top
        assert_eq!(m.key(Key::End), Step::Redraw);
        assert_eq!(m.selected, 2);
        assert_eq!(m.key(Key::Down), Step::Wait);
        assert_eq!(m.key(Key::Home), Step::Redraw);
        assert_eq!(m.key(Key::Enter), Step::Boot(0));

        assert!(Menu::new(2, 0, 0).immediate());
        assert_eq!(Menu::new(2, 7, 5).selected, 1);
    }

    #[test]
    fn layout_matches_grubs_render_at_1024x768() {
        let l = Layout::new(1024, 768);
        // Measured on GRUB 2.06's screenshot of this theme (lucinaos `~/scratch/gbgo-brand/render`).
        assert_eq!((l.title.x, l.title.top), (81, 78));
        assert_eq!(l.subtitle.top, 141);
        assert_eq!(
            l.item,
            Rect {
                x: 93,
                y: 242,
                w: 834,
                h: 40
            }
        );
        assert_eq!(l.item_rect(1).y, 288);
        assert_eq!(l.item_text_x, 133);
        assert_eq!(l.item_rect(0).y + l.item_text_dy, 252);
        assert_eq!(l.timeout.top, 614);
        assert_eq!(
            l.bar,
            Rect {
                x: 81,
                y: 645,
                w: 860,
                h: 28
            }
        );
        assert_eq!(l.items_fit, 8);
        assert_eq!(first_visible(2, 8), 0);
        assert_eq!(first_visible(9, 8), 2);
        let book_go = Layout::new(1366, 768);
        assert_eq!((book_go.title.x, book_go.bar.w), (109, 1147));
    }
}
