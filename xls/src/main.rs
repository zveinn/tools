mod columns;
mod entry;
mod format;
mod sort;
mod sys;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use columns::Column;
use entry::Entry;
use format::{
    ColorMode, DIM, GREEN, LIGHT_BLUE, ORANGE, RED, RESET, SOFT_BLUE, WHITE, Widths, init_color,
    write_entry, write_entry_cards, write_header,
};
use sort::{entry_order, sort_entries};

enum Cli {
    Help {
        color: ColorMode,
    },
    List {
        paths: Vec<PathBuf>,
        columns: Vec<Column>,
        sort: Option<Column>,
        headers: bool,
        table: bool,
        cards: bool,
        color: ColorMode,
    },
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Cli::Help { color }) => {
            init_color(color);
            print_help();
            ExitCode::SUCCESS
        }
        Ok(Cli::List {
            paths,
            columns,
            sort,
            headers,
            table,
            cards,
            color,
        }) => {
            init_color(color);
            match run(&paths, &columns, sort, headers, table, cards) {
                // Individual operands that failed were already reported.
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                // The reader closed early (`xls x* | head`) — not an error.
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{RED}xls: {e}{RESET}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(msg) => {
            // Best-effort color for errors (auto).
            init_color(ColorMode::Auto);
            eprintln!("{RED}xls: {msg}{RESET}");
            eprintln!("Try '{WHITE}xls --help{RESET}' for more information.");
            ExitCode::FAILURE
        }
    }
}

fn parse_color_mode(s: &str) -> Result<ColorMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(ColorMode::Auto),
        "always" | "on" | "yes" | "true" | "1" => Ok(ColorMode::Always),
        "never" | "off" | "no" | "false" | "0" => Ok(ColorMode::Never),
        other => Err(format!(
            "invalid --color value '{other}' (use auto, always, or never)"
        )),
    }
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut help = false;
    let mut sort = None;
    let mut headers = true;
    let mut table = true;
    let mut cards = false;
    let mut columns = None;
    let mut all = false;
    let mut color = ColorMode::Auto;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => help = true,
            "--noHeaders" | "--no-headers" => headers = false,
            "--noTable" | "--no-table" => table = false,
            "--cards" => cards = true,
            "--all" => all = true,
            "--color" => {
                // GNU ls style: bare `--color` => always; or take next token as mode.
                if let Some(v) = args.get(i + 1) {
                    if let Ok(mode) = parse_color_mode(v) {
                        color = mode;
                        i += 1;
                    } else {
                        color = ColorMode::Always;
                    }
                } else {
                    color = ColorMode::Always;
                }
            }
            s if let Some(v) = s.strip_prefix("--color=") => {
                color = parse_color_mode(v)?;
            }
            "--columns" => {
                i += 1;
                let Some(list) = args.get(i) else {
                    return Err(
                        "--columns requires a comma-separated list (e.g. --columns MTIME,NAME)"
                            .into(),
                    );
                };
                columns = Some(Column::parse_list(list)?);
            }
            s if let Some(list) = s.strip_prefix("--columns=") => {
                if list.is_empty() {
                    return Err(
                        "--columns requires a comma-separated list (e.g. --columns=MTIME,NAME)"
                            .into(),
                    );
                }
                columns = Some(Column::parse_list(list)?);
            }
            "--sort" => {
                i += 1;
                let Some(field) = args.get(i) else {
                    return Err("--sort requires a column name (e.g. --sort MTIME)".into());
                };
                sort = Some(Column::parse(field)?);
            }
            s if let Some(field) = s.strip_prefix("--sort=") => {
                if field.is_empty() {
                    return Err("--sort requires a column name (e.g. --sort=MTIME)".into());
                }
                sort = Some(Column::parse(field)?);
            }
            s if s.starts_with('-') => return Err(format!("unknown flag {s}")),
            // Any number of operands: the shell expands globs like `x*` into
            // one argument per match.
            s => paths.push(PathBuf::from(s)),
        }
        i += 1;
    }

    if help {
        return Ok(Cli::Help { color });
    }

    if all && columns.is_some() {
        return Err("use either --all or --columns, not both".into());
    }

    let columns = if all {
        Column::all()
    } else {
        columns.unwrap_or_else(Column::defaults)
    };

    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }

    Ok(Cli::List {
        paths,
        columns,
        sort,
        headers,
        table,
        cards,
        color,
    })
}

