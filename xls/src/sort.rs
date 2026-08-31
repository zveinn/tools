//! Ascending sort by column.

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::columns::Column;
use crate::entry::Entry;

/// Sort `entries` by `key` in **ascending** order.
/// Ties break on name ascending (case-insensitive).
pub fn sort_entries(entries: &mut [Entry], key: Column) {
    entries.sort_by(|a, b| entry_order(a, b, key));
}

/// Ascending order for two entries by `key`, ties broken on name.
/// Also used to order the per-directory sections of a multi-operand listing.
pub fn entry_order(a: &Entry, b: &Entry, key: Column) -> Ordering {
    cmp_asc(a, b, key).then_with(|| cmp_str_ci(&a.name, &b.name))
}

fn cmp_asc(a: &Entry, b: &Entry, key: Column) -> Ordering {
    match key {
        Column::Perms => a.mode.cmp(&b.mode),
        Column::Size => a.size.cmp(&b.size),
        Column::Mtime => cmp_time(a.mtime, b.mtime),
        Column::Name => a
            .name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase()),
        Column::Nlink => a.nlink.cmp(&b.nlink),
        Column::User => cmp_str_ci(&a.user, &b.user).then_with(|| cmp_str_ci(&a.group, &b.group)),
        Column::Group => cmp_str_ci(&a.group, &b.group),
        Column::Other => (a.mode & 0o1007).cmp(&(b.mode & 0o1007)), // other + sticky
        Column::Blocks => a
            .blocks
            .cmp(&b.blocks)
            .then_with(|| a.blksize.cmp(&b.blksize)),
        Column::Sparse => a.sparse.cmp(&b.sparse),
        Column::Ino => a
            .ino
            .cmp(&b.ino)
            .then_with(|| cmp_opt_u32(a.extras.inode_gen, b.extras.inode_gen)),
        Column::Dev => a
            .dev_major
            .cmp(&b.dev_major)
            .then_with(|| a.dev_minor.cmp(&b.dev_minor))
            .then_with(|| a.rdev_major.cmp(&b.rdev_major))
            .then_with(|| a.rdev_minor.cmp(&b.rdev_minor)),
        Column::Atime => cmp_time(a.atime, b.atime),
        Column::Ctime => a.ctime_secs.cmp(&b.ctime_secs),
        Column::Birth => cmp_time(a.birth, b.birth),
        Column::Flags => cmp_str_ci(&a.extras.flags.join(","), &b.extras.flags.join(",")),
        Column::Xattrs => a
            .extras
            .xattrs
            .len()
            .cmp(&b.extras.xattrs.len())
            .then_with(|| cmp_str_ci(&a.extras.xattrs.join(","), &b.extras.xattrs.join(","))),
        Column::Xfs => cmp_xfs(a, b),
    }
}

fn cmp_str_ci(a: &str, b: &str) -> Ordering {
    a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
}

fn cmp_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

fn cmp_opt_u32(a: Option<u32>, b: Option<u32>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

fn cmp_xfs(a: &Entry, b: &Entry) -> Ordering {
    match (a.xfs(), b.xfs()) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(ax), Some(bx)) => ax
            .nextents
            .cmp(&bx.nextents)
            .then_with(|| ax.projid.cmp(&bx.projid))
            .then_with(|| ax.extsize.cmp(&bx.extsize))
            .then_with(|| ax.cowextsize.cmp(&bx.cowextsize))
            .then_with(|| ax.xflags.join(",").cmp(&bx.xflags.join(","))),
    }
}
