//! Reading the installed applications out of desktop entries, and running one.
//!
//! Everything here is the freedesktop desktop-entry spec rather than anything
//! COSMIC-specific: `$XDG_DATA_DIRS/applications` is walked once at startup,
//! each `.desktop` file is parsed in a single pass, and the entries that are
//! not meant to be shown are dropped.
//!
//! The scan is deliberately not cached. Reading a few hundred small files takes
//! a couple of milliseconds and is overlapped with the compositor's replies to
//! our first requests (see `main.rs`), whereas a cache would have to be
//! invalidated when a package is installed — and a launcher that does not list
//! the program you just installed is worse than one that is a millisecond
//! slower.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;

/// How deep to walk `applications/`. The spec allows subdirectories, whose
/// names become part of the entry id; nobody nests them far.
const MAX_DEPTH: usize = 4;

/// Terminal emulators tried, in order, when a `Terminal=true` entry has to be
/// run and the config says nothing.
///
/// Each is a command prefix the application's own command is appended to, so
/// the flag that means "run this" is part of the entry. There is no convention
/// to rely on here: cosmic-term has no such flag at all (its whole command line
/// is `--help`, `--version` and `--working-directory`), foot and kitty take the
/// command as trailing arguments, and the rest want `-e`.
const TERMINALS: &[&str] = &[
    "ghostty -e",
    "alacritty -e",
    "foot",
    "kitty",
    "wezterm start --",
    "gnome-terminal --",
    "konsole -e",
    "xterm -e",
];

/// One launchable application.
#[derive(Debug, Clone)]
pub struct App {
    /// Desktop entry id: the file name without `.desktop`, with any
    /// subdirectories joined by `-`, e.g. `firefox` or `kde-konsole`.
    pub id: String,
    pub name: String,
    /// `Comment=`, falling back to `GenericName=`. Empty when the entry has
    /// neither.
    pub description: String,
    /// `Icon=`, which is a theme icon name or an absolute path.
    pub icon: Option<String>,
    /// The raw `Exec=` value, field codes and all. Parsed at launch time by
    /// [`command_line`].
    pub exec: String,
    /// `Terminal=true`: has to be run inside a terminal emulator.
    pub terminal: bool,
    /// `Path=`, the working directory to run in.
    pub path: Option<String>,
    /// Lowercased text the query is matched against, with a weight each.
    pub fields: Vec<Field>,
}

/// One searchable string and how much a match in it counts.
///
/// Weights are percentages applied to the match score, so a good match in a
/// description can never outrank an equally good match in a name.
#[derive(Debug, Clone)]
pub struct Field {
    pub text: String,
    pub weight: i32,
}

impl Field {
    /// A field, or `None` when there is no text to search.
    pub fn new(text: &str, weight: i32) -> Option<Self> {
        let text = text.trim().to_lowercase();
        (!text.is_empty()).then_some(Self { text, weight })
    }
}