fn print_help() {
    let h = LIGHT_BLUE;
    let k = WHITE;
    let d = DIM;
    let o = ORANGE;
    let fields = Column::names().join(", ");
    let defaults = Column::defaults()
        .iter()
        .map(|c| c.header())
        .collect::<Vec<_>>()
        .join(",");

    println!(
        "\
{h}xls{RESET} — colored directory listing

{h}USAGE{RESET}
  {k}xls{RESET} [{k}--all{RESET}|{k}--columns{RESET} {k}COLS{RESET}] [{k}--sort{RESET} {k}COL{RESET}] [{k}--cards{RESET}] [{k}--color{RESET} {k}WHEN{RESET}] [{k}path{RESET}...]
  {k}xls{RESET} [{k}-h{RESET}|{k}--help{RESET}]

  Multiple paths are listed in sequence: files first, then one labelled
  section per directory. Shell globs work as usual ({k}xls x*{RESET}), since the
  shell expands them into separate paths before {k}xls{RESET} runs.

{h}OPTIONS{RESET}
  {k}--all{RESET}             Show every column in a sensible order
  {k}--columns{RESET} {k}COLS{RESET}   Comma-separated columns to show, in order
  {k}--sort{RESET} {k}COL{RESET}       Sort by column (always ascending)
  {k}--cards{RESET}           Bordered cards instead of a table (grid when space allows)
  {k}--noHeaders{RESET}      Do not print the column header row
  {k}--noTable{RESET}        Skip table frame (no {d}│{RESET} / {d}─┼─{RESET} rules)
  {k}--color{RESET} {k}WHEN{RESET}     When to use colors: {k}auto{RESET} (default), {k}always{RESET}, {k}never{RESET}
  {k}-h{RESET}, {k}--help{RESET}      Show this help and exit

  Color is disabled automatically when stdout is not a terminal (e.g. pipes
  to {k}less{RESET}, files). Also respects {k}NO_COLOR{RESET}, {k}CLICOLOR=0{RESET},
  and {k}CLICOLOR_FORCE{RESET}/{k}FORCE_COLOR{RESET}. Use {k}--color=always{RESET}
  with {k}less -R{RESET} to keep colors in a pager.

{h}COLUMNS{RESET}
  Default ({k}--columns{RESET} / {k}--all{RESET} omitted):
    {k}{defaults}{RESET}

  {k}--all{RESET} order:
    {k}MTIME,N,USER,PERMS,GROUP,OTHER,SIZE,BLOCKS,S,INO:IGEN,DEV,ATIME,CTIME,BIRTH,FLAGS,XATTRS,XFS,NAME{RESET}

  Available:
    {k}{fields}{RESET}

  Examples:
    {k}xls --all{RESET}
    {k}xls --columns NAME,SIZE{RESET}
    {k}xls --columns=MTIME,USER,PERMS,SIZE,NAME{RESET}
    {k}xls --columns MTIME,NAME,XFS --sort SIZE{RESET}

{h}SORTING{RESET}
  Use {k}--sort COL{RESET} or {k}--sort=COL{RESET}. Names are case-insensitive.
  Order is always {o}ascending{RESET} (smallest / oldest / A–Z first).
  Ties break on {k}NAME{RESET} ascending.
  You may sort by a column that is not displayed.

  Notes:
    {k}SIZE{RESET}, {k}N{RESET}, {k}BLOCKS{RESET}, {k}INO:IGEN{RESET}, {k}DEV{RESET}  numeric (low → high)
    {k}MTIME{RESET}, {k}ATIME{RESET}, {k}CTIME{RESET}, {k}BIRTH{RESET}   oldest first
    {k}NAME{RESET}, {k}USER{RESET}, {k}GROUP{RESET}     lexicographic A–Z
    {k}OTHER{RESET}                          by other-class mode bits
    Aliases: {d}NLINK/LINKS{RESET}→N, {d}INODE{RESET}→INO:IGEN, {d}OWNER{RESET}→USER, …

{h}COLORS{RESET}
  {WHITE}white{RESET}        regular file
  {SOFT_BLUE}soft blue{RESET}     directory (same as USER)
  {WHITE}bold white{RESET}    column headers
  {GREEN}green{RESET}        executable
  {o}orange{RESET}       symlink / special file
  {RED}red{RESET}          error or broken symlink

{h}COLUMN REFERENCE{RESET}
  {k}MTIME{RESET}     Last content modification time (UTC, DD-MM-YYYY HH:MM:SS)
  {k}USER{RESET}      Owner identity: {d}sveinn{RESET}, or {d}sveinn/staff{RESET}
                    when group name differs
  {k}PERMS{RESET}     Triads + type: {d}[rwx][r-x][r-x] dir{RESET}
                    (user · group · other · type; {d}+{RESET}/{d}@{RESET} ACL/xattr)
  {k}GROUP{RESET}     Group name only (optional detail column)
  {k}OTHER{RESET}     Other triad only, e.g. {d}[r-x]{RESET} (optional)
  {k}SIZE{RESET}      Logical size (human-readable: B/K/M/G/T)
  {k}NAME{RESET}      Entry name (color indicates type); symlinks show {d}→{RESET} target
  {k}N{RESET}         Hard link count
  {k}BLOCKS{RESET}    Allocated blocks and I/O block size ({d}<st_blocks>b/<blksize>{RESET})
  {k}S{RESET}         Sparse: {ORANGE}◆{RESET} sparse, {d}◇{RESET} not
  {k}INO:IGEN{RESET}  Inode number and generation (when available)
  {k}DEV{RESET}       Device id ({d}major:minor{RESET}); devices also show {d}rdev{RESET}
  {k}ATIME{RESET}     Last access time (may be stale on noatime mounts)
  {k}CTIME{RESET}     Last status-change time (metadata change, not create)
  {k}BIRTH{RESET}     Creation / birth time when the filesystem provides it
  {k}FLAGS{RESET}     Linux inode flags from {d}FS_IOC_GETFLAGS{RESET}, or {d}-{RESET}
  {k}XATTRS{RESET}    Extended attribute names, comma-separated, or {d}-{RESET}
  {k}XFS{RESET}       Cheap XFS info ({d}FS_IOC_FSGETXATTR{RESET} / {d}DIOINFO{RESET}):
                    xflags, exts, proj, esz, cow, dio — or {d}-{RESET} if unavailable

{h}EXAMPLES{RESET}
  {k}xls{RESET}
  {k}xls /var/log{RESET}
  {k}xls src/*.rs{RESET}
  {k}xls x* --sort MTIME{RESET}
  {k}xls --all{RESET}
  {k}xls --cards{RESET}
  {k}xls --all --cards{RESET}
  {k}xls --columns NAME,SIZE,MTIME{RESET}
  {k}xls --columns NAME,XFS --sort XFS{RESET}
  {k}xls --sort SIZE --noHeaders{RESET}
"
    );
}

/// A command-line operand that turned out to be a directory. `entry` is only
/// used to order the sections; the listing itself comes from reading `path`.
struct DirOperand {
    path: PathBuf,
    entry: Entry,
}

/// Returns `Ok(false)` when some operand failed but the rest were listed;
/// `Err` only for a failure to write the listing itself.
fn run(
    paths: &[PathBuf],
    columns: &[Column],
    sort: Option<Column>,
    headers: bool,
    table: bool,
    cards: bool,
) -> io::Result<bool> {
    // Default sort is name ascending, which is just the NAME key.
    let key = sort.unwrap_or(Column::Name);
    let detail = Column::max_detail(columns).max(key.min_detail());

    let mut ok = true;

    // Stat every operand first so we can split them the way ls does: all
    // non-directories in one listing, then a section per directory.
    let mut files: Vec<Entry> = Vec::new();
    let mut dirs: Vec<DirOperand> = Vec::new();
    for path in paths {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(err) => {
                eprintln!("{RED}xls: {}: {err}{RESET}", path.display());
                ok = false;
                continue;
            }
        };
        // Operands keep the name as typed, so `xls */*.md` stays unambiguous.
        let entry = match Entry::collect(path.clone(), operand_name(path), detail) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("{RED}xls: {}: {err}{RESET}", path.display());
                ok = false;
                continue;
            }
        };
        // Symlinks to directories show as a single row, not as their contents.
        if meta.is_dir() {
            dirs.push(DirOperand {
                path: path.clone(),
                entry,
            });
        } else {
            files.push(entry);
        }
    }

    if files.is_empty() && dirs.is_empty() {
        return Ok(ok);
    }

    sort_entries(&mut files, key);
    dirs.sort_by(|a, b| entry_order(&a.entry, &b.entry, key));

    // ls labels each section once more than one operand is on the command line.
    let labels = paths.len() > 1;

    let mut out = io::stdout().lock();
    // Blank line so the listing separates cleanly from the shell prompt.
    writeln!(out)?;

    let mut first = true;
    if !files.is_empty() {
        write_listing(&mut out, &files, columns, headers, table, cards)?;
        first = false;
    }

    for dir in &dirs {
        // Card rows already end in a blank line, so they need no separator.
        if !first && !cards {
            writeln!(out)?;
        }
        first = false;
        if labels {
            writeln!(out, "{SOFT_BLUE}{}:{RESET}", dir.entry.name)?;
        }
        match read_dir_entries(&dir.path, detail) {
            Ok((mut entries, entries_ok)) => {
                ok &= entries_ok;
                sort_entries(&mut entries, key);
                write_listing(&mut out, &entries, columns, headers, table, cards)?;
            }
            Err(err) => {
                out.flush()?;
                eprintln!("{RED}xls: {}: {err}{RESET}", dir.path.display());
                ok = false;
            }
        }
    }

    out.flush()?;
    Ok(ok)
}

/// Name shown for an operand: the string the user typed, minus any trailing
/// slash. Entries *inside* a directory keep using their bare file name.
fn operand_name(path: &Path) -> String {
    let s = path.to_string_lossy();
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() {
        s.into_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Collect one directory's entries. Unreadable entries are reported and
/// skipped; the `bool` is false when that happened.
fn read_dir_entries(path: &Path, detail: u8) -> io::Result<(Vec<Entry>, bool)> {
    let mut ok = true;
    let mut entries = Vec::new();
    for ent in fs::read_dir(path)? {
        let ent = match ent {
            Ok(e) => e,
            Err(err) => {
                eprintln!("{RED}xls: {}: {err}{RESET}", path.display());
                ok = false;
                continue;
            }
        };
        let name = ent.file_name().to_string_lossy().into_owned();
        match Entry::collect(ent.path(), name, detail) {
            Ok(e) => entries.push(e),
            Err(err) => {
                eprintln!("{RED}xls: {}: {err}{RESET}", ent.path().display());
                ok = false;
            }
        }
    }
    Ok((entries, ok))
}

fn write_listing(
    out: &mut impl Write,
    entries: &[Entry],
    columns: &[Column],
    headers: bool,
    table: bool,
    cards: bool,
) -> io::Result<()> {
    if cards {
        return write_entry_cards(out, entries, columns, headers);
    }
    let widths = Widths::measure(entries, columns);
    if headers {
        write_header(out, columns, &widths, table)?;
    }
    for e in entries {
        write_entry(out, e, columns, &widths, table)?;
    }
    Ok(())
}