/// Reads every application that should be offered, sorted by name.
///
/// Sorting here rather than at query time gives the list a stable base order,
/// which is what decides ties between two equally good matches that have never
/// been launched.
pub fn load(config: &Config) -> Vec<App> {
    let langs = locales();
    let mut apps: Vec<App> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in data_dirs() {
        let mut files = Vec::new();
        scan(&dir.join("applications"), "", &mut files, 0);

        for (id, path) in files {
            // Earlier data dirs win, matching XDG precedence: a copy in
            // ~/.local/share shadows the system one entirely.
            if !seen.insert(id.clone()) {
                continue;
            }

            let Some(entry) = Entry::read(&path) else { continue };
            if !entry.should_show() {
                continue;
            }

            let rule = config.app_rule(&id);
            if rule.is_some_and(|rule| rule.hide) {
                continue;
            }

            let Some(exec) = entry.get("Exec") else { continue };
            let name = rule
                .and_then(|rule| rule.name.clone())
                .or_else(|| entry.localized("Name", &langs).as_deref().map(unescape))
                .unwrap_or_else(|| id.clone());
            let generic = entry.localized("GenericName", &langs).as_deref().map(unescape);
            let description = rule
                .and_then(|rule| rule.description.clone())
                .or_else(|| entry.localized("Comment", &langs).as_deref().map(unescape))
                .or_else(|| generic.clone())
                .unwrap_or_default();
            let icon = rule
                .and_then(|rule| rule.icon.clone())
                .or_else(|| entry.get("Icon").as_deref().map(unescape));

            let keywords = entry
                .localized("Keywords", &langs)
                .map(|raw| split_list(&raw).join(" "))
                .unwrap_or_default();

            // Weights: a name match is what the user almost always means, and
            // a description match is a last resort that should only surface an
            // application when nothing else matches at all.
            let fields = [
                Field::new(&name, 100),
                Field::new(generic.as_deref().unwrap_or(""), 70),
                Field::new(&keywords, 60),
                Field::new(&id, 60),
                Field::new(binary_name(&exec), 50),
                Field::new(&description, 30),
            ]
            .into_iter()
            .flatten()
            .collect();

            apps.push(App {
                id,
                name,
                description,
                icon,
                exec: unescape(&exec),
                terminal: entry.flag("Terminal"),
                path: entry.get("Path").as_deref().map(unescape).filter(|p| !p.is_empty()),
                fields,
            });
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    apps
}

/// Runs an application, detached from this process.
///
/// The child is put in its own process group so that nothing aimed at ours can
/// reach it, and its standard streams are closed: xla is spawned by the
/// compositor and whatever it inherited is not something a browser should be
/// writing to. It is deliberately not waited for — we exit immediately after,
/// and the child is reparented to init.
pub fn launch(app: &App, terminal: Option<&str>) -> Result<(), String> {
    let mut argv =
        command_line(&app.exec).ok_or_else(|| format!("{}: Exec= has no command", app.id))?;

    if app.terminal {
        match terminal {
            Some(prefix) => match command_line(prefix) {
                Some(mut prefix_argv) => {
                    prefix_argv.append(&mut argv);
                    argv = prefix_argv;
                }
                None => return Err(format!("terminal {prefix:?} is not a command")),
            },
            // Running it anyway beats doing nothing: some entries claim they
            // need a terminal but open their own window regardless.
            None => eprintln!(
                "xla: {} wants a terminal and none was found; running it directly",
                app.id
            ),
        }
    }

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Stable since 1.64, and enough on its own here: we are about to exit, so
    // there is no session for the child to be dragged down with.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    if let Some(dir) = &app.path {
        command.current_dir(dir);
    }

    command.spawn().map(|_| ()).map_err(|err| format!("cannot run {:?}: {err}", argv[0]))
}

/// The terminal prefix to use for `Terminal=true` entries.
///
/// `$TERMINAL` is honoured first and used exactly as written, since someone who
/// sets it knows what their emulator wants; otherwise the first entry of
/// [`TERMINALS`] whose program is on `PATH` wins.
pub fn default_terminal() -> Option<String> {
    if let Some(raw) = std::env::var_os("TERMINAL") {
        let raw = raw.to_string_lossy().trim().to_string();
        if !raw.is_empty() && on_path(first_word(&raw)) {
            return Some(raw);
        }
    }
    TERMINALS.iter().find(|candidate| on_path(first_word(candidate))).map(|s| s.to_string())
}

/// Splits an `Exec=` value into a command and its arguments.
///
/// Follows the desktop-entry spec: arguments are whitespace separated, may be
/// double quoted with `\` escaping inside the quotes, and field codes (`%f`,
/// `%U`, ...) are dropped since we never launch an application against a file
/// or URL. `%%` is a literal percent sign.
///
/// Returns `None` when nothing executable is left, which is what an entry that
/// is only field codes amounts to.
pub fn command_line(exec: &str) -> Option<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    // Tracked separately from `current.is_empty()` so that a deliberately
    // empty argument (`""`) survives while a dropped field code does not
    // leave one behind.
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                // Only these four are escapes; anything else keeps its
                // backslash, which is what a Windows-style path in an Exec
                // line needs.
                match chars.peek() {
                    Some(&next @ ('"' | '\\' | '`' | '$')) => {
                        current.push(next);
                        chars.next();
                    }
                    _ => current.push('\\'),
                }
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '%' => match chars.next() {
                Some('%') => {
                    current.push('%');
                    started = true;
                }
                // A field code, dropped. `started` is left alone so a lone
                // `%U` does not become an empty argument.
                Some(_) => {}
                None => {}
            },
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }

    (!args.is_empty() && !args[0].is_empty()).then_some(args)
}

/// One desktop file's `[Desktop Entry]` group, keys in file order.
///
/// Kept as a flat list rather than a map because it is read a handful of times
/// and thrown away, and because the localised variants (`Name[de]`) have to be
/// looked up by prefix.
struct Entry {
    keys: Vec<(String, String)>,
}

impl Entry {
    /// Parses a desktop file, or `None` if it cannot be read.
    ///
    /// Only the `[Desktop Entry]` group is kept: a `[Desktop Action ...]` group
    /// carries its own `Name=` and `Exec=`, and letting those through would
    /// give the wrong name and run the wrong command.
    fn read(path: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        let mut keys = Vec::new();
        let mut in_entry = false;

        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                keys.push((key.trim().to_string(), value.trim().to_string()));
            }
        }

        (!keys.is_empty()).then_some(Self { keys })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.keys
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    }

    /// A boolean key, absent meaning false.
    fn flag(&self, key: &str) -> bool {
        self.get(key).is_some_and(|value| value == "true")
    }

    /// `key` in the best available locale, e.g. `Name[de_DE]` then `Name[de]`
    /// then `Name`.
    fn localized(&self, key: &str, langs: &[String]) -> Option<String> {
        for lang in langs {
            if let Some(value) = self.get(&format!("{key}[{lang}]")) {
                return Some(value);
            }
        }
        self.get(key)
    }

    /// Whether this entry belongs in a launcher at all.
    fn should_show(&self) -> bool {
        if self.get("Type").as_deref() != Some("Application") {
            return false;
        }
        // NoDisplay is for entries that exist to own a MIME association or a
        // D-Bus name; Hidden means "deleted" per the spec.
        if self.flag("NoDisplay") || self.flag("Hidden") {
            return false;
        }
        // An entry whose program is not installed, which is common for
        // packages that ship one desktop file per optional backend.
        if let Some(try_exec) = self.get("TryExec")
            && !on_path(&try_exec)
        {
            return false;
        }

        let current = current_desktops();
        if let Some(only) = self.get("OnlyShowIn")
            && !split_list(&only).iter().any(|want| current.iter().any(|have| have == want))
        {
            return false;
        }
        if let Some(never) = self.get("NotShowIn")
            && split_list(&never).iter().any(|want| current.iter().any(|have| have == want))
        {
            return false;
        }
        true
    }
}

/// Collects `*.desktop` paths under `dir`, with the entry id each one gets.
///
/// The id of a file in a subdirectory carries the subdirectory name, so
/// `applications/kde/konsole.desktop` is `kde-konsole`, which is what the spec
/// says and what other launchers show.
fn scan(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };

        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            scan(&path, &format!("{prefix}{name}-"), out, depth + 1);
        } else if let Some(stem) = name.strip_suffix(".desktop") {
            out.push((format!("{prefix}{stem}"), path));
        }
    }
}

/// Every directory that may hold an `applications/` subdirectory, in XDG
/// precedence order.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }

    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));

    dirs
}

/// The desktop names `OnlyShowIn` and `NotShowIn` are matched against.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The locale suffixes to try, read from the environment.
fn locales() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_default();
    locale_candidates(&raw)
}

/// Locale suffixes to try for a localised key, most specific first.
///
/// `de_DE.UTF-8@euro` gives `["de_DE", "de"]`; the encoding and modifier are
/// dropped because desktop files spell the key `Name[de_DE]`.
fn locale_candidates(raw: &str) -> Vec<String> {
    let base = raw.split(['.', '@']).next().unwrap_or("").trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }

    let mut langs = vec![base.to_string()];
    if let Some((language, _)) = base.split_once('_') {
        langs.push(language.to_string());
    }
    langs
}

/// Splits a `;`-separated list value, honouring `\;`.
fn split_list(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&';') => {
                current.push(';');
                chars.next();
            }
            ';' => {
                if !current.trim().is_empty() {
                    items.push(current.trim().to_string());
                }
                current.clear();
            }
            other => current.push(other),
        }
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_string());
    }
    items
}

/// Resolves the spec's string escapes: `\s`, `\n`, `\t`, `\r`, `\\`.
fn unescape(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            // Not an escape the spec defines; keep both characters so an
            // `Exec=` with a real backslash in it still works.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The program name out of an `Exec=` line, for searching.
///
/// `/usr/bin/gnome-calculator %U` is found by typing "calc", which the
/// application's own name ("Calculator") would also match — but plenty of
/// entries have a name that shares nothing with the command people know.
fn binary_name(exec: &str) -> &str {
    let first = exec.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first)
}

fn first_word(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

/// Whether a program can be run: an absolute path that exists, or a name found
/// on `PATH`.
fn on_path(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_entry(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xla-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.desktop"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn reads_keys_from_the_desktop_entry_group_only() {
        // A Desktop Action carries its own Name= and Exec= that must not win:
        // launching "New Window" when the user picked the application itself
        // would be the wrong command.
        let path = write_entry(
            "groups",
            "[Desktop Entry]\nType=Application\nName=Real\nExec=real %U\n\n\
             [Desktop Action new]\nName=New Window\nExec=wrong\n",
        );
        let entry = Entry::read(&path).unwrap();
        assert_eq!(entry.get("Name").as_deref(), Some("Real"));
        assert_eq!(entry.get("Exec").as_deref(), Some("real %U"));
        assert_eq!(entry.get("Absent"), None);
    }

    #[test]
    fn empty_values_are_treated_as_absent() {
        let path = write_entry("empty", "[Desktop Entry]\nType=Application\nIcon=\n");
        assert_eq!(Entry::read(&path).unwrap().get("Icon"), None);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        assert!(Entry::read(Path::new("/nonexistent/xla.desktop")).is_none());
    }

    #[test]
    fn only_applications_are_shown() {
        let link = write_entry("link", "[Desktop Entry]\nType=Link\nName=Site\nURL=x\n");
        assert!(!Entry::read(&link).unwrap().should_show());

        let hidden = write_entry(
            "hidden",
            "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n",
        );
        assert!(!Entry::read(&hidden).unwrap().should_show());

        let shown = write_entry("shown", "[Desktop Entry]\nType=Application\nName=X\nExec=x\n");
        assert!(Entry::read(&shown).unwrap().should_show());
    }

    #[test]
    fn localized_names_prefer_the_most_specific_match() {
        let path = write_entry(
            "l10n",
            "[Desktop Entry]\nType=Application\nName=Net\nName[de]=Netz\nName[de_DE]=Netzwerk\n",
        );
        let entry = Entry::read(&path).unwrap();
        let langs = |raw: &[&str]| raw.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(entry.localized("Name", &langs(&["de_DE", "de"])).as_deref(), Some("Netzwerk"));
        assert_eq!(entry.localized("Name", &langs(&["de"])).as_deref(), Some("Netz"));
        // No locale, or one the file does not carry, falls back to the plain
        // key rather than to nothing.
        assert_eq!(entry.localized("Name", &[]).as_deref(), Some("Net"));
        assert_eq!(entry.localized("Name", &langs(&["fr"])).as_deref(), Some("Net"));
    }

    #[test]
    fn locale_candidates_drop_encoding_and_modifier() {
        // Only the plain forms appear as desktop file keys.
        assert_eq!(locale_candidates("de_DE.UTF-8@euro"), ["de_DE", "de"]);
        assert_eq!(locale_candidates("pt"), ["pt"]);
        assert!(locale_candidates("C").is_empty(), "the C locale has no localised keys");
        assert!(locale_candidates("POSIX").is_empty());
        assert!(locale_candidates("").is_empty());
    }

    #[test]
    fn exec_is_split_into_a_command_line() {
        assert_eq!(command_line("firefox").unwrap(), ["firefox"]);
        assert_eq!(command_line("  firefox   --new-window ").unwrap(), [
            "firefox",
            "--new-window"
        ]);
        assert_eq!(command_line("/usr/bin/env FOO=1 app").unwrap(), [
            "/usr/bin/env",
            "FOO=1",
            "app"
        ]);
        assert!(command_line("").is_none());
    }

    #[test]
    fn field_codes_are_dropped_without_leaving_empty_arguments() {
        // The bug this guards: `%U` becoming an empty argument, which some
        // programs treat as a file name and refuse to start.
        assert_eq!(command_line("gimp %U").unwrap(), ["gimp"]);
        assert_eq!(command_line("app %f --flag %F").unwrap(), ["app", "--flag"]);
        assert_eq!(command_line("app 100%% sure").unwrap(), ["app", "100%", "sure"]);
        // Only field codes: nothing runnable is left.
        assert!(command_line("%U").is_none());
    }

    #[test]
    fn quoted_arguments_survive_intact() {
        assert_eq!(command_line("\"/opt/My App/run\" --go").unwrap(), [
            "/opt/My App/run",
            "--go"
        ]);
        assert_eq!(command_line(r#"app "say \"hi\"""#).unwrap(), ["app", "say \"hi\""]);
        // A backslash that is not one of the spec's escapes keeps itself.
        assert_eq!(command_line(r#"app "a\b""#).unwrap(), ["app", r"a\b"]);
    }

    #[test]
    fn terminal_prefix_is_prepended_to_the_command() {
        // How a Terminal=true entry is actually run; the prefix carries the
        // flag because there is no convention shared by terminal emulators.
        let mut argv = command_line("alacritty -e").unwrap();
        argv.append(&mut command_line("htop %U").unwrap());
        assert_eq!(argv, ["alacritty", "-e", "htop"]);
    }

    #[test]
    fn list_values_split_on_semicolons() {
        assert_eq!(split_list("web;browser;"), ["web", "browser"]);
        assert_eq!(split_list("GNOME:Unity"), ["GNOME:Unity"], "not a list separator");
        assert_eq!(split_list(r"a\;b;c"), ["a;b", "c"]);
        assert!(split_list(";;").is_empty());
    }

    #[test]
    fn string_escapes_are_resolved() {
        assert_eq!(unescape(r"Hello\sWorld"), "Hello World");
        assert_eq!(unescape(r"a\\b"), r"a\b");
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape(r"C:\Users"), r"C:\Users", "unknown escapes keep both characters");
    }

    #[test]
    fn binary_name_is_the_last_path_component() {
        assert_eq!(binary_name("/usr/bin/gnome-calculator %U"), "gnome-calculator");
        assert_eq!(binary_name("firefox"), "firefox");
        assert_eq!(binary_name(""), "");
    }

    #[test]
    fn on_path_finds_a_real_program() {
        assert!(on_path("sh"), "sh is on PATH in any environment this runs in");
        assert!(on_path("/bin/sh"));
        assert!(!on_path("xla-definitely-not-installed"));
        assert!(!on_path(""));
    }

    #[test]
    fn first_word_is_the_program_of_a_prefix() {
        assert_eq!(first_word("wezterm start --"), "wezterm");
        assert_eq!(first_word("foot"), "foot");
    }
}
